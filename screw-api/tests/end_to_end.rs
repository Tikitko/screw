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
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::{TcpListener, TcpStream};

const MAX_BODY_SIZE: usize = 64;

struct Extensions;

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

impl ApiRequestContent<Extensions> for EchoContent {
    type Data = EchoData;
    fn create(origin: ApiRequestOriginContent<Self::Data, Extensions>) -> Self {
        Self {
            data: origin.data_result,
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
        })
    });

    let responder_factory = ResponderFactory::with_router(router).and_extensions(Extensions);
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
    let stream = TcpStream::connect(addr).await.unwrap();
    let (mut sender, connection) = hyper::client::conn::http1::handshake(TokioIo::new(stream))
        .await
        .unwrap();
    tokio::spawn(connection);

    let mut builder = Request::builder()
        .method(Method::POST)
        .uri("/api/echo")
        .header(hyper::header::HOST, "localhost");
    if let Some(content_type) = content_type {
        builder = builder.header(hyper::header::CONTENT_TYPE, content_type);
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
