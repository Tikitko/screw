use super::*;
use actix::{Path, ResourceDef, Router as InnerRouter};
use std::collections::HashMap;

pub struct RoutedRequest<ORq> {
    pub path: Path<String>,
    pub query: HashMap<String, String>,
    pub origin: ORq,
}

pub mod first {
    use super::*;
    use screw_components::dyn_fn::{AsDynFn, DFn};
    use std::future::Future;

    pub struct Router<ORq, ORs>
    where
        ORq: Send + 'static,
        ORs: Send + 'static,
    {
        fallback_handler: DFn<RoutedRequest<ORq>, ORs>,
    }

    impl<ORq, ORs> Router<ORq, ORs>
    where
        ORq: Send + 'static,
        ORs: Send + 'static,
    {
        pub fn with_fallback_handler<HFn, HFut>(fallback_handler: HFn) -> Self
        where
            HFn: Fn(RoutedRequest<ORq>) -> HFut + Send + Sync + 'static,
            HFut: Future<Output = ORs> + Send + 'static,
        {
            Router {
                fallback_handler: fallback_handler.to_dyn_fn(),
            }
        }

        pub fn and_routes<F>(self, handler: F) -> router::second::Router<ORq, ORs>
        where
            F: FnOnce(
                routes::Routes<RoutedRequest<ORq>, ORs, ()>,
            ) -> routes::Routes<RoutedRequest<ORq>, ORs, ()>,
        {
            let routes = handler(routes::Routes::new());
            router::second::Router {
                inner: {
                    let mut inner_router = InnerRouter::build();
                    for (methods, path, handler) in routes.handlers() {
                        inner_router.push(ResourceDef::new(path), handler, methods);
                    }
                    inner_router.finish()
                },
                fallback_handler: self.fallback_handler,
            }
        }
    }
}

pub mod second {
    use super::*;
    use hyper::body::Incoming;
    use hyper::{Method, Request};
    use screw_components::dyn_fn::DFn;

    pub struct Router<ORq, ORs>
    where
        ORq: Send + 'static,
        ORs: Send + 'static,
    {
        pub(super) inner: InnerRouter<DFn<RoutedRequest<ORq>, ORs>, Vec<&'static Method>>,
        pub(super) fallback_handler: DFn<RoutedRequest<ORq>, ORs>,
    }

    impl<ORq, ORs> Router<ORq, ORs>
    where
        ORq: AsRef<Request<Incoming>> + Send + 'static,
        ORs: Send + 'static,
    {
        pub async fn process(&self, request: ORq) -> ORs {
            let http_request_ref = request.as_ref();

            let method = http_request_ref.method();
            let mut path = Path::new(
                urlencoding::decode(http_request_ref.uri().path())
                    .unwrap_or_default()
                    .into_owned(),
            );
            let query = http_request_ref
                .uri()
                .query()
                .map(|v| {
                    url::form_urlencoded::parse(v.as_bytes())
                        .into_owned()
                        .collect()
                })
                .unwrap_or_default();

            let handler = self
                .inner
                .recognize_fn(&mut path, |_, m| {
                    if !m.is_empty() {
                        m.contains(&method)
                    } else {
                        true
                    }
                })
                .map(|r| r.0)
                .unwrap_or(&self.fallback_handler);

            let request = RoutedRequest {
                path,
                query,
                origin: request,
            };
            let response = handler(request).await;
            response
        }
    }
}
