use std::any::Any;
use std::future::{Future, poll_fn};
use std::panic::AssertUnwindSafe;
use std::pin::pin;
use std::task::Poll;

pub type PanicPayload = Box<dyn Any + Send>;

pub async fn catch_unwind<F>(future: F) -> Result<F::Output, PanicPayload>
where
    F: Future,
{
    let mut future = pin!(future);
    poll_fn(move |context| {
        match std::panic::catch_unwind(AssertUnwindSafe(|| future.as_mut().poll(context))) {
            Ok(Poll::Ready(output)) => Poll::Ready(Ok(output)),
            Ok(Poll::Pending) => Poll::Pending,
            Err(payload) => Poll::Ready(Err(payload)),
        }
    })
    .await
}
