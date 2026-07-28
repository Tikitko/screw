use super::*;
use actix::{Path, Quoter, ResourceDef, Router as InnerRouter};
use hyper::Method;
use screw_components::dyn_fn::DFn;
use std::collections::HashMap;
use std::sync::LazyLock;

pub struct RoutedRequest<ORq> {
    pub path: Path<String>,
    pub query: HashMap<String, String>,
    pub allowed_methods: Vec<&'static Method>,
    pub origin: ORq,
}

fn requote_path(path: &str) -> String {
    static PATH_QUOTER: LazyLock<Quoter> = LazyLock::new(|| Quoter::new(b"", b"%/+"));
    match PATH_QUOTER.requote(path.as_bytes()) {
        Some(requoted) => String::from_utf8_lossy(&requoted).into_owned(),
        None => path.to_owned(),
    }
}

type RouteEntry<ORq, ORs> = (String, DFn<RoutedRequest<ORq>, ORs>);

pub mod first {
    use super::*;
    use screw_components::dyn_fn::AsDynFn;
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

            let mut inner_router = InnerRouter::build();
            let mut methods_by_pattern: HashMap<String, Vec<&'static Method>> = HashMap::new();

            for (methods, path, handler) in routes.handlers() {
                let pattern_methods = methods_by_pattern.entry(path.clone()).or_default();
                for method in &methods {
                    if !pattern_methods.contains(method) {
                        pattern_methods.push(method);
                    }
                }
                inner_router.push(ResourceDef::new(path.clone()), (path, handler), methods);
            }

            router::second::Router {
                inner: inner_router.finish(),
                methods_by_pattern,
                fallback_handler: self.fallback_handler,
            }
        }
    }
}

pub mod second {
    use super::*;
    use hyper::Request;
    use hyper::body::Incoming;

    pub struct Router<ORq, ORs>
    where
        ORq: Send + 'static,
        ORs: Send + 'static,
    {
        pub(super) inner: InnerRouter<RouteEntry<ORq, ORs>, Vec<&'static Method>>,
        pub(super) methods_by_pattern: HashMap<String, Vec<&'static Method>>,
        pub(super) fallback_handler: DFn<RoutedRequest<ORq>, ORs>,
    }

    impl<ORq, ORs> Router<ORq, ORs>
    where
        ORq: Send + 'static,
        ORs: Send + 'static,
    {
        pub(super) fn resolve(
            &self,
            method: &Method,
            path: &mut Path<String>,
        ) -> (&DFn<RoutedRequest<ORq>, ORs>, Vec<&'static Method>) {
            let matched = self.inner.recognize_fn(path, |_, methods| {
                methods.is_empty() || methods.contains(&method)
            });

            if let Some(((_, handler), _)) = matched {
                return (handler, Vec::new());
            }

            let mut probe_path = Path::new(path.as_str().to_owned());
            let allowed_methods = self
                .inner
                .recognize_fn(&mut probe_path, |_, _| true)
                .and_then(|((pattern, _), _)| self.methods_by_pattern.get(pattern))
                .cloned()
                .unwrap_or_default();

            (&self.fallback_handler, allowed_methods)
        }
    }

