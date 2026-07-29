use super::*;
use hyper::Method;
use screw_components::dyn_fn::{DFn, DFnOnce};
use std::future::Future;
use std::sync::Arc;

/// The set of routes being built, and the middleware chain they will sit under.
///
/// `ORq` and `ORs` are the types the router itself deals in and stay fixed for
/// the whole tree; `M` is the middleware chain accumulated so far, and it is
/// what grows as scopes nest. Each [`route`](Self::route) call resolves the
/// chain against that route's own handler types, which is why two endpoints
/// under the same middleware may take entirely different request types.
pub struct Routes<ORq, ORs, M>
where
    ORq: Send + 'static,
    ORs: Send + 'static,
    M: Send + Sync + 'static,
{
    scope_path: String,
    middleware: Arc<M>,
    handlers: Vec<(Vec<&'static Method>, String, DFn<ORq, ORs>)>,
}

impl<ORq, ORs> Routes<ORq, ORs, ()>
where
    ORq: Send + 'static,
    ORs: Send + 'static,
{
    pub(super) fn new() -> Self {
        Self {
            scope_path: "".to_owned(),
            middleware: Arc::new(()),
            handlers: Vec::new(),
        }
    }
    pub(super) fn handlers(self) -> Vec<(Vec<&'static Method>, String, DFn<ORq, ORs>)> {
        self.handlers
    }
}

impl<ORq, ORs, M> Routes<ORq, ORs, M>
where
    ORq: Send + 'static,
    ORs: Send + 'static,
    M: Send + Sync + 'static,
{
    /// Registers routes under a path prefix, leaving the middleware chain alone.
    pub fn scoped<F>(self, scope_path: &'static str, handler: F) -> Self
    where
        F: FnOnce(Routes<ORq, ORs, M>) -> Routes<ORq, ORs, M>,
    {
        let Self { handlers, .. } = handler(Self {
            scope_path: self.scope_path.clone() + scope_path,
            middleware: self.middleware.clone(),
            handlers: self.handlers,
        });
        Self {
            scope_path: self.scope_path,
            middleware: self.middleware,
            handlers,
        }
    }

    /// Registers routes under a path prefix and an extra middleware.
    ///
    /// `middleware` runs innermost, after everything already in the chain. The
    /// routes inside see a [`Chained`](middleware::Chained) built from the two,
    /// so scopes can nest arbitrarily deep and each level may change the
    /// request and response types.
    pub fn scoped_middleware<NM, F>(
        self,
        scope_path: &'static str,
        middleware: NM,
        handler: F,
    ) -> Self
    where
        NM: Send + Sync + 'static,
        F: FnOnce(
            Routes<ORq, ORs, middleware::Chained<M, NM>>,
        ) -> Routes<ORq, ORs, middleware::Chained<M, NM>>,
    {
        let Self {
            scope_path: outer_scope_path,
            middleware: outer_middleware,
            handlers,
        } = self;
        let Routes { handlers, .. } = handler(Routes {
            scope_path: outer_scope_path.clone() + scope_path,
            middleware: Arc::new(middleware::Chained::new(
                outer_middleware.clone(),
                Arc::new(middleware),
            )),
            handlers,
        });
        Self {
            scope_path: outer_scope_path,
            middleware: outer_middleware,
            handlers,
        }
    }

    /// [`scoped_middleware`](Self::scoped_middleware) without a path prefix.
    pub fn middleware<NM, F>(self, middleware: NM, handler: F) -> Self
    where
        NM: Send + Sync + 'static,
        F: FnOnce(
            Routes<ORq, ORs, middleware::Chained<M, NM>>,
        ) -> Routes<ORq, ORs, middleware::Chained<M, NM>>,
    {
        self.scoped_middleware("", middleware, handler)
    }

    /// Registers one route, binding the middleware chain to this handler.
    ///
    /// This is where the chain is resolved: the whole of `M` must convert the
    /// router's `ORq`/`ORs` into whatever this handler takes and returns. The
    /// last step across that gap is a plain conversion, `FRq: From<Rq>` and
    /// `IRs: Into<Rs>`, so a handler can take a newtype over what the innermost
    /// middleware produces without a middleware of its own.
    ///
    /// The chain is baked into the handler here, at registration. A request
    /// that reaches this route cannot arrive without having gone through it.
    pub fn route<FRq, Rq, IRs, Rs, HFn, HFut>(
        self,
        route: route::third::Route<FRq, IRs, HFn, HFut>,
    ) -> Self
    where
        M: middleware::Middleware<Rq, Rs, Request = ORq, Response = ORs>,
        FRq: From<Rq> + Send + 'static,
        Rq: Send + 'static,
        IRs: Into<Rs> + Send + 'static,
        Rs: Send + 'static,
        HFn: Fn(FRq) -> HFut + Send + Sync + 'static,
        HFut: Future<Output = IRs> + Send + 'static,
    {
        let Self {
            scope_path,
            middleware,
            mut handlers,
        } = self;

        let path = scope_path.clone() + route.path.as_str();
        let handler = Arc::new(route.handler);
        let route_middleware = middleware.clone();

        handlers.push((
            route.methods,
            path,
            Box::new(move |request| {
                let handler = handler.clone();
                let middleware = route_middleware.clone();
                Box::pin(async move {
                    let next: DFnOnce<Rq, Rs> = Box::new(move |rq| {
                        Box::pin(async move { handler(From::from(rq)).await.into() })
                    });
                    middleware.respond(request, next).await
                })
            }),
        ));

        Self {
            scope_path,
            middleware,
            handlers,
        }
    }
}
