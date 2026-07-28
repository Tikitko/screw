//! A JSON chat over WebSockets, routed and middlewared like any other endpoint.
//!
//! `WebSocketMiddlewareConverter` performs the upgrade handshake;
//! `JsonApiStreamConverter` turns the raw socket into an `ApiChannel`, whose
//! sender and receiver speak your own message types instead of frames.
//!
//! ```sh
//! cargo run -p screw-api --example websocket_chat --features json,ws
//! ```
//!
//! Then open <http://127.0.0.1:8080> in two browser tabs and talk to yourself,
//! or use any WebSocket client against `ws://127.0.0.1:8080/ws/chat/lobby` and
//! send `{"text":"hi"}`.

use hyper::{Method, StatusCode};
use hyper_util::rt::TokioIo;
use screw_api::channel::{ApiChannel, ApiChannelReceiverError};
use screw_api::json::JsonApiStreamConverter;
use screw_core::request::Request;
use screw_core::responder_factory::ResponderFactory;
use screw_core::response::Response;
use screw_core::routing::route::first::Route;
use screw_core::routing::router::{self, RoutedRequest};
use screw_core::server::ServerService;
use screw_ws::{
    WebSocketContent, WebSocketMiddlewareConverter, WebSocketOriginContent, WebSocketRequest,
    WebSocketResponse,
};
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::TcpListener;
use tokio::sync::broadcast;

// ---------------------------------------------------------------- messages

#[derive(Deserialize)]
struct ClientMessage {
    text: String,
}

#[derive(Clone, Serialize)]
struct ServerMessage {
    room: String,
    from: String,
    text: String,
}

// ---------------------------------------------------------------- state

struct Extensions {
    /// Every connected socket subscribes to this; each message carries the room
    /// it belongs to and subscribers filter on it.
    chat: broadcast::Sender<ServerMessage>,
}

// ---------------------------------------------------------------- request

struct ChatContent {
    room: String,
    who: String,
    extensions: Arc<Extensions>,
}

impl WebSocketContent<Extensions> for ChatContent {
    fn create(origin: WebSocketOriginContent<Extensions>) -> Self {
        Self {
            room: origin.path.get("room").unwrap_or("lobby").to_owned(),
            who: origin.remote_addr.to_string(),
            extensions: origin.extensions,
        }
    }
}

// ---------------------------------------------------------------- handler

async fn chat(
    request: WebSocketRequest<ChatContent, ApiChannel<ServerMessage, ClientMessage>, Extensions>,
) -> WebSocketResponse {
    let (content, upgrade) = request.split();

    // The handler returns right away — the closure below runs on its own task
    // once hyper has finished the upgrade.
    upgrade.on(move |channel| async move {
        let ApiChannel {
            mut sender,
            mut receiver,
        } = channel;

        let chat = content.extensions.chat.clone();
        let mut subscription = chat.subscribe();

        println!("{} joined {}", content.who, content.room);

        loop {
            tokio::select! {
                incoming = receiver.receive() => match incoming {
                    Ok(message) => {
                        // Ignored when nobody is listening, which is fine.
                        let _ = chat.send(ServerMessage {
                            room: content.room.clone(),
                            from: content.who.clone(),
                            text: message.text,
                        });
                    }
                    // A control or binary frame is not something this protocol
                    // uses, but it is no reason to hang up either.
                    Err(ApiChannelReceiverError::UnsupportedMessage) => continue,
                    Err(_) => break,
                },
                outgoing = subscription.recv() => match outgoing {
                    Ok(message) if message.room == content.room => {
                        if sender.send(message).await.is_err() {
                            break;
                        }
                    }
                    Ok(_) => continue,
                    // Lagged behind the broadcast buffer, or the sender is gone.
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => break,
                },
            }
        }

        let _ = sender.close().await;
        println!("{} left {}", content.who, content.room);
    })
}

// ---------------------------------------------------------------- page

const PAGE: &str = r#"<!doctype html>
<meta charset="utf-8">
<title>screw chat</title>
<input id="text" placeholder="say something" autofocus>
<ul id="log"></ul>
<script>
  const socket = new WebSocket(`ws://${location.host}/ws/chat/lobby`);
  socket.onmessage = (event) => {
    const message = JSON.parse(event.data);
    const item = document.createElement("li");
    item.textContent = `${message.from}: ${message.text}`;
    document.getElementById("log").append(item);
  };
  document.getElementById("text").addEventListener("keydown", (event) => {
    if (event.key !== "Enter" || !event.target.value) return;
    socket.send(JSON.stringify({ text: event.target.value }));
    event.target.value = "";
  });
</script>
"#;

async fn fallback(request: RoutedRequest<Request<Extensions>>) -> Response {
    let http = if request.origin.http.uri().path() == "/" {
        hyper::Response::builder()
            .status(StatusCode::OK)
            .header(hyper::header::CONTENT_TYPE, "text/html; charset=utf-8")
            .body(screw_core::body::full(PAGE))
            .unwrap()
    } else {
        hyper::Response::builder()
            .status(StatusCode::NOT_FOUND)
            .body(screw_core::body::empty())
            .unwrap()
    };
    Response { http }
}

// ---------------------------------------------------------------- server

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let router = router::first::Router::with_fallback_handler(fallback).and_routes(|r| {
        r.scoped_middleware(
            "/ws",
            WebSocketMiddlewareConverter::with_stream_converter(JsonApiStreamConverter {
                pretty_printed: false,
            })
            // `Option<tokio_tungstenite::protocol::WebSocketConfig>`; `None`
            // keeps tungstenite's defaults.
            .and_config(None),
            |r| {
                r.route(
                    Route::with_method(&Method::GET)
                        .and_path("/chat/{room}")
                        .and_handler(chat),
                )
            },
        )
    });

    let (chat, _) = broadcast::channel(64);
    let responder_factory =
        ResponderFactory::with_router(router).and_extensions(Extensions { chat });
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
                // Without this the handshake succeeds and the upgrade never
                // arrives, so the socket goes quiet.
                .with_upgrades()
                .await
            {
                eprintln!("connection error: {error}");
            }
        });
    }
}
