use super::*;
use actix::{Path, Quoter, ResourceDef};
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
    /// The query string. Parsed on first access, not on every request.
    pub query: query::Query,
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

struct RouteEntry<ORq, ORs> {
    rdef: ResourceDef,
    pattern: String,
    methods: Vec<&'static Method>,
    handler: DFn<RoutedRequest<ORq>, ORs>,
}

fn literal_prefix(pattern: &str) -> impl Iterator<Item = &str> {
    segments(pattern).take_while(|segment| !segment.contains(['{', '*']))
}

fn segments(path: &str) -> impl Iterator<Item = &str> {
    path.strip_prefix('/').unwrap_or(path).split('/')
}

#[derive(Default)]
struct PrefixIndex {
    routes: Vec<usize>,
    children: HashMap<String, PrefixIndex>,
}

impl PrefixIndex {
    fn build(patterns: &[String]) -> Self {
        let mut root = PrefixIndex::default();

        for (route, pattern) in patterns.iter().enumerate() {
            let mut node = &mut root;
            for segment in literal_prefix(pattern) {
                node = node.children.entry(segment.to_owned()).or_default();
            }
            node.routes.push(route);
        }

        root.push_down(&[]);
        root
    }

    fn push_down(&mut self, inherited: &[usize]) {
        self.routes.extend_from_slice(inherited);
        self.routes.sort_unstable();
        let inherited = self.routes.clone();
        for child in self.children.values_mut() {
            child.push_down(&inherited);
        }
    }

