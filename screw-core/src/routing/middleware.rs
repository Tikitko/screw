use screw_components::dyn_fn::DFnOnce;
use std::future::Future;
use std::sync::Arc;

#[async_trait]
pub trait Middleware<Rq, Rs> {
    type Request;
    type Response;
    async fn respond(&self, request: Self::Request, next: DFnOnce<Rq, Rs>) -> Self::Response;
}

pub struct Chained<Outer, Inner> {
    outer: Arc<Outer>,
    inner: Arc<Inner>,
}

impl<Outer, Inner> Chained<Outer, Inner> {
    pub fn new(outer: Arc<Outer>, inner: Arc<Inner>) -> Self {
        Self { outer, inner }
    }
}

#[async_trait]
impl<Outer, Inner, Rq, Rs, MidRq, MidRs> Middleware<Rq, Rs> for Chained<Outer, Inner>
where
    Outer: Middleware<MidRq, MidRs> + Send + Sync + 'static,
    Outer::Request: Send + 'static,
    Inner: Middleware<Rq, Rs, Request = MidRq, Response = MidRs> + Send + Sync + 'static,
    MidRq: Send + 'static,
    MidRs: Send + 'static,
    Rq: Send + 'static,
    Rs: Send + 'static,
{
    type Request = Outer::Request;
    type Response = Outer::Response;
    async fn respond(&self, request: Self::Request, next: DFnOnce<Rq, Rs>) -> Self::Response {
        let inner = self.inner.clone();
        let middle_next: DFnOnce<MidRq, MidRs> = Box::new(move |middle_request| {
            Box::pin(async move { inner.respond(middle_request, next).await })
        });
        self.outer.respond(request, middle_next).await
    }
}

#[async_trait]
impl<Rq, Rs> Middleware<Rq, Rs> for ()
where
    Rq: Send + 'static,
    Rs: Send + 'static,
{
    type Request = Rq;
    type Response = Rs;
    async fn respond(&self, request: Self::Request, next: DFnOnce<Rq, Rs>) -> Self::Response {
        next(request).await
    }
}

#[async_trait]
impl<Rq, Rs, HFn, HFut> Middleware<Rq, Rs> for HFn
where
    Rq: Send + 'static,
    Rs: Send + 'static,
    HFn: Fn(Rq, DFnOnce<Rq, Rs>) -> HFut + Send + Sync + 'static,
    HFut: Future<Output = Rs> + Send + 'static,
{
    type Request = Rq;
    type Response = Rs;
    async fn respond(&self, request: Self::Request, next: DFnOnce<Rq, Rs>) -> Self::Response {
        self(request, next).await
    }
}
