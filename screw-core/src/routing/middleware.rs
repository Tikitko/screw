use screw_components::dyn_fn::DFnOnce;
use std::future::Future;
use std::sync::Arc;

/// A step in a request's path to its handler, which may change the request and
/// response types on the way.
///
/// The four types read as two pairs. `Self::Request` and `Self::Response` are
/// what this middleware is handed and what it must produce -- its view of the
/// outside. `Rq` and `Rs` are what it passes inward through `next` and gets back
/// -- its view of whatever it wraps.
///
/// `Rq` and `Rs` are parameters of the trait rather than associated types on
/// purpose: one converter can then serve endpoints of many different types,
/// with a separate impl selected per route.
///
/// Implemented for `()`, which passes the request straight through, and for any
/// `Fn(Rq, DFnOnce<Rq, Rs>) -> impl Future<Output = Rs>`, which is the form to
/// reach for when a middleware does not change the types.
pub trait Middleware<Rq, Rs> {
    type Request;
    type Response;
    fn respond(
        &self,
        request: Self::Request,
        next: DFnOnce<Rq, Rs>,
    ) -> impl Future<Output = Self::Response> + Send;
}

/// Two middlewares run as one, `Outer` first.
///
/// This is what lets middlewares nest. `Outer` is handed the request and hands
/// its own output type inward to `Inner`, so `Chained` accepts what `Outer`
/// accepts and passes inward what `Inner` passes inward. Nesting a scope inside
/// another wraps the chain in a further `Chained` rather than replacing it,
/// which is why nested scopes keep the same outermost request and response
/// types however deep they go.
///
/// Built by [`Routes::middleware`](super::routes::Routes::middleware) and
/// [`Routes::scoped_middleware`](super::routes::Routes::scoped_middleware);
/// there is rarely a reason to name it directly.
pub struct Chained<Outer, Inner> {
    outer: Arc<Outer>,
    inner: Arc<Inner>,
}

impl<Outer, Inner> Chained<Outer, Inner> {
    pub fn new(outer: Arc<Outer>, inner: Arc<Inner>) -> Self {
        Self { outer, inner }
    }
}

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
