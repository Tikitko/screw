use hyper::Method;
use screw_components::dyn_fn::DFnOnce;
use screw_core::routing::actix::Path;
use screw_core::routing::middleware::{Chained, Middleware};
use screw_core::routing::route;
use screw_core::routing::router::{self, RoutedRequest};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

type Log = Arc<Mutex<Vec<&'static str>>>;

fn routed_request() -> RoutedRequest<()> {
    RoutedRequest {
        path: Path::new(String::new()),
        query: HashMap::new(),
        allowed_methods: Vec::new(),
        origin: (),
    }
}

struct Mid(String);
struct MidRs(String);

struct Outer(Log);

#[async_trait::async_trait]
impl Middleware<Mid, MidRs> for Outer {
    type Request = RoutedRequest<()>;
    type Response = String;
    async fn respond(&self, _request: RoutedRequest<()>, next: DFnOnce<Mid, MidRs>) -> String {
        self.0.lock().unwrap().push("outer:in");
        let response = next(Mid("root".to_owned())).await;
        self.0.lock().unwrap().push("outer:out");
        format!("outer({})", response.0)
    }
}

struct Leaf(String);
struct LeafRs(String);

struct Inner(Log);

#[async_trait::async_trait]
impl Middleware<Leaf, LeafRs> for Inner {
    type Request = Mid;
    type Response = MidRs;
    async fn respond(&self, request: Mid, next: DFnOnce<Leaf, LeafRs>) -> MidRs {
        self.0.lock().unwrap().push("inner:in");
        let response = next(Leaf(format!("{}+inner", request.0))).await;
        self.0.lock().unwrap().push("inner:out");
        MidRs(format!("inner({})", response.0))
    }
}

struct Twig(String);
struct TwigRs(String);

struct Deepest;

#[async_trait::async_trait]
impl Middleware<Twig, TwigRs> for Deepest {
    type Request = Leaf;
    type Response = LeafRs;
    async fn respond(&self, request: Leaf, next: DFnOnce<Twig, TwigRs>) -> LeafRs {
        LeafRs(next(Twig(request.0)).await.0)
    }
}

async fn leaf_handler(request: Leaf) -> LeafRs {
    LeafRs(format!("{}+handler", request.0))
}

async fn twig_handler(request: Twig) -> TwigRs {
    TwigRs(request.0)
}

#[test]
fn two_type_changing_middlewares_stack() {
    let log: Log = Default::default();
    let _router = router::first::Router::with_fallback_handler(|_: RoutedRequest<()>| async {
        String::new()
    })
    .and_routes(|r| {
        r.middleware(Outer(log.clone()), |r| {
            r.middleware(Inner(log.clone()), |r| {
                r.route(
                    route::first::Route::with_method(&Method::GET)
                        .and_path("/x")
                        .and_handler(leaf_handler),
                )
            })
        })
    });
}

#[test]
fn three_type_changing_middlewares_stack() {
    let log: Log = Default::default();
    let _router = router::first::Router::with_fallback_handler(|_: RoutedRequest<()>| async {
        String::new()
    })
    .and_routes(|r| {
        r.middleware(Outer(log.clone()), |r| {
            r.middleware(Inner(log.clone()), |r| {
                r.middleware(Deepest, |r| {
                    r.route(
                        route::first::Route::with_method(&Method::GET)
                            .and_path("/x")
                            .and_handler(twig_handler),
                    )
                })
            })
        })
    });
}

#[test]
fn function_middleware_stacks_on_a_type_changing_one() {
    async fn logging(request: Mid, next: DFnOnce<Mid, MidRs>) -> MidRs {
        next(request).await
    }
    async fn mid_handler(request: Mid) -> MidRs {
        MidRs(request.0)
    }

    let log: Log = Default::default();
    let _router = router::first::Router::with_fallback_handler(|_: RoutedRequest<()>| async {
        String::new()
    })
    .and_routes(|r| {
        r.middleware(Outer(log.clone()), |r| {
            r.middleware(logging, |r| {
                r.route(
                    route::first::Route::with_method(&Method::GET)
                        .and_path("/x")
                        .and_handler(mid_handler),
                )
            })
        })
    });
}

#[tokio::test]
async fn chain_runs_outermost_first_and_threads_values_both_ways() {
    let log: Log = Default::default();
    let chained = Chained::new(Arc::new(Outer(log.clone())), Arc::new(Inner(log.clone())));

    let next: DFnOnce<Leaf, LeafRs> =
        Box::new(|leaf| Box::pin(async move { leaf_handler(leaf).await }));
    let response = chained.respond(routed_request(), next).await;

    assert_eq!(response, "outer(inner(root+inner+handler))");
    assert_eq!(
        *log.lock().unwrap(),
        vec!["outer:in", "inner:in", "inner:out", "outer:out"]
    );
}
