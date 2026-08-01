use http_body_util::BodyExt;
use hyper::{Method, Request, StatusCode};
use hyper_util::rt::TokioIo;
use screw_api::json::JsonApiMiddlewareConverter;
use screw_api::request::{ApiRequest, ApiRequestContent, ApiRequestOriginContent};
use screw_api::response::{
    ApiResponse, ApiResponseContentBase, ApiResponseContentFailure, ApiResponseContentSuccess,
};
use screw_components::dyn_result::DResult;
use screw_core::request::Request as ScrewRequest;
use screw_core::responder_factory::ResponderFactory;
use screw_core::response::Response;
use screw_core::routing::route::first::Route;
use screw_core::routing::router::{self, RoutedRequest};
use screw_core::server::ServerService;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::{TcpListener, TcpStream};

const MAX_BODY_SIZE: usize = 64;

#[derive(Default)]
struct Extensions {
    tokens: tokio::sync::Mutex<HashSet<String>>,
}

impl Extensions {
    /// Awaited from `create`, so the lookup happens before the handler is
    /// reached rather than inside it.
    async fn is_known(&self, token: &str) -> bool {
        self.tokens.lock().await.contains(token)
    }
}

#[derive(Deserialize)]
struct EchoData {
    message: String,
}

#[derive(Serialize)]
struct Echoed {
    message: String,
}

struct EchoContent {
    data: DResult<EchoData>,
}

impl<Failure> ApiRequestContent<Extensions, Failure> for EchoContent
where
    Failure: ApiResponseContentFailure,
{
    type Data = EchoData;
    async fn create(
        origin: ApiRequestOriginContent<Self::Data, Extensions>,
    ) -> Result<Self, Failure> {
        Ok(Self {
            data: origin.data_result,
        })
    }
}

/// Refuses to be built unless the request carries an `X-Echo` token the
/// extensions know about, which it has to await to find out.
struct GuardedContent;

impl ApiRequestContent<Extensions, EchoFailure> for GuardedContent {
    type Data = ();
    async fn create(
        origin: ApiRequestOriginContent<Self::Data, Extensions>,
    ) -> Result<Self, EchoFailure> {
        let token = origin
            .http_parts
            .headers
            .get("x-echo")
            .and_then(|value| value.to_str().ok())
            .ok_or_else(|| EchoFailure("missing X-Echo".to_owned()))?;

        if origin.extensions.is_known(token).await {
            Ok(Self)
        } else {
            Err(EchoFailure(format!("unknown token {token}")))
        }
    }
}

struct EchoSuccess(Echoed);

impl ApiResponseContentBase for EchoSuccess {
    fn status_code(&self) -> StatusCode {
        StatusCode::OK
    }
}

impl ApiResponseContentSuccess for EchoSuccess {
    type Data = Echoed;
    fn identifier(&self) -> &'static str {
        "ECHOED"
    }
    fn description(&self) -> Option<String> {
        None
    }
    fn data(&self) -> &Self::Data {
        &self.0
    }
}

struct EchoFailure(String);

impl ApiResponseContentBase for EchoFailure {
    fn status_code(&self) -> StatusCode {
        StatusCode::BAD_REQUEST
    }
}

impl ApiResponseContentFailure for EchoFailure {
    fn identifier(&self) -> &'static str {
        "BAD_BODY"
    }
    fn reason(&self) -> Option<String> {
        Some(self.0.clone())
    }
}

async fn echo(
    request: ApiRequest<EchoContent, Extensions>,
) -> ApiResponse<EchoSuccess, EchoFailure> {
    match request.content.data {
        Ok(data) => ApiResponse::success(EchoSuccess(Echoed {
            message: data.message,
        })),
        Err(error) => ApiResponse::failure(EchoFailure(error.to_string())),
    }
}

