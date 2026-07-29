use super::*;
use futures_util::{FutureExt, TryFutureExt};
use hyper::body::Incoming;
use hyper::header::{self, HeaderValue};
use hyper::{Method, StatusCode, Version, upgrade};
use hyper_util::rt::TokioIo;
use screw_components::dyn_fn::DFnOnce;
use screw_core::request::Request;
use screw_core::response::Response;
use screw_core::routing::middleware::Middleware;
use screw_core::routing::router::RoutedRequest;
use std::sync::Arc;
use tokio::task;
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::error::ProtocolError;
use tokio_tungstenite::tungstenite::handshake::derive_accept_key;
use tokio_tungstenite::tungstenite::protocol::{Role, WebSocketConfig};

fn is_get_method(request: &hyper::Request<Incoming>) -> bool {
    request.method() == Method::GET
}
fn is_http_version_11_or_larger(request: &hyper::Request<Incoming>) -> bool {
    request.version() >= Version::HTTP_11
}
fn is_connection_header_upgrade(request: &hyper::Request<Incoming>) -> bool {
    request
        .headers()
        .get("Connection")
        .and_then(|h| h.to_str().ok())
        .map(|h| {
            h.split([' ', ','])
                .any(|p| p.eq_ignore_ascii_case("Upgrade"))
        })
        .unwrap_or(false)
}
fn is_upgrade_header_web_socket(request: &hyper::Request<Incoming>) -> bool {
    request
        .headers()
        .get("Upgrade")
        .and_then(|h| h.to_str().ok())
        .map(|h| h.eq_ignore_ascii_case("websocket"))
        .unwrap_or(false)
}
fn is_web_socket_version_header_13(request: &hyper::Request<Incoming>) -> bool {
    request
        .headers()
        .get("Sec-WebSocket-Version")
        .map(|h| h == "13")
        .unwrap_or(false)
}
fn get_web_socket_key_header(request: &hyper::Request<Incoming>) -> Option<&HeaderValue> {
    request.headers().get("Sec-WebSocket-Key")
}

fn try_upgradable(
    http_request: &mut hyper::Request<Incoming>,
) -> Result<WebSocketUpgradable, ProtocolError> {
    if !is_get_method(http_request) {
        return Err(ProtocolError::WrongHttpMethod);
    }

    if !is_http_version_11_or_larger(http_request) {
        return Err(ProtocolError::WrongHttpVersion);
    }

    if !is_connection_header_upgrade(http_request) {
        return Err(ProtocolError::MissingConnectionUpgradeHeader);
    }

    if !is_upgrade_header_web_socket(http_request) {
        return Err(ProtocolError::MissingUpgradeWebSocketHeader);
    }

    if !is_web_socket_version_header_13(http_request) {
        return Err(ProtocolError::MissingSecWebSocketVersionHeader);
    }

    let key = derive_accept_key(
        get_web_socket_key_header(http_request)
            .ok_or(ProtocolError::MissingSecWebSocketKey)?
            .as_bytes(),
    );

    let on_upgrade = upgrade::on(http_request);

    Ok(WebSocketUpgradable { on_upgrade, key })
}

pub struct WebSocketMiddlewareConverter<StreamConverter>
where
    StreamConverter: Sync + Send + 'static,
{
    stream_converter: Arc<StreamConverter>,
    config: Option<WebSocketConfig>,
}

impl<StreamConverter> WebSocketMiddlewareConverter<StreamConverter>
where
    StreamConverter: Sync + Send + 'static,
{
    pub fn with_stream_converter(stream_converter: StreamConverter) -> Self {
        Self {
            stream_converter: Arc::new(stream_converter),
            config: None,
        }
    }
    pub fn and_config(mut self, config: Option<WebSocketConfig>) -> Self {
        self.config = config;
        self
    }
}

impl<StreamConverter, Content, Stream, Extensions>
    Middleware<WebSocketRequest<Content, Stream, Extensions>, WebSocketResponse>
    for WebSocketMiddlewareConverter<StreamConverter>
where
    StreamConverter: WebSocketStreamConverter<Stream> + Sync + Send + 'static,
    Content: WebSocketContent<Extensions> + Send + 'static,
    Stream: Send + Sync + 'static,
    Extensions: Sync + Send + 'static,
{
    type Request = RoutedRequest<Request<Extensions>>;
    type Response = Response;
    async fn respond(
        &self,
        mut routed_request: RoutedRequest<Request<Extensions>>,
        next: DFnOnce<WebSocketRequest<Content, Stream, Extensions>, WebSocketResponse>,
    ) -> Response {
        let http_response = match try_upgradable(&mut routed_request.origin.http) {
            Ok(upgradable) => {
                let request_content = Content::create(WebSocketOriginContent {
                    path: routed_request.path,
                    query: routed_request.query,
                    http_parts: routed_request.origin.http.into_parts().0,
                    remote_addr: routed_request.origin.remote_addr,
                    extensions: routed_request.origin.extensions,
                });

                let stream_converter = self.stream_converter.clone();
                let request_upgrade = WebSocketUpgrade {
                    convert_stream_fn: Box::new(move |generic_stream| {
                        let stream_converter = stream_converter.clone();
                        Box::pin(
                            async move { stream_converter.convert_stream(generic_stream).await },
                        )
                    }),
                };

                let ws_request = WebSocketRequest {
                    content: request_content,
                    upgrade: request_upgrade,
                    _p_e: Default::default(),
                };

                let ws_response = next(ws_request).await;

                let config = self.config;
                let future = upgradable
                    .on_upgrade
                    .and_then(move |upgraded| {
                        WebSocketStream::from_raw_socket(
                            TokioIo::new(upgraded),
                            Role::Server,
                            config,
                        )
                        .map(Ok)
                    })
                    .and_then(move |stream| (ws_response.upgraded_fn)(stream).map(Ok));

                task::spawn(future);

                hyper::Response::builder()
                    .status(StatusCode::SWITCHING_PROTOCOLS)
                    .header("Connection", "Upgrade")
                    .header("Upgrade", "websocket")
                    .header("Sec-WebSocket-Accept", upgradable.key)
                    .body(screw_core::body::empty())
                    .unwrap()
            }
            Err(protocol_error) => match protocol_error {
                ProtocolError::WrongHttpMethod => hyper::Response::builder()
                    .status(StatusCode::METHOD_NOT_ALLOWED)
                    .header(header::ALLOW, "GET")
                    .body(screw_core::body::empty())
                    .unwrap(),
                _ => hyper::Response::builder()
                    .status(StatusCode::BAD_REQUEST)
                    .body(screw_core::body::empty())
                    .unwrap(),
            },
        };
        Response {
            http: http_response,
        }
    }
}
