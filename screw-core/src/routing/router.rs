use super::*;
use actix::{Path, Quoter, ResourceDef, Router as InnerRouter};
use hyper::Method;
use screw_components::dyn_fn::DFn;
use std::collections::{HashMap, HashSet};
use std::sync::LazyLock;

/// A request that has been through the router, carrying what the router learned
/// about it alongside the original.
pub struct RoutedRequest<ORq> {
    /// Path parameters captured by the matched pattern. Percent escapes are
    /// decoded except for `%2F`, `%25` and `%2B`, which stay encoded so that an
    /// escaped separator cannot be mistaken for a real one.
    pub path: Path<String>,
    /// The query string, parsed. Repeated keys keep the last value.
    pub query: HashMap<String, String>,
    /// The methods the matched path does accept, filled in only when no route
    /// matched. Non-empty means the path exists but the method is wrong -- a
    /// 405 with this as the `Allow` header -- and empty means no such path,
    /// which is a 404. Only the fallback handler ever sees this non-empty.
    pub allowed_methods: Vec<&'static Method>,
    /// The request as it arrived, untouched.
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
            let mut catch_all_patterns: HashSet<String> = HashSet::new();

            for (methods, path, handler) in routes.handlers() {
                if catch_all_patterns.contains(&path) {
                    panic!(
                        "route {path:?} is unreachable: an earlier route on the same pattern \
                         already accepts any method"
                    );
                }

                let pattern_methods = methods_by_pattern.entry(path.clone()).or_default();
                if methods.is_empty() {
                    catch_all_patterns.insert(path.clone());
                } else {
                    for method in &methods {
                        if pattern_methods.contains(method) {
                            panic!(
                                "route {method} {path:?} is registered twice: \
                                 only the first registration is reachable"
                            );
                        }
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
                .map(|v| form_urlencoded::parse(v.as_bytes()).into_owned().collect())
                .unwrap_or_default();

            let (handler, allowed_methods) = self.resolve(&method, &mut path);

            let request = RoutedRequest {
                path,
                query,
                allowed_methods,
                origin: request,
            };
            handler(request).await
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

    async fn resolve(
        router: &second::Router<(), &'static str>,
        method: &Method,
        path: &str,
    ) -> (Path<String>, Vec<&'static Method>, &'static str) {
        let mut path = Path::new(requote_path(path));
        let (handler, allowed_methods) = router.resolve(method, &mut path);
        let response = handler(RoutedRequest {
            path: Path::new(String::new()),
            query: HashMap::new(),
            allowed_methods: Vec::new(),
            origin: (),
        })
        .await;
        (path, allowed_methods, response)
    }

    #[tokio::test]
    async fn encoded_slash_does_not_split_path_segments() {
        let router = router_with(vec![(vec![&Method::GET], "/post/{id}")]);

        let (path, _, response) = resolve(&router, &Method::GET, "/post/a%2Fb").await;

        assert_eq!(response, "/post/{id}");
        assert_eq!(path.get("id"), Some("a%2Fb"));
    }

    #[tokio::test]
    async fn ordinary_percent_escapes_are_decoded() {
        let router = router_with(vec![(vec![&Method::GET], "/author/slug/{slug}")]);

        let (path, _, response) =
            resolve(&router, &Method::GET, "/author/slug/hello%20world").await;

        assert_eq!(response, "/author/slug/{slug}");
        assert_eq!(path.get("slug"), Some("hello world"));
    }

    #[tokio::test]
    async fn malformed_percent_escape_does_not_empty_the_path() {
        let router = router_with(vec![(vec![&Method::GET], "/post/{id}")]);

        let (path, _, response) = resolve(&router, &Method::GET, "/post/%zz").await;

        assert_eq!(response, "/post/{id}");
        assert_eq!(path.get("id"), Some("%zz"));
    }

    #[tokio::test]
    async fn non_utf8_percent_escape_does_not_empty_the_path() {
        let router = router_with(vec![(vec![&Method::GET], "/post/{id}")]);

        let (path, _, response) = resolve(&router, &Method::GET, "/post/%FF%FE").await;

        assert_eq!(response, "/post/{id}");
        assert!(!path.as_str().is_empty());
    }

    #[tokio::test]
    async fn method_mismatch_reports_allowed_methods() {
        let router = router_with(vec![
            (vec![&Method::GET], "/post/{id}"),
            (vec![&Method::PATCH, &Method::DELETE], "/post/{id}"),
        ]);

        let (_, allowed_methods, response) = resolve(&router, &Method::POST, "/post/1").await;

        assert_eq!(response, "fallback");
        assert_eq!(
            allowed_methods,
            vec![&Method::GET, &Method::PATCH, &Method::DELETE]
        );
    }

    #[tokio::test]
    async fn unknown_path_reports_no_allowed_methods() {
        let router = router_with(vec![(vec![&Method::GET], "/post/{id}")]);

        let (_, allowed_methods, response) = resolve(&router, &Method::GET, "/nothing/here").await;

        assert_eq!(response, "fallback");
        assert!(allowed_methods.is_empty());
    }

    #[tokio::test]
    async fn any_method_route_accepts_every_method() {
        let router = router_with(vec![(vec![], "/any")]);

        for method in [&Method::GET, &Method::POST, &Method::DELETE] {
            let (_, allowed_methods, response) = resolve(&router, method, "/any").await;
            assert_eq!(response, "/any");
            assert!(allowed_methods.is_empty());
        }
    }

    #[tokio::test]
    async fn earliest_registered_of_two_overlapping_patterns_wins() {
        let router =
            first::Router::with_fallback_handler(|_: RoutedRequest<()>| async { "fallback" })
                .and_routes(|r| {
                    r.route(
                        route::first::Route::with_method(&Method::GET)
                            .and_path("/api/{id}")
                            .and_handler(|_: RoutedRequest<()>| async { "first" }),
                    )
                    .route(
                        route::first::Route::with_method(&Method::GET)
                            .and_path("/api/x")
                            .and_handler(|_: RoutedRequest<()>| async { "second" }),
                    )
                });

        let (_, _, response) = resolve(&router, &Method::GET, "/api/x").await;
        assert_eq!(response, "first");
    }

    #[test]
    #[should_panic(expected = "registered twice")]
    fn same_pattern_and_method_twice_panics() {
        router_with(vec![
            (vec![&Method::GET], "/api/x"),
            (vec![&Method::GET], "/api/x"),
        ]);
    }

    #[test]
    #[should_panic(expected = "registered twice")]
    fn partially_overlapping_methods_on_one_pattern_panic() {
        router_with(vec![
            (vec![&Method::GET], "/api/x"),
            (vec![&Method::POST, &Method::GET], "/api/x"),
        ]);
    }

    #[test]
    #[should_panic(expected = "already accepts any method")]
    fn route_after_a_catch_all_on_the_same_pattern_panics() {
        router_with(vec![(vec![], "/api/x"), (vec![&Method::GET], "/api/x")]);
    }

    #[test]
    fn distinct_methods_on_one_pattern_are_allowed() {
        router_with(vec![
            (vec![&Method::GET], "/api/x"),
            (vec![&Method::POST, &Method::DELETE], "/api/x"),
        ]);
    }

    #[test]
    fn a_catch_all_after_specific_methods_is_allowed() {
        router_with(vec![(vec![&Method::GET], "/api/x"), (vec![], "/api/x")]);
    }

    #[tokio::test]
    async fn a_catch_all_after_specific_methods_only_serves_the_rest() {
        let router =
            first::Router::with_fallback_handler(|_: RoutedRequest<()>| async { "fallback" })
                .and_routes(|r| {
                    r.route(
                        route::first::Route::with_method(&Method::GET)
                            .and_path("/api/x")
                            .and_handler(|_: RoutedRequest<()>| async { "specific" }),
                    )
                    .route(
                        route::first::Route::with_any_method()
                            .and_path("/api/x")
                            .and_handler(|_: RoutedRequest<()>| async { "catch-all" }),
                    )
                });

        let (_, _, response) = resolve(&router, &Method::GET, "/api/x").await;
        assert_eq!(response, "specific");

        let (_, _, response) = resolve(&router, &Method::POST, "/api/x").await;
        assert_eq!(response, "catch-all");
    }
}