/// The same endpoint written without naming either wrapper: the request
/// arrives as the content alone, through `From<ApiRequest<_, _>> for (Content,)`,
/// and the answer goes back as a `Result`, through
/// `From<Result<_, _>> for ApiResponse<_, _>`.
async fn echo_unpacked((content,): (EchoContent,)) -> Result<EchoSuccess, EchoFailure> {
    match content.data {
        Ok(data) => Ok(EchoSuccess(Echoed {
            message: data.message,
        })),
        Err(error) => Err(EchoFailure(error.to_string())),
    }
}

async fn guarded(
    _: ApiRequest<GuardedContent, Extensions>,
) -> ApiResponse<EchoSuccess, EchoFailure> {
    ApiResponse::success(EchoSuccess(Echoed {
        message: "guarded".to_owned(),
    }))
}

async fn fallback(_: RoutedRequest<ScrewRequest<Extensions>>) -> Response {
    Response {
        http: hyper::Response::builder()
            .status(StatusCode::NOT_FOUND)
            .body(screw_core::body::empty())
            .unwrap(),
    }
}

async fn start_server() -> SocketAddr {
    let converter = JsonApiMiddlewareConverter {
        pretty_printed: false,
        max_body_size: MAX_BODY_SIZE,
    };

    let router = router::first::Router::with_fallback_handler(fallback).and_routes(|r| {
        r.scoped_middleware("/api", converter, |r| {
            r.route(
                Route::with_method(&Method::POST)
                    .and_path("/echo")
                    .and_handler(echo),
            )
            .route(
                Route::with_method(&Method::POST)
                    .and_path("/echo-unpacked")
                    .and_handler(echo_unpacked),
            )
            .route(
                Route::with_method(&Method::POST)
                    .and_path("/guarded")
                    .and_handler(guarded),
            )
        })
    });

    let extensions = Extensions::default();
    extensions.tokens.lock().await.insert("yes".to_owned());

    let responder_factory = ResponderFactory::with_router(router).and_extensions(extensions);
    let server_service = Arc::new(ServerService::with_responder_factory(responder_factory));

    let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
        .await
        .unwrap();
    let addr = listener.local_addr().unwrap();

    tokio::spawn(async move {
        loop {
            let (stream, remote_addr) = listener.accept().await.unwrap();
            let session_service = server_service.make_session_service(remote_addr);
            tokio::spawn(async move {
                let _ = hyper::server::conn::http1::Builder::new()
                    .serve_connection(TokioIo::new(stream), session_service)
                    .await;
            });
        }
    });

    addr
}

async fn post(
    addr: SocketAddr,
    content_type: Option<&str>,
    body: &str,
) -> (StatusCode, hyper::HeaderMap, serde_json::Value) {
    post_to(addr, "/api/echo", content_type, body).await
}

async fn post_to(
    addr: SocketAddr,
    path: &str,
    content_type: Option<&str>,
    body: &str,
) -> (StatusCode, hyper::HeaderMap, serde_json::Value) {
    post_with_headers(addr, path, content_type, &[], body).await
}

async fn post_with_headers(
    addr: SocketAddr,
    path: &str,
    content_type: Option<&str>,
    extra_headers: &[(&str, &str)],
    body: &str,
) -> (StatusCode, hyper::HeaderMap, serde_json::Value) {
    let stream = TcpStream::connect(addr).await.unwrap();
    let (mut sender, connection) = hyper::client::conn::http1::handshake(TokioIo::new(stream))
        .await
        .unwrap();
    tokio::spawn(connection);

    let mut builder = Request::builder()
        .method(Method::POST)
        .uri(path)
        .header(hyper::header::HOST, "localhost");
    if let Some(content_type) = content_type {
        builder = builder.header(hyper::header::CONTENT_TYPE, content_type);
    }
    for (name, value) in extra_headers {
        builder = builder.header(*name, *value);
    }
    let request = builder
        .body(screw_core::body::full(body.to_owned()))
        .unwrap();

    let response = sender.send_request(request).await.unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let json = serde_json::from_slice(&bytes).expect("response body should be JSON");

    (status, headers, json)
}

