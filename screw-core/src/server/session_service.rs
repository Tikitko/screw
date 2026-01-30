use super::*;
use crate::body::ResponseBody;
use hyper::body::Incoming;
use hyper::service::Service;
use hyper::{Request, Response};
use std::convert::Infallible;
use std::future::Future;
use std::pin::Pin;

pub struct SessionService<R>
where
    R: Responder,
    R::ResponseFuture: Send + 'static,
{
    pub(super) responder: R,
}

impl<R> Service<Request<Incoming>> for SessionService<R>
where
    R: Responder,
    R::ResponseFuture: Send + 'static,
{
    type Response = Response<ResponseBody>;
    type Error = Infallible;
    type Future = Pin<Box<dyn Future<Output = Result<Response<ResponseBody>, Infallible>> + Send>>;

    fn call(&self, request: Request<Incoming>) -> Self::Future {
        let response_future = self.responder.response(request);
        Box::pin(async move {
            let response = response_future.await;
            Ok(response)
        })
    }
}
