//! The smallest useful `screw` server: no API layer, just the router.
//!
//! Handlers here take the raw `RoutedRequest` and build a `hyper::Response`
//! themselves, which is the level you work at when you want full control over
//! the wire format.
//!
//! ```sh
//! cargo run -p screw-core --example hello
//!
//! curl -i localhost:8080/hello/world
//! curl -i 'localhost:8080/hello/world?shout=yes'
//! curl -i -X POST localhost:8080/hello/world   # 405, with an Allow header
//! curl -i localhost:8080/nothing-here          # 404
//! ```

use hyper::{Method, StatusCode};
use hyper_util::rt::TokioIo;
use screw_components::dyn_fn::DFnOnce;
use screw_core::request::Request;
use screw_core::responder_factory::ResponderFactory;
use screw_core::response::Response;
use screw_core::routing::route::first::Route;
use screw_core::routing::router::{self, RoutedRequest};
use screw_core::server::ServerService;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::TcpListener;

/// Shared application state. Every handler sees it as `Arc<Extensions>`.
struct Extensions {
    greeting: String,
}

type Rq = RoutedRequest<Request<Extensions>>;

fn text(status: StatusCode, body: impl Into<String>) -> Response {
    Response {
        http: hyper::Response::builder()
            .status(status)
            .header(hyper::header::CONTENT_TYPE, "text/plain; charset=utf-8")
            .body(screw_core::body::full(body.into()))
            .unwrap(),
    }
}

async fn hello(request: Rq) -> Response {
    let name = request.path.get("name").unwrap_or("stranger");
    let greeting = &request.origin.extensions.greeting;

    let message = format!("{greeting}, {name}!");
    let message = if request.query.get("shout").is_some_and(|v| v == "yes") {
        message.to_uppercase()
    } else {
        message
    };

    text(StatusCode::OK, message)
}

async fn whoami(request: Rq) -> Response {
    let remote_addr = request.origin.remote_addr;
    text(StatusCode::OK, format!("you are {remote_addr}"))
}

/// A plain `async fn` is a middleware as long as it does not change the request
/// and response types. Here it just logs; `next` runs the rest of the chain.
async fn logging(request: Rq, next: DFnOnce<Rq, Response>) -> Response {
    let method = request.origin.http.method().clone();
    let path = request.origin.http.uri().path().to_owned();

    let response = next(request).await;

    println!("{method} {path} -> {}", response.http.status());
    response
}

/// Runs when no route matched. `allowed_methods` is non-empty exactly when the
/// path matched but the method did not, which is the 405 case.
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

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let router = router::first::Router::with_fallback_handler(fallback).and_routes(|r| {
        r.middleware(logging, |r| {
            r.route(
                Route::with_method(&Method::GET)
                    .and_path("/hello/{name}")
                    .and_handler(hello),
            )
            .route(
                Route::with_any_method()
                    .and_path("/whoami")
                    .and_handler(whoami),
            )
        })
    });

    let responder_factory = ResponderFactory::with_router(router).and_extensions(Extensions {
        greeting: "Hello".to_owned(),
    });
    let server_service = Arc::new(ServerService::with_responder_factory(responder_factory));

    let addr = SocketAddr::from(([127, 0, 0, 1], 8080));
    let listener = TcpListener::bind(addr).await?;
    println!("listening on http://{addr}");

    loop {
        let (stream, remote_addr) = listener.accept().await?;
        let session_service = server_service.make_session_service(remote_addr);
        tokio::task::spawn(async move {
            if let Err(error) = hyper::server::conn::http1::Builder::new()
                .serve_connection(TokioIo::new(stream), session_service)
                .await
            {
                eprintln!("connection error: {error}");
            }
        });
    }
}
