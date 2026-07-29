use http_body_util::BodyExt;
use hyper::{Method, Request, StatusCode};
use hyper_util::rt::TokioIo;
use screw_core::request::Request as ScrewRequest;
use screw_core::responder_factory::ResponderFactory;
use screw_core::response::Response;
use screw_core::routing::route::first::Route;
use screw_core::routing::router::{self, RoutedRequest};
use screw_core::server::ServerService;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::{TcpListener, TcpStream};

struct Extensions {
    greeting: String,
}

type Rq = RoutedRequest<ScrewRequest<Extensions>>;

fn text(status: StatusCode, body: impl Into<String>) -> Response {
    Response {
        http: hyper::Response::builder()
            .status(status)
            .body(screw_core::body::full(body.into()))
            .unwrap(),
    }
}

async fn greet(request: Rq) -> Response {
    let name = request.path.get("name").unwrap_or("stranger").to_owned();
    let greeting = &request.origin.extensions.greeting;
    text(StatusCode::OK, format!("{greeting}, {name}!"))
}

async fn echo_raw_id(request: Rq) -> Response {
    let id = request.path.get("id").unwrap_or_default().to_owned();
    text(StatusCode::OK, id)
}

async fn echo_query(request: Rq) -> Response {
    let mut pairs = request
        .query
        .iter()
        .map(|(key, value)| format!("{key}={value}"))
        .collect::<Vec<_>>();
    pairs.sort();
    text(StatusCode::OK, pairs.join("&"))
}

async fn report_remote_addr(request: Rq) -> Response {
    let remote_addr = request.origin.remote_addr;
    text(StatusCode::OK, remote_addr.to_string())
}

async fn boom(_: Rq) -> Response {
    panic!("handler blew up");
}

async fn fallback(request: Rq) -> Response {
    if request.allowed_methods.is_empty() {
        return text(StatusCode::NOT_FOUND, "not found");
    }

    let allow = request
        .allowed_methods
        .iter()
        .map(|method| method.as_str())
        .collect::<Vec<_>>()
        .join(", ");

    Response {
        http: hyper::Response::builder()
            .status(StatusCode::METHOD_NOT_ALLOWED)
            .header(hyper::header::ALLOW, allow)
            .body(screw_core::body::empty())
            .unwrap(),
    }
}

async fn start_server() -> SocketAddr {
    let router = router::first::Router::with_fallback_handler(fallback).and_routes(|r| {
        r.route(
            Route::with_method(&Method::GET)
                .and_path("/greet/{name}")
                .and_handler(greet),
        )
        .route(
            Route::with_methods(vec![&Method::PATCH, &Method::DELETE])
                .and_path("/greet/{name}")
                .and_handler(greet),
        )
        .route(
            Route::with_method(&Method::GET)
                .and_path("/raw/{id}")
                .and_handler(echo_raw_id),
        )
        .route(
            Route::with_method(&Method::GET)
                .and_path("/query")
                .and_handler(echo_query),
        )
        .route(
            Route::with_method(&Method::GET)
                .and_path("/whoami")
                .and_handler(report_remote_addr),
        )
        .route(
            Route::with_method(&Method::GET)
                .and_path("/boom")
                .and_handler(boom),
        )
    });

    let responder_factory = ResponderFactory::with_router(router).and_extensions(Extensions {
        greeting: "Hello".to_owned(),
    });
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

struct Client {
    sender: hyper::client::conn::http1::SendRequest<screw_core::body::ResponseBody>,
}

impl Client {
    async fn connect(addr: SocketAddr) -> Self {
        let stream = TcpStream::connect(addr).await.unwrap();
        let (sender, connection) = hyper::client::conn::http1::handshake(TokioIo::new(stream))
            .await
            .unwrap();
        tokio::spawn(connection);
        Self { sender }
    }

    async fn get(&mut self, path: &str) -> (StatusCode, hyper::HeaderMap, String) {
        self.request(Method::GET, path).await
    }

    async fn request(
        &mut self,
        method: Method,
        path: &str,
    ) -> (StatusCode, hyper::HeaderMap, String) {
        let request = Request::builder()
            .method(method)
            .uri(path)
            .header(hyper::header::HOST, "localhost")
            .body(screw_core::body::empty())
            .unwrap();

        let response = self.sender.send_request(request).await.unwrap();
        let status = response.status();
        let headers = response.headers().clone();
        let body = response.into_body().collect().await.unwrap().to_bytes();

        (status, headers, String::from_utf8_lossy(&body).into_owned())
    }
}

#[tokio::test]
async fn serves_a_matched_route_with_path_params_and_extensions() {
    let addr = start_server().await;
    let mut client = Client::connect(addr).await;

    let (status, _, body) = client.get("/greet/world").await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, "Hello, world!");
}

#[tokio::test]
async fn parses_the_query_string() {
    let addr = start_server().await;
    let mut client = Client::connect(addr).await;

    let (status, _, body) = client.get("/query?b=2&a=1").await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, "a=1&b=2");
}

#[tokio::test]
async fn exposes_the_remote_address_to_handlers() {
    let addr = start_server().await;
    let mut client = Client::connect(addr).await;

    let (status, _, body) = client.get("/whoami").await;

    assert_eq!(status, StatusCode::OK);
    assert!(
        body.starts_with("127.0.0.1:"),
        "expected a loopback address, got {body:?}"
    );
}

#[tokio::test]
async fn unknown_path_is_a_404() {
    let addr = start_server().await;
    let mut client = Client::connect(addr).await;

    let (status, _, body) = client.get("/nothing/here").await;

    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body, "not found");
}

#[tokio::test]
async fn method_mismatch_is_a_405_with_an_allow_header() {
    let addr = start_server().await;
    let mut client = Client::connect(addr).await;

    let (status, headers, _) = client.request(Method::POST, "/greet/world").await;

    assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED);
    assert_eq!(
        headers.get(hyper::header::ALLOW).unwrap(),
        "GET, PATCH, DELETE"
    );
}

#[tokio::test]
async fn an_encoded_slash_stays_inside_one_path_segment() {
    let addr = start_server().await;
    let mut client = Client::connect(addr).await;

    let (status, _, body) = client.get("/raw/a%2Fb").await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, "a%2Fb");
}

#[tokio::test]
async fn a_panicking_handler_answers_500_and_keeps_the_connection_usable() {
    let addr = start_server().await;
    let mut client = Client::connect(addr).await;

    let (status, _, _) = client.get("/boom").await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);

    let (status, _, body) = client.get("/greet/world").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, "Hello, world!");
}
