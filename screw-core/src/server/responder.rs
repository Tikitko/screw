use crate::body::ResponseBody;
use hyper::body::Incoming;
use hyper::{Request, Response};
use std::future::Future;

pub trait Responder {
    type ResponseFuture: Future<Output = Response<ResponseBody>>;
    fn response(&self, request: Request<Incoming>) -> Self::ResponseFuture;
}

impl<'a, T> Responder for &'a T
where
    T: Responder + 'a,
{
    type ResponseFuture = T::ResponseFuture;
    fn response(&self, request: Request<Incoming>) -> Self::ResponseFuture {
        (**self).response(request)
    }
}

impl<T> Responder for Box<T>
where
    T: Responder + ?Sized,
{
    type ResponseFuture = T::ResponseFuture;
    fn response(&self, request: Request<Incoming>) -> Self::ResponseFuture {
        (**self).response(request)
    }
}
