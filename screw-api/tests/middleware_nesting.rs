use hyper::StatusCode;
use screw_api::json::JsonApiMiddlewareConverter;
use screw_api::request::{ApiRequest, ApiRequestContent, ApiRequestOriginContent};
use screw_api::response::{
    ApiResponse, ApiResponseContentBase, ApiResponseContentFailure, ApiResponseContentSuccess,
};
use screw_components::dyn_fn::DFnOnce;
use screw_core::request::Request;
use screw_core::response::Response;
use screw_core::routing::middleware::Middleware;
use screw_core::routing::route;
use screw_core::routing::router::{self, RoutedRequest};

struct Extensions;

// `ContentA` is generic over the failure type and `ContentB` names one, on
// purpose: routing has to resolve `Data` through either kind of impl.
struct ContentA;
impl<Failure> ApiRequestContent<Extensions, Failure> for ContentA
where
    Failure: ApiResponseContentFailure,
{
    type Data = ();
    async fn create(_: ApiRequestOriginContent<Self::Data, Extensions>) -> Result<Self, Failure> {
        Ok(ContentA)
    }
}

struct ContentB;
impl ApiRequestContent<Extensions, FailureB> for ContentB {
    type Data = ();
    async fn create(_: ApiRequestOriginContent<Self::Data, Extensions>) -> Result<Self, FailureB> {
        Ok(ContentB)
    }
}

macro_rules! success {
    ($name:ident, $id:literal) => {
        struct $name;
        impl ApiResponseContentBase for $name {
            fn status_code(&self) -> StatusCode {
                StatusCode::OK
            }
        }
        impl ApiResponseContentSuccess for $name {
            type Data = ();
            fn identifier(&self) -> &'static str {
                $id
            }
            fn description(&self) -> Option<String> {
                None
            }
            fn data(&self) -> &Self::Data {
                &()
            }
        }
    };
}

macro_rules! failure {
    ($name:ident, $id:literal) => {
        struct $name;
        impl ApiResponseContentBase for $name {
            fn status_code(&self) -> StatusCode {
                StatusCode::BAD_REQUEST
            }
        }
        impl ApiResponseContentFailure for $name {
            fn identifier(&self) -> &'static str {
                $id
            }
            fn reason(&self) -> Option<String> {
                None
            }
        }
    };
}

success!(SuccessA, "A");
success!(SuccessB, "B");
failure!(FailureA, "A_ERR");
failure!(FailureB, "B_ERR");

struct Authed<Content, Extensions> {
    #[allow(dead_code)]
    request: ApiRequest<Content, Extensions>,
}

struct Auth;

impl<Content, Ext, Success, Failure> Middleware<Authed<Content, Ext>, ApiResponse<Success, Failure>>
    for Auth
where
    Content: ApiRequestContent<Ext, Failure> + Send + 'static,
    Ext: Send + Sync + 'static,
    Success: ApiResponseContentSuccess + Send + 'static,
    Failure: ApiResponseContentFailure + Send + 'static,
{
    type Request = ApiRequest<Content, Ext>;
    type Response = ApiResponse<Success, Failure>;
    async fn respond(
        &self,
        request: ApiRequest<Content, Ext>,
        next: DFnOnce<Authed<Content, Ext>, ApiResponse<Success, Failure>>,
    ) -> ApiResponse<Success, Failure> {
        next(Authed { request }).await
    }
}

async fn handler_a(_: Authed<ContentA, Extensions>) -> ApiResponse<SuccessA, FailureA> {
    ApiResponse::success(SuccessA)
}

async fn handler_b(_: Authed<ContentB, Extensions>) -> ApiResponse<SuccessB, FailureB> {
    ApiResponse::success(SuccessB)
}

async fn fallback(_: RoutedRequest<Request<Extensions>>) -> Response {
    Response {
        http: hyper::Response::builder()
            .status(StatusCode::NOT_FOUND)
            .body(screw_core::body::empty())
            .unwrap(),
    }
}

#[test]
fn single_endpoint_under_nested_middleware() {
    let _router = router::first::Router::with_fallback_handler(fallback).and_routes(|r| {
        r.scoped_middleware("/api", JsonApiMiddlewareConverter::default(), |r| {
            r.middleware(Auth, |r| {
                r.route(
                    route::first::Route::with_method(&hyper::Method::GET)
                        .and_path("/a")
                        .and_handler(handler_a),
                )
            })
        })
    });
}

#[test]
fn two_endpoints_under_nested_middleware() {
    let _router = router::first::Router::with_fallback_handler(fallback).and_routes(|r| {
        r.scoped_middleware("/api", JsonApiMiddlewareConverter::default(), |r| {
            r.middleware(Auth, |r| {
                r.route(
                    route::first::Route::with_method(&hyper::Method::GET)
                        .and_path("/a")
                        .and_handler(handler_a),
                )
                .route(
                    route::first::Route::with_method(&hyper::Method::GET)
                        .and_path("/b")
                        .and_handler(handler_b),
                )
            })
        })
    });
}

#[test]
fn two_endpoints_directly_under_json() {
    async fn plain_a(_: ApiRequest<ContentA, Extensions>) -> ApiResponse<SuccessA, FailureA> {
        ApiResponse::success(SuccessA)
    }
    async fn plain_b(_: ApiRequest<ContentB, Extensions>) -> ApiResponse<SuccessB, FailureB> {
        ApiResponse::success(SuccessB)
    }

    let _router = router::first::Router::with_fallback_handler(fallback).and_routes(|r| {
        r.scoped_middleware("/api", JsonApiMiddlewareConverter::default(), |r| {
            r.route(
                route::first::Route::with_method(&hyper::Method::GET)
                    .and_path("/a")
                    .and_handler(plain_a),
            )
            .route(
                route::first::Route::with_method(&hyper::Method::GET)
                    .and_path("/b")
                    .and_handler(plain_b),
            )
        })
    });
}