    fn candidates(&self, path: &str) -> &[usize] {
        let mut node = self;
        for segment in segments(path) {
            match node.children.get(segment) {
                Some(child) => node = child,
                None => break,
            }
        }
        &node.routes
    }
}

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

            let mut entries: Vec<RouteEntry<ORq, ORs>> = Vec::new();
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

                entries.push(RouteEntry {
                    rdef: ResourceDef::new(path.clone()),
                    pattern: path,
                    methods,
                    handler,
                });
            }

            let index = PrefixIndex::build(
                &entries
                    .iter()
                    .map(|entry| entry.pattern.clone())
                    .collect::<Vec<_>>(),
            );

            router::second::Router {
                entries,
                index,
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
        pub(super) entries: Vec<RouteEntry<ORq, ORs>>,
        pub(super) index: PrefixIndex,
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
            let mut rejected_pattern: Option<&str> = None;

            for &route in self.index.candidates(path.unprocessed()) {
                let entry = &self.entries[route];
                let mut path_matched = false;

                let matched = entry.rdef.capture_match_info_fn(path, |_| {
                    path_matched = true;
                    entry.methods.is_empty() || entry.methods.contains(&method)
                });

                if matched {
                    return (&entry.handler, Vec::new());
                }
                if path_matched && rejected_pattern.is_none() {
                    rejected_pattern = Some(&entry.pattern);
                }
            }

            let allowed_methods = rejected_pattern
                .and_then(|pattern| self.methods_by_pattern.get(pattern))
                .cloned()
                .unwrap_or_default();

            (&self.fallback_handler, allowed_methods)
        }
    }

    impl<ORq, ORs> Router<ORq, ORs>
    where
        ORq: Send + 'static,
        ORs: Send + 'static,
    {
        #[cfg(test)]
        pub(super) fn resolve_unindexed(
            &self,
            method: &Method,
            path: &mut Path<String>,
        ) -> (&DFn<RoutedRequest<ORq>, ORs>, Vec<&'static Method>) {
            let mut rejected_pattern: Option<&str> = None;

            for entry in &self.entries {
                let mut path_matched = false;
                let matched = entry.rdef.capture_match_info_fn(path, |_| {
                    path_matched = true;
                    entry.methods.is_empty() || entry.methods.contains(&method)
                });
                if matched {
                    return (&entry.handler, Vec::new());
                }
                if path_matched && rejected_pattern.is_none() {
                    rejected_pattern = Some(&entry.pattern);
                }
            }

            let allowed_methods = rejected_pattern
                .and_then(|pattern| self.methods_by_pattern.get(pattern))
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
            let query = query::Query::new(http_request_ref.uri().query());

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
            query: Default::default(),
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

#[cfg(test)]
mod index_tests {
    use super::*;

    const PATTERNS: &[&str] = &[
        "/api/notes/{id}",
        "/api/notes",
        "/api/{anything}",
        "/api/notes/{id}/comments/{comment}",
        "/{tenant}/dashboard",
        "/files/{path}*",
        "/post/{id:\\d+}",
        "/post/{slug}",
        "/file/{name}.{ext}",
        "/",
        "/deep/a/b/c/d/e/f/g",
        "/api",
    ];

    const PATHS: &[&str] = &[
        "/",
        "/api",
        "/api/",
        "/api/notes",
        "/api/notes/",
        "/api/notes/1",
        "/api/notes/1/comments/2",
        "/api/notes/1/comments",
        "/api/other",
        "/acme/dashboard",
        "/acme/dashboard/x",
        "/files/a/b/c",
        "/files/",
        "/post/12",
        "/post/hello",
        "/file/report.pdf",
        "/file/report",
        "/deep/a/b/c/d/e/f/g",
        "/deep/a/b/c/d/e/f",
        "/nothing/here",
        "",
        "/api/notes/1/",
        "/apix/notes",
        "/dashboard",
    ];

    fn router() -> second::Router<(), &'static str> {
        first::Router::with_fallback_handler(|_: RoutedRequest<()>| async { "fallback" })
            .and_routes(|mut r| {
                for pattern in PATTERNS {
                    r = r.route(
                        route::first::Route::with_method(&Method::GET)
                            .and_path(*pattern)
                            .and_handler(move |_: RoutedRequest<()>| async move { *pattern }),
                    );
                }
                r
            })
    }

    async fn run(
        router: &second::Router<(), &'static str>,
        indexed: bool,
        method: &Method,
        path: &str,
    ) -> (String, Vec<&'static Method>, &'static str) {
        let mut path = Path::new(requote_path(path));
        let (handler, allowed) = if indexed {
            router.resolve(method, &mut path)
        } else {
            router.resolve_unindexed(method, &mut path)
        };
        let response = handler(RoutedRequest {
            path: Path::new(String::new()),
            query: Default::default(),
            allowed_methods: Vec::new(),
            origin: (),
        })
        .await;
        let captured = format!("{:?}", path.iter().collect::<Vec<_>>());
        (captured, allowed, response)
    }

    #[tokio::test]
    async fn index_resolves_exactly_like_a_full_scan() {
        let router = router();

        for method in [&Method::GET, &Method::POST] {
            for path in PATHS {
                let indexed = run(&router, true, method, path).await;
                let scanned = run(&router, false, method, path).await;
                assert_eq!(
                    indexed, scanned,
                    "index disagrees with a full scan for {method} {path:?}"
                );
            }
        }
    }

    #[tokio::test]
    async fn a_shallow_route_registered_earlier_beats_a_deeper_later_one() {
        let router =
            first::Router::with_fallback_handler(|_: RoutedRequest<()>| async { "fallback" })
                .and_routes(|r| {
                    r.route(
                        route::first::Route::with_method(&Method::GET)
                            .and_path("/{tenant}/dashboard")
                            .and_handler(|_: RoutedRequest<()>| async { "dynamic" }),
                    )
                    .route(
                        route::first::Route::with_method(&Method::GET)
                            .and_path("/acme/dashboard")
                            .and_handler(|_: RoutedRequest<()>| async { "literal" }),
                    )
                });

        let (captured, _, response) = run(&router, true, &Method::GET, "/acme/dashboard").await;
        assert_eq!(response, "dynamic");
        assert_eq!(captured, r#"[("tenant", "acme")]"#);
    }
}
