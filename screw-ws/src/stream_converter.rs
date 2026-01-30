use hyper::upgrade::Upgraded;
use hyper_util::rt::TokioIo;
use tokio_tungstenite::WebSocketStream;

#[async_trait]
pub trait WebSocketStreamConverter<Stream> {
    async fn convert_stream(&self, stream: WebSocketStream<TokioIo<Upgraded>>) -> Stream;
}
