use hyper::upgrade::Upgraded;
use hyper_util::rt::TokioIo;
use tokio_tungstenite::WebSocketStream;

use std::future::Future;

pub trait WebSocketStreamConverter<Stream> {
    fn convert_stream(
        &self,
        stream: WebSocketStream<TokioIo<Upgraded>>,
    ) -> impl Future<Output = Stream> + Send;
}