#[tokio::test]
async fn a_valid_request_round_trips_through_the_json_layer() {
    let addr = start_server().await;

    let (status, headers, json) =
        post(addr, Some("application/json"), r#"{"message":"hi there"}"#).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        headers.get(hyper::header::CONTENT_TYPE).unwrap(),
        "application/json"
    );
    assert_eq!(json["success"]["identifier"], "ECHOED");
    assert_eq!(json["success"]["data"]["message"], "hi there");
}

#[tokio::test]
async fn a_handler_may_take_the_content_alone_and_return_a_result() {
    let addr = start_server().await;

    let (status, headers, json) = post_to(
        addr,
        "/api/echo-unpacked",
        Some("application/json"),
        r#"{"message":"hi there"}"#,
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        headers.get(hyper::header::CONTENT_TYPE).unwrap(),
        "application/json"
    );
    assert_eq!(json["success"]["identifier"], "ECHOED");
    assert_eq!(json["success"]["data"]["message"], "hi there");
}

#[tokio::test]
async fn an_err_from_such_a_handler_becomes_a_failure_response() {
    let addr = start_server().await;

    let (status, _, json) = post_to(addr, "/api/echo-unpacked", None, r#"{"message":"hi"}"#).await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(json["failure"]["identifier"], "BAD_BODY");
}

#[tokio::test]
async fn a_content_that_refuses_to_be_built_answers_as_a_failure() {
    let addr = start_server().await;

    let (status, headers, json) =
        post_with_headers(addr, "/api/guarded", Some("application/json"), &[], "{}").await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(
        headers.get(hyper::header::CONTENT_TYPE).unwrap(),
        "application/json"
    );
    assert_eq!(json["failure"]["identifier"], "BAD_BODY");
    assert_eq!(json["failure"]["reason"], "missing X-Echo");
}

#[tokio::test]
async fn the_handler_runs_once_the_content_is_built() {
    let addr = start_server().await;

    let (status, _, json) = post_with_headers(
        addr,
        "/api/guarded",
        Some("application/json"),
        &[("x-echo", "yes")],
        "{}",
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["success"]["data"]["message"], "guarded");
}

#[tokio::test]
async fn a_content_may_await_before_deciding_to_refuse() {
    let addr = start_server().await;

    let (status, _, json) = post_with_headers(
        addr,
        "/api/guarded",
        Some("application/json"),
        &[("x-echo", "no-such-token")],
        "{}",
    )
    .await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(json["failure"]["identifier"], "BAD_BODY");
    assert_eq!(json["failure"]["reason"], "unknown token no-such-token");
}

#[tokio::test]
async fn a_content_type_parameter_is_ignored() {
    let addr = start_server().await;

    let (status, _, json) = post(
        addr,
        Some("application/json; charset=utf-8"),
        r#"{"message":"hi"}"#,
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["success"]["data"]["message"], "hi");
}

#[tokio::test]
async fn a_missing_content_type_reaches_the_handler_as_a_failure() {
    let addr = start_server().await;

    let (status, _, json) = post(addr, None, r#"{"message":"hi"}"#).await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(json["failure"]["identifier"], "BAD_BODY");
}

#[tokio::test]
async fn a_wrong_content_type_reaches_the_handler_as_a_failure() {
    let addr = start_server().await;

    let (status, _, json) = post(addr, Some("text/plain"), r#"{"message":"hi"}"#).await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(json["failure"]["identifier"], "BAD_BODY");
}

#[tokio::test]
async fn a_malformed_body_reaches_the_handler_as_a_failure() {
    let addr = start_server().await;

    let (status, _, json) = post(addr, Some("application/json"), "{not json").await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(json["failure"]["identifier"], "BAD_BODY");
}

#[tokio::test]
async fn a_body_over_the_limit_is_rejected_rather_than_buffered() {
    let addr = start_server().await;

    let oversized = format!(r#"{{"message":"{}"}}"#, "x".repeat(MAX_BODY_SIZE * 4));
    assert!(oversized.len() > MAX_BODY_SIZE);

    let (status, _, json) = post(addr, Some("application/json"), &oversized).await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(json["failure"]["identifier"], "BAD_BODY");
}
