use super::super::*;
use futures::{future, StreamExt};
use hyper::upgrade::Upgraded;
use hyper_util::rt::TokioIo;
use screw_ws::WebSocketStreamConverter;
use serde::Deserialize;
use serde::Serialize;
use tokio_tungstenite::WebSocketStream;

#[derive(Clone, Copy, Debug)]
pub struct XmlApiWebSocketConverter;

#[async_trait]
impl<Send, Receive> WebSocketStreamConverter<channel::ApiChannel<Send, Receive>>
    for XmlApiWebSocketConverter
where
    Send: Serialize + std::marker::Send + 'static,
    Receive: for<'de> Deserialize<'de> + std::marker::Send + 'static,
{
    async fn convert_stream(
        &self,
        stream: WebSocketStream<TokioIo<Upgraded>>,
    ) -> channel::ApiChannel<Send, Receive> {
        let (sink, stream) = stream.split();

        let sender = channel::first::ApiChannelSender::with_sink(sink)
            .and_convert_typed_message_fn(move |typed_message| {
                let generic_message_result = quick_xml::se::to_string(&typed_message);
                future::ready(generic_message_result.map_err(|e| e.into()))
            });

        let receiver = channel::first::ApiChannelReceiver::with_stream(stream)
            .and_convert_generic_message_fn(|generic_message| {
                let typed_message_result = quick_xml::de::from_str(generic_message.as_str());
                future::ready(typed_message_result.map_err(|e| e.into()))
            });

        channel::ApiChannel { sender, receiver }
    }
}
