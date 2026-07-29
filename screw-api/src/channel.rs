use futures::stream::SplitStream;
use futures::{SinkExt, StreamExt};
use hyper::upgrade::Upgraded;
use hyper_util::rt::TokioIo;
use screw_components::dyn_fn::DFn;
use screw_components::dyn_result::DError;
use serde::{Deserialize, Serialize};
use tokio_tungstenite::tungstenite::Error;

pub struct ApiChannel<Send, Receive>
where
    Send: Serialize + std::marker::Send + 'static,
    Receive: for<'de> Deserialize<'de> + std::marker::Send + 'static,
{
    pub sender: second::ApiChannelSender<Send>,
    pub receiver: second::ApiChannelReceiver<Receive>,
}

pub enum ApiChannelSenderError {
    Convert(DError),
    Tungstenite(Error),
}

pub enum ApiChannelReceiverError {
    Convert(DError),
    Tungstenite(Error),
    NoMessage,
    UnsupportedMessage,
    Closed,
}

pub mod first {
    use super::*;
    use futures::stream::SplitSink;
    use screw_components::dyn_fn::AsDynFn;
    use screw_components::dyn_result::DResult;
    use serde::Serialize;
    use std::future::Future;
    use tokio_tungstenite::WebSocketStream;
    use tokio_tungstenite::tungstenite::Message;

    pub struct ApiChannelSender {
        sink: SplitSink<WebSocketStream<TokioIo<Upgraded>>, Message>,
    }

    impl ApiChannelSender {
        pub fn with_sink(sink: SplitSink<WebSocketStream<TokioIo<Upgraded>>, Message>) -> Self {
            Self { sink }
        }

        pub fn and_convert_typed_message_fn<Send, HFn, HFut>(
            self,
            convert_typed_message_fn: HFn,
        ) -> second::ApiChannelSender<Send>
        where
            Send: Serialize + std::marker::Send + 'static,
            HFn: Fn(Send) -> HFut + std::marker::Send + Sync + 'static,
            HFut: Future<Output = DResult<String>> + std::marker::Send + 'static,
        {
            second::ApiChannelSender {
                sink: self.sink,
                convert_typed_message_fn: convert_typed_message_fn.to_dyn_fn(),
            }
        }
    }

    pub struct ApiChannelReceiver {
        stream: SplitStream<WebSocketStream<TokioIo<Upgraded>>>,
    }

    impl ApiChannelReceiver {
        pub fn with_stream(stream: SplitStream<WebSocketStream<TokioIo<Upgraded>>>) -> Self {
            Self { stream }
        }

        pub fn and_convert_generic_message_fn<Receive, HFn, HFut>(
            self,
            convert_generic_message_fn: HFn,
        ) -> second::ApiChannelReceiver<Receive>
        where
            for<'de> Receive: Deserialize<'de> + std::marker::Send + 'static,
            HFn: Fn(String) -> HFut + std::marker::Send + Sync + 'static,
            HFut: Future<Output = DResult<Receive>> + std::marker::Send + 'static,
        {
            second::ApiChannelReceiver {
                stream: self.stream,
                convert_generic_message_fn: convert_generic_message_fn.to_dyn_fn(),
            }
        }
    }
}

pub mod second {
    use super::*;
    use futures::stream::SplitSink;
    use screw_components::dyn_result::DResult;
    use serde::Serialize;
    use tokio_tungstenite::WebSocketStream;
    use tokio_tungstenite::tungstenite::Message;

    pub struct ApiChannelSender<Send>
    where
        Send: Serialize + std::marker::Send + 'static,
    {
        pub(super) sink: SplitSink<WebSocketStream<TokioIo<Upgraded>>, Message>,
        pub(super) convert_typed_message_fn: DFn<Send, DResult<String>>,
    }

    impl<Send> ApiChannelSender<Send>
    where
        Send: Serialize + std::marker::Send + 'static,
    {
        pub async fn send(&mut self, typed_message: Send) -> Result<(), ApiChannelSenderError> {
            let convert_typed_message_fn = &self.convert_typed_message_fn;

            let generic_message = convert_typed_message_fn(typed_message)
                .await
                .map_err(ApiChannelSenderError::Convert)?;
            self.sink
                .send(Message::Text(generic_message.into()))
                .await
                .map_err(ApiChannelSenderError::Tungstenite)?;
            Ok(())
        }

        pub async fn close(&mut self) -> Result<(), ApiChannelSenderError> {
            self.sink
                .send(Message::Close(None))
                .await
                .map_err(ApiChannelSenderError::Tungstenite)
        }
    }

    pub struct ApiChannelReceiver<Receive>
    where
        for<'de> Receive: Deserialize<'de> + std::marker::Send + 'static,
    {
        pub(super) stream: SplitStream<WebSocketStream<TokioIo<Upgraded>>>,
        pub(super) convert_generic_message_fn: DFn<String, DResult<Receive>>,
    }

    impl<Receive> ApiChannelReceiver<Receive>
    where
        for<'de> Receive: Deserialize<'de> + std::marker::Send + 'static,
    {
        pub async fn receive(&mut self) -> Result<Receive, ApiChannelReceiverError> {
            let convert_generic_message_fn = &self.convert_generic_message_fn;

            let message_type_result = self
                .stream
                .next()
                .await
                .ok_or(ApiChannelReceiverError::NoMessage)?;
            let message_type = message_type_result.map_err(ApiChannelReceiverError::Tungstenite)?;
            let generic_message = match message_type {
                Message::Text(generic_message) => Ok(generic_message.to_string()),
                Message::Ping(_) | Message::Pong(_) | Message::Binary(_) | Message::Frame(_) => {
                    Err(ApiChannelReceiverError::UnsupportedMessage)
                }
                Message::Close(_) => Err(ApiChannelReceiverError::Closed),
            }?;
            let typed_message = convert_generic_message_fn(generic_message)
                .await
                .map_err(ApiChannelReceiverError::Convert)?;
            Ok(typed_message)
        }
    }
}
