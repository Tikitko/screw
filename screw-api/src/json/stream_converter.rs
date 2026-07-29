use super::super::*;
use futures::{StreamExt, future};
use hyper::upgrade::Upgraded;
use hyper_util::rt::TokioIo;
use screw_ws::WebSocketStreamConverter;
use serde::Deserialize;
use serde::Serialize;
use tokio_tungstenite::WebSocketStream;

#[derive(Clone, Copy, Debug)]
pub struct JsonApiStreamConverter {
    pub pretty_printed: bool,
}

impl<Send, Receive> WebSocketStreamConverter<channel::ApiChannel<Send, Receive>>
    for JsonApiStreamConverter
where
    Send: Serialize + std::marker::Send + 'static,
    Receive: for<'de> Deserialize<'de> + std::marker::Send + 'static,
{
    async fn convert_stream(
        &self,
        stream: WebSocketStream<TokioIo<Upgraded>>,
    ) -> channel::ApiChannel<Send, Receive> {
        let (sink, stream) = stream.split();
        let pretty_printed = self.pretty_printed;

        let sender = channel::first::ApiChannelSender::with_sink(sink)
            .and_convert_typed_message_fn(move |typed_message| {
                let generic_message_result = if pretty_printed {
                    serde_json::to_string_pretty(&typed_message)
                } else {
                    serde_json::to_string(&typed_message)
                };
                future::ready(generic_message_result.map_err(|e| e.into()))
            });

        let receiver = channel::first::ApiChannelReceiver::with_stream(stream)
            .and_convert_generic_message_fn(|generic_message| {
                let typed_message_result = serde_json::from_str(generic_message.as_str());
                future::ready(typed_message_result.map_err(|e| e.into()))
            });

        channel::ApiChannel { sender, receiver }
    }
}