    impl<ORq, ORs> Router<ORq, ORs>
    where
        ORq: AsRef<Request<Incoming>> + Send + 'static,
        ORs: Send + 'static,
    {
        pub async fn process(&self, request: ORq) -> ORs {
            let http_request_ref = request.as_ref();

            let method = http_request_ref.method().clone();
            let mut path = Path::new(requote_path(http_request_ref.uri().path()));
            let query = http_request_ref
                .uri()
                .query()
                .map(|v| {
                    url::form_urlencoded::parse(v.as_bytes())
                        .into_owned()
                        .collect()
                })
                .unwrap_or_default();

            let (handler, allowed_methods) = self.resolve(&method, &mut path);

            let request = RoutedRequest {
                path,
                query,
                allowed_methods,
                origin: request,
            };
            let response = handler(request).await;
            response
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use screw_components::dyn_fn::DFuture;

    fn router_with(
        routes: Vec<(Vec<&'static Method>, &'static str)>,
    ) -> second::Router<(), &'static str> {
        first::Router::with_fallback_handler(|_: RoutedRequest<()>| async { "fallback" })
            .and_routes(|mut r| {
                for (methods, path) in routes {
                    r = r.route(
                        route::first::Route::with_methods(methods)
                            .and_path(path)
                            .and_handler(move |_: RoutedRequest<()>| async move { path }),
                    );
                }
                r
            })
    }

    fn resolve(
        router: &second::Router<(), &'static str>,
        method: &Method,
        path: &str,
    ) -> (Path<String>, Vec<&'static Method>, &'static str) {
        let mut path = Path::new(requote_path(path));
        let (handler, allowed_methods) = router.resolve(method, &mut path);
        let response = block_on(handler(RoutedRequest {
            path: Path::new(String::new()),
            query: HashMap::new(),
            allowed_methods: Vec::new(),
            origin: (),
        }));
        (path, allowed_methods, response)
    }

    fn block_on<T>(mut future: DFuture<T>) -> T {
        use std::pin::Pin;
        use std::task::{Context, Poll, RawWaker, RawWakerVTable, Waker};

        const VTABLE: RawWakerVTable = RawWakerVTable::new(
            |_| RawWaker::new(std::ptr::null(), &VTABLE),
            |_| {},
            |_| {},
            |_| {},
        );
        let waker = unsafe { Waker::from_raw(RawWaker::new(std::ptr::null(), &VTABLE)) };
        let mut context = Context::from_waker(&waker);
        match Pin::new(&mut future).poll(&mut context) {
            Poll::Ready(value) => value,
            Poll::Pending => panic!("test handler is not immediately ready"),
        }
    }

    #[test]
    fn encoded_slash_does_not_split_path_segments() {
        let router = router_with(vec![(vec![&Method::GET], "/post/{id}")]);

        let (path, _, response) = resolve(&router, &Method::GET, "/post/a%2Fb");

        assert_eq!(response, "/post/{id}");
        assert_eq!(path.get("id"), Some("a%2Fb"));
    }

    #[test]
    fn ordinary_percent_escapes_are_decoded() {
        let router = router_with(vec![(vec![&Method::GET], "/author/slug/{slug}")]);

        let (path, _, response) = resolve(&router, &Method::GET, "/author/slug/hello%20world");

        assert_eq!(response, "/author/slug/{slug}");
        assert_eq!(path.get("slug"), Some("hello world"));
    }

    #[test]
    fn malformed_percent_escape_does_not_empty_the_path() {
        let router = router_with(vec![(vec![&Method::GET], "/post/{id}")]);

        let (path, _, response) = resolve(&router, &Method::GET, "/post/%zz");

        assert_eq!(response, "/post/{id}");
        assert_eq!(path.get("id"), Some("%zz"));
    }

    #[test]
    fn non_utf8_percent_escape_does_not_empty_the_path() {
        let router = router_with(vec![(vec![&Method::GET], "/post/{id}")]);

        let (path, _, response) = resolve(&router, &Method::GET, "/post/%FF%FE");

        assert_eq!(response, "/post/{id}");
        assert!(!path.as_str().is_empty());
    }

    #[test]
    fn method_mismatch_reports_allowed_methods() {
        let router = router_with(vec![
            (vec![&Method::GET], "/post/{id}"),
            (vec![&Method::PATCH, &Method::DELETE], "/post/{id}"),
        ]);

        let (_, allowed_methods, response) = resolve(&router, &Method::POST, "/post/1");

        assert_eq!(response, "fallback");
        assert_eq!(
            allowed_methods,
            vec![&Method::GET, &Method::PATCH, &Method::DELETE]
        );
    }

    #[test]
    fn unknown_path_reports_no_allowed_methods() {
        let router = router_with(vec![(vec![&Method::GET], "/post/{id}")]);

        let (_, allowed_methods, response) = resolve(&router, &Method::GET, "/nothing/here");

        assert_eq!(response, "fallback");
        assert!(allowed_methods.is_empty());
    }

    #[test]
    fn any_method_route_accepts_every_method() {
        let router = router_with(vec![(vec![], "/any")]);

        for method in [&Method::GET, &Method::POST, &Method::DELETE] {
            let (_, allowed_methods, response) = resolve(&router, method, "/any");
            assert_eq!(response, "/any");
            assert!(allowed_methods.is_empty());
        }
    }

    #[test]
    fn earliest_registered_of_two_matching_patterns_wins() {
        let labelled = |label: &'static str, path: &'static str| {
            first::Router::with_fallback_handler(|_: RoutedRequest<()>| async { "fallback" })
                .and_routes(move |r| {
                    r.route(
                        route::first::Route::with_method(&Method::GET)
                            .and_path(path)
                            .and_handler(move |_: RoutedRequest<()>| async move { label }),
                    )
                    .route(
                        route::first::Route::with_method(&Method::GET)
                            .and_path("/api/x")
                            .and_handler(|_: RoutedRequest<()>| async { "second" }),
                    )
                })
        };

        let (_, _, response) = resolve(&labelled("first", "/api/x"), &Method::GET, "/api/x");
        assert_eq!(response, "first");

        let (_, _, response) = resolve(&labelled("first", "/api/{id}"), &Method::GET, "/api/x");
        assert_eq!(response, "first");
    }
}
