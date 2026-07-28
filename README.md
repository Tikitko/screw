# screw

A small, strongly typed HTTP/WebSocket server toolkit for Rust, built on [hyper](https://hyper.rs) 1.x.

`screw` is not a "batteries included" framework — it is a thin, explicit layer over hyper that gives you:

- a **router** with actix-style path patterns, method matching, and query/path extraction;
- **middlewares that can change the request and response types** as the chain nests, so a handler's signature says exactly what it receives;
- **JSON/XML API layers** that turn a raw `hyper::Request` into a typed request struct and a typed response enum into a serialized HTTP response;
- a **WebSocket layer** with typed send/receive channels over the same routing and middleware machinery.

Everything is opt-in via Cargo features, and every stage of construction is a distinct type, so a half-built router will not compile.

## Workspace layout

| Crate | What it is |
| --- | --- |
| [`screw-core`](screw-core) | Router, routes, middleware trait, request/response types, hyper service glue. The only crate you always need. |
| [`screw-api`](screw-api) | Typed request/response API layer with JSON and XML converters, plus typed WebSocket channels. |
| [`screw-ws`](screw-ws) | WebSocket upgrade handling as a middleware, generic over the stream type. |
| [`screw-components`](screw-components) | Tiny shared aliases: `DFn`, `DFnOnce`, `DFuture`, `DResult`, `DError`. |

## Installation

The crates are consumed from git (or as path dependencies inside this workspace):

```toml
[dependencies]
screw-core = { git = "https://github.com/tikitko/screw" }
screw-api = { git = "https://github.com/tikitko/screw", features = ["json"] }
screw-components = { git = "https://github.com/tikitko/screw" }
hyper = { version = "1", features = ["full"] }
hyper-util = { version = "0.1", features = ["tokio"] }
tokio = { version = "1", features = ["full"] }
serde = { version = "1", features = ["derive"] }
```

The crates carry no `version` field — versioning is done through git, so pin a `tag`, `branch`, or `rev` when you need a fixed state:

```toml
screw-core = { git = "https://github.com/tikitko/screw", rev = "…" }
```

`screw` does not pick a runtime or an accept loop for you — you bring your own `tokio` listener and hand each connection to a `SessionService`.

### `screw-api` features

| Feature | Enables |
| --- | --- |
| `json` | `JsonApiMiddlewareConverter` — JSON request parsing and response serialization. |
| `xml` | `XmlApiMiddlewareConverter` — the same, over `quick-xml`. |
| `ws` | `channel::ApiChannel`, used with `screw-ws`. |

No features are on by default. The stream converters that bridge the two worlds — `json::JsonApiStreamConverter` and `xml::XmlApiWebSocketConverter` — need `ws` plus the matching format feature.

## Examples

Four runnable servers live in the workspace. Each listens on `127.0.0.1:8080` and its header comment lists the `curl` commands to try.

| Example | Run it | Shows |
| --- | --- | --- |
| [`hello`](screw-core/examples/hello.rs) | `cargo run -p screw-core --example hello` | The router on its own — path params, query, `Extensions`, a function middleware, 404 vs 405. |
| [`json_api`](screw-api/examples/json_api.rs) | `cargo run -p screw-api --example json_api --features json` | Typed JSON requests and responses, per-variant status codes, malformed bodies, `Infallible` failures, body-size limits. |
| [`typed_middleware`](screw-api/examples/typed_middleware.rs) | `cargo run -p screw-api --example typed_middleware --features json` | Middlewares that change the request type: `ApiRequest` → `Authed` → `AdminOnly`, with short-circuited 401/403. |
| [`websocket_chat`](screw-api/examples/websocket_chat.rs) | `cargo run -p screw-api --example websocket_chat --features json,ws` | The WebSocket upgrade as a middleware and a typed `ApiChannel`; open the served page in two tabs to chat. |

## Quick start

A complete JSON echo endpoint at `POST /api/echo/{id}`:

```rust
use hyper::{Method, StatusCode};
use hyper_util::rt::TokioIo;
use screw_api::json::JsonApiMiddlewareConverter;
use screw_api::request::{ApiRequest, ApiRequestContent, ApiRequestOriginContent};
use screw_api::response::{
    ApiResponse, ApiResponseContentBase, ApiResponseContentFailure, ApiResponseContentSuccess,
};
use screw_components::dyn_result::DResult;
use screw_core::request::Request;
use screw_core::response::Response;
use screw_core::responder_factory::ResponderFactory;
use screw_core::routing::route::first::Route;
use screw_core::routing::router::{self, RoutedRequest};
use screw_core::server::ServerService;
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::TcpListener;

// Shared application state, available to every handler.
struct Extensions {
    greeting: String,
}

// ---- request ----

#[derive(Deserialize)]
struct EchoData {
    message: String,
}

// What the handler actually gets. You decide which parts of the raw
// request are worth carrying, and in what shape.
struct EchoRequestContent {
    id: String,
    data: DResult<EchoData>,
    greeting: String,
}

impl ApiRequestContent<Extensions> for EchoRequestContent {
    type Data = EchoData;
    fn create(origin: ApiRequestOriginContent<Self::Data, Extensions>) -> Self {
        Self {
            id: origin.path.get("id").unwrap_or_default().to_owned(),
            data: origin.data_result,
            greeting: origin.extensions.greeting.clone(),
        }
    }
}

// ---- response ----

#[derive(Serialize)]
struct EchoPayload {
    id: String,
    message: String,
}

enum EchoSuccess {
    Echoed(EchoPayload),
}

impl ApiResponseContentBase for EchoSuccess {
    fn status_code(&self) -> &'static StatusCode {
        &StatusCode::OK
    }
}

impl ApiResponseContentSuccess for EchoSuccess {
    type Data = EchoPayload;
    fn identifier(&self) -> &'static str {
        "ECHOED"
    }
    fn description(&self) -> Option<String> {
        Some("Message echoed back".to_owned())
    }
    fn data(&self) -> &Self::Data {
        let Self::Echoed(payload) = self;
        payload
    }
}

enum EchoFailure {
    BadRequest,
}

impl ApiResponseContentBase for EchoFailure {
    fn status_code(&self) -> &'static StatusCode {
        &StatusCode::BAD_REQUEST
    }
}

impl ApiResponseContentFailure for EchoFailure {
    fn identifier(&self) -> &'static str {
        "BAD_REQUEST"
    }
    fn reason(&self) -> Option<String> {
        Some("Body is not a valid echo request".to_owned())
    }
}

// ---- handlers ----

async fn echo(
    request: ApiRequest<EchoRequestContent, Extensions>,
) -> ApiResponse<EchoSuccess, EchoFailure> {
    let content = request.content;
    let Ok(data) = content.data else {
        return ApiResponse::failure(EchoFailure::BadRequest);
    };
    ApiResponse::success(EchoSuccess::Echoed(EchoPayload {
        id: content.id,
        message: format!("{}, {}", content.greeting, data.message),
    }))
}

// Runs when nothing matched. `allowed_methods` is non-empty when the path
// matched but the method did not, which is exactly a 405 with `Allow`.
async fn fallback(request: RoutedRequest<Request<Extensions>>) -> Response {
    let mut builder = hyper::Response::builder();
    if request.allowed_methods.is_empty() {
        builder = builder.status(StatusCode::NOT_FOUND);
    } else {
        let allow = request
            .allowed_methods
            .iter()
            .map(|m| m.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        builder = builder
            .status(StatusCode::METHOD_NOT_ALLOWED)
            .header(hyper::header::ALLOW, allow);
    }
    Response {
        http: builder.body(screw_core::body::empty()).unwrap(),
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let router = router::first::Router::with_fallback_handler(fallback).and_routes(|r| {
        r.scoped_middleware("/api", JsonApiMiddlewareConverter::default(), |r| {
            r.route(
                Route::with_method(&Method::POST)
                    .and_path("/echo/{id}")
                    .and_handler(echo),
            )
        })
    });

    let responder_factory = ResponderFactory::with_router(router).and_extensions(Extensions {
        greeting: "hello".to_owned(),
    });
    let server_service = Arc::new(ServerService::with_responder_factory(responder_factory));

    let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 8080))).await?;
    loop {
        let (stream, remote_addr) = listener.accept().await?;
        let session_service = server_service.make_session_service(remote_addr);
        tokio::task::spawn(async move {
            let _ = hyper::server::conn::http1::Builder::new()
                .serve_connection(TokioIo::new(stream), session_service)
                .with_upgrades() // only needed if you also serve WebSockets
                .await;
        });
    }
}
```

```bash
curl -s -X POST localhost:8080/api/echo/42 -H 'Content-Type: application/json' -d '{"message":"world"}'
```

```json
{"success":{"identifier":"ECHOED","description":"Message echoed back","data":{"id":"42","message":"hello, world"}}}
```

A handler returning a failure produces the other variant, with that failure's status code:

```json
{"failure":{"identifier":"BAD_REQUEST","reason":"Body is not a valid echo request"}}
```

## How it fits together

```
TcpListener
  └── ServerService              — one per process, holds the router + extensions
        └── SessionService       — one per connection, implements hyper::service::Service
              └── Responder      — knows the remote address
                    └── Router   — matches method + path, builds RoutedRequest
                          └── middleware chain (outermost → innermost)
                                └── handler
```

### Builder stages

Types are built in named stages (`first`, `second`, `third`), each one returning the next. You cannot skip a step or reorder it:

```rust
Route::with_method(&Method::GET)   // route::first::Route
    .and_path("/post/{id}")        // route::second::Route
    .and_handler(handler);         // route::third::Route

router::first::Router::with_fallback_handler(fallback)
    .and_routes(|r| r);            // router::second::Router

ResponderFactory::with_router(router)
    .and_extensions(extensions);   // ready for ServerService
```

### Routing

- Path patterns come from [`actix-router`](https://docs.rs/actix-router): `/post/{id}`, `/files/{path}*`, and so on. Matched segments are read via `path.get("id")`.
- `Route::with_method`, `with_methods`, and `with_any_method` control method matching; `with_any_method` matches every method.
- The query string is parsed into a `HashMap<String, String>` on `RoutedRequest::query`.
- When two registered patterns match the same request, **the one registered first wins**.
- When a path matches but the method does not, the fallback handler runs with `allowed_methods` filled in from every route sharing that pattern — enough to answer `405` with a correct `Allow` header. For an unknown path, `allowed_methods` is empty.
- Percent-escapes in the path are decoded, except `%2F`, `%2B` and `%25`, which stay encoded so an escaped slash cannot forge an extra path segment. A malformed or non-UTF-8 escape leaves the path untouched rather than emptying it.

### Scopes and middleware

`Routes` is built with a closure per nesting level. `scoped` adds a path prefix, `middleware` adds a middleware, and `scoped_middleware` does both:

```rust
.and_routes(|r| {
    r.scoped_middleware("/api", JsonApiMiddlewareConverter::default(), |r| {
        r.middleware(Auth, |r| {
            r.route(/* … */)
        })
        .scoped("/v2", |r| {
            r.route(/* … */)
        })
    })
})
```

A middleware implements `Middleware<Rq, Rs>`, where `Rq`/`Rs` are what it hands *inward* and the associated `Request`/`Response` types are what it accepts from *outside*:

```rust
#[async_trait]
pub trait Middleware<Rq, Rs> {
    type Request;
    type Response;
    async fn respond(&self, request: Self::Request, next: DFnOnce<Rq, Rs>) -> Self::Response;
}
```

Because the inward and outward types are independent, each nesting level can change them. That is how `JsonApiMiddlewareConverter` turns a `RoutedRequest<Request<Extensions>>` into a typed `ApiRequest<Content, Extensions>` — and how your own middleware can, say, wrap an `ApiRequest` into an `Authed<…>` that only authenticated handlers accept:

```rust
#[async_trait]
impl<Content, Ext, Success, Failure>
    Middleware<Authed<Content, Ext>, ApiResponse<Success, Failure>> for Auth
where /* … */
{
    type Request = ApiRequest<Content, Ext>;
    type Response = ApiResponse<Success, Failure>;
    async fn respond(
        &self,
        request: ApiRequest<Content, Ext>,
        next: DFnOnce<Authed<Content, Ext>, ApiResponse<Success, Failure>>,
    ) -> ApiResponse<Success, Failure> {
        next(Authed { request }).await
    }
}
```

Middlewares nest arbitrarily deep, and each is entered outermost-first and left innermost-first. An `async fn(Rq, DFnOnce<Rq, Rs>) -> Rs` is also a `Middleware` when it does not change types, so simple logging or timing layers need no struct:

```rust
async fn logging(request: Mid, next: DFnOnce<Mid, MidRs>) -> MidRs {
    next(request).await
}
```

### Extensions

`Extensions` is your own type, threaded through the whole stack as `Arc<Extensions>` and reachable from `ApiRequestOriginContent::extensions` (or `Request::extensions` at the core level). Put your database pool, config, and clients there. It is shared, so interior mutability is up to you.

## The API layer

`ApiRequestContent<Extensions>` is the bridge from a raw request to your handler's argument. Its `create` gets everything the server knows:

```rust
pub struct ApiRequestOriginContent<Data, Extensions> {
    pub path: Path<String>,
    pub query: HashMap<String, String>,
    pub http_parts: Parts,
    pub remote_addr: SocketAddr,
    pub extensions: Arc<Extensions>,
    pub data_result: DResult<Data>,
}
```

Body parsing is *not* fatal: `data_result` is a `DResult<Data>`, so a bad body reaches your handler as an `Err` and you decide the response. `()` implements `ApiRequestContent`, which is convenient for endpoints that ignore the request entirely.

On the way out, `ApiResponse<Success, Failure>` is an enum of two content traits:

- `ApiResponseContentSuccess` — `identifier`, `description`, and a serializable `data`;
- `ApiResponseContentFailure` — `identifier` and `reason`;
- both extend `ApiResponseContentBase`, which supplies the HTTP status code.

`Infallible` implements all three, so `ApiResponse<Success, Infallible>` is the type for an endpoint that cannot fail. `ApiResponse` also has `From<Result<Success, Failure>>`, so a handler can end in `result.into()`.

Serialization failures never leak: if the response cannot be written, the middleware answers `500` with an empty body.

### Request hardening

Both API converters, before deserializing:

- require a matching `Content-Type` (`application/json` / `application/xml`), comparing the media type case-insensitively and ignoring parameters like `; charset=utf-8`; a missing or empty header is rejected;
- cap the body at `max_body_size`, defaulting to `DEFAULT_MAX_BODY_SIZE` (2 MiB), so an unbounded upload cannot exhaust memory.

Both are plain public fields, so you can raise or lower them per scope:

```rust
JsonApiMiddlewareConverter {
    pretty_printed: false,
    max_body_size: 512 * 1024,
}
```

Every rejection above lands in `data_result` as an `Err`, which your `ApiRequestContent` can inspect.

The XML layer is the same shape: swap `JsonApiMiddlewareConverter` for `XmlApiMiddlewareConverter` and nothing else in the example changes, since `Serialize`/`Deserialize` do the work either way.

## WebSockets

`WebSocketMiddlewareConverter` performs the upgrade handshake and is generic over a *stream converter*, which decides what the handler's stream looks like. Pairing it with `screw-api`'s `JsonApiStreamConverter` yields an `ApiChannel<Send, Receive>` — a typed sender and receiver of JSON messages:

```rust
use screw_api::channel::ApiChannel;
use screw_api::json::JsonApiStreamConverter;
use screw_ws::{
    WebSocketContent, WebSocketMiddlewareConverter, WebSocketOriginContent, WebSocketRequest,
    WebSocketResponse,
};

#[derive(Deserialize)]
struct Incoming { text: String }

#[derive(Serialize)]
struct Outgoing { text: String }

struct ChatContent { room: String }

impl WebSocketContent<Extensions> for ChatContent {
    fn create(origin: WebSocketOriginContent<Extensions>) -> Self {
        Self { room: origin.path.get("room").unwrap_or_default().to_owned() }
    }
}

async fn chat(
    request: WebSocketRequest<ChatContent, ApiChannel<Outgoing, Incoming>, Extensions>,
) -> WebSocketResponse {
    let (content, upgrade) = request.split();
    // The handler returns immediately; this closure runs after the upgrade completes.
    upgrade.on(move |mut channel| async move {
        while let Ok(message) = channel.receiver.receive().await {
            let echoed = Outgoing { text: format!("[{}] {}", content.room, message.text) };
            if channel.sender.send(echoed).await.is_err() {
                break;
            }
        }
    })
}
```

Registered like any other route:

```rust
r.scoped_middleware(
    "/ws",
    WebSocketMiddlewareConverter::with_stream_converter(JsonApiStreamConverter {
        pretty_printed: false,
    })
    .and_config(None), // Option<WebSocketConfig> from tokio-tungstenite
    |r| {
        r.route(
            Route::with_method(&Method::GET)
                .and_path("/chat/{room}")
                .and_handler(chat),
        )
    },
)
```

Notes:

- Serve connections with `.with_upgrades()`, or the handshake will succeed and the upgrade will never arrive.
- The middleware validates the handshake before anything else: `GET`, HTTP/1.1 or newer, `Connection: Upgrade`, `Upgrade: websocket`, `Sec-WebSocket-Version: 13`, and a `Sec-WebSocket-Key`. Any violation gets `400`, except a non-`GET` request, which gets `405` with `Allow: GET` — though a route registered as `GET`-only never reaches the middleware for other methods, since the router sends those to the fallback first.
- `ApiChannelReceiver::receive` reports control and binary frames as `UnsupportedMessage` and a close frame as `Closed`, so the loop above ends when the peer disconnects.
- `WebSocketStreamConverter` is a public trait — implement it for a protocol that is not JSON or XML, and the rest of the machinery is unchanged.

## Building and testing

```bash
cargo build --all-features
```

```bash
cargo test --all-features
```

`cargo test --all-features` also compiles every example, so they cannot drift from the crates. CI runs both on every push and pull request against `master`.

## License

MIT — see [LICENSE](LICENSE).
