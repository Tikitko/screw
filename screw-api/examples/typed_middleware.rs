//! Middlewares that change the request type as the chain nests.
//!
//! `JsonApiMiddlewareConverter` turns a raw request into `ApiRequest`, `Auth`
//! turns that into `Authed`, and `RequireAdmin` turns that into `AdminOnly`.
//! Each handler's signature therefore states exactly how much checking has
//! already happened — an `AdminOnly` handler cannot be routed without both
//! middlewares above it.
//!
//! The two handlers are written in the two available styles: `stats` names the
//! wrapper it is handed, while `profile` takes the pieces and answers with a
//! `Result`, letting `route` convert on both sides.
//!
//! ```sh
//! cargo run -p screw-api --example typed_middleware --features json
//!
//! curl -s localhost:8080/api/profile -H 'Authorization: Bearer alice-token'
//! curl -s localhost:8080/api/profile                            # 401
//! curl -s localhost:8080/api/admin/stats -H 'Authorization: Bearer root-token'
//! curl -s localhost:8080/api/admin/stats -H 'Authorization: Bearer alice-token'  # 403
//! ```

use hyper::{Method, StatusCode};
use hyper_util::rt::TokioIo;
use screw_api::json::JsonApiMiddlewareConverter;
use screw_api::request::{ApiRequest, ApiRequestContent, ApiRequestOriginContent};
use screw_api::response::{
    ApiResponse, ApiResponseContentBase, ApiResponseContentFailure, ApiResponseContentSuccess,
};
use screw_components::dyn_fn::DFnOnce;
use screw_core::request::Request;
use screw_core::responder_factory::ResponderFactory;
use screw_core::response::Response;
use screw_core::routing::middleware::Middleware;
use screw_core::routing::route::first::Route;
use screw_core::routing::router::{self, RoutedRequest};
use screw_core::server::ServerService;
use serde::Serialize;
use std::marker::PhantomData;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::TcpListener;

// ---------------------------------------------------------------- state

#[derive(Clone)]
struct User {
    name: &'static str,
    is_admin: bool,
}

struct Extensions;

impl Extensions {
    /// Stands in for a real session store.
    fn user_for_token(&self, token: &str) -> Option<User> {
        match token {
            "alice-token" => Some(User {
                name: "alice",
                is_admin: false,
            }),
            "root-token" => Some(User {
                name: "root",
                is_admin: true,
            }),
            _ => None,
        }
    }
}

// ---------------------------------------------------------------- requests

/// The middlewares below are generic over the request content, so anything they
/// need from it has to be spelled out as a trait.
trait Authorizable {
    fn bearer_token(&self) -> Option<&str>;
    fn extensions(&self) -> &Arc<Extensions>;
}

struct PlainContent {
    extensions: Arc<Extensions>,
    token: Option<String>,
}

/// Generic over the failure type, because both endpoints below share this
/// content and it never refuses a request itself.
impl<Failure> ApiRequestContent<Extensions, Failure> for PlainContent
where
    Failure: ApiResponseContentFailure,
{
    type Data = ();
    fn create(origin: ApiRequestOriginContent<Self::Data, Extensions>) -> Result<Self, Failure> {
        let token = origin
            .http_parts
            .headers
            .get(hyper::header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.strip_prefix("Bearer "))
            .map(str::to_owned);
        Ok(Self {
            extensions: origin.extensions,
            token,
        })
    }
}

impl Authorizable for PlainContent {
    fn bearer_token(&self) -> Option<&str> {
        self.token.as_deref()
    }
    fn extensions(&self) -> &Arc<Extensions> {
        &self.extensions
    }
}

/// What an authenticated handler receives. `Extensions` only shows up in a
/// `PhantomData` here, but it has to show up somewhere: the middleware impl
/// below is generic over it, and Rust requires every impl parameter to appear
/// in the trait or the self type.
struct Authed<Content, Extensions> {
    content: Content,
    user: User,
    _p_e: PhantomData<Extensions>,
}

/// Lets a handler take the parts instead of the wrapper. [`Routes::route`]
/// applies this between the middleware and the handler, which is why `profile`
/// below can destructure the pair right in its signature — no middleware of its
/// own, and `Authed` never appears in the handler's types.
///
/// [`Routes::route`]: screw_core::routing::routes::Routes::route
impl<Content, Extensions> From<Authed<Content, Extensions>> for (User, Content) {
    fn from(value: Authed<Content, Extensions>) -> Self {
        (value.user, value.content)
    }
}

/// What an admin-only handler receives.
struct AdminOnly<Content, Extensions> {
    authed: Authed<Content, Extensions>,
}

// ---------------------------------------------------------------- middlewares

/// A middleware that rejects a request has to build a failure of the handler's
/// own failure type, which it does not know. This trait is how a handler
/// declares that its failure type can express these two cases.
trait AuthFailure {
    fn unauthorized() -> Self;
    fn forbidden() -> Self;
}

struct Auth;

impl<Content, Extensions, Success, Failure>
    Middleware<Authed<Content, Extensions>, ApiResponse<Success, Failure>> for Auth
where
    Content: ApiRequestContent<Extensions, Failure> + Authorizable + Send + 'static,
    Extensions: Send + Sync + 'static,
    Success: ApiResponseContentSuccess + Send + 'static,
    Failure: ApiResponseContentFailure + AuthFailure + Send + 'static,
{
    type Request = ApiRequest<Content, Extensions>;
    type Response = ApiResponse<Success, Failure>;

    async fn respond(
        &self,
        request: ApiRequest<Content, Extensions>,
        next: DFnOnce<Authed<Content, Extensions>, ApiResponse<Success, Failure>>,
    ) -> ApiResponse<Success, Failure> {
        let content = request.content;

        let user = content
            .bearer_token()
            .and_then(|token| content.extensions().user_for_token(token));

        // Short-circuit: `next` is simply never called.
        let Some(user) = user else {
            return ApiResponse::failure(Failure::unauthorized());
        };

        next(Authed {
            content,
            user,
            _p_e: PhantomData,
        })
        .await
    }
}

struct RequireAdmin;

impl<Content, Extensions, Success, Failure>
    Middleware<AdminOnly<Content, Extensions>, ApiResponse<Success, Failure>> for RequireAdmin
where
    Content: Send + 'static,
    Extensions: Send + Sync + 'static,
    Success: ApiResponseContentSuccess + Send + 'static,
    Failure: ApiResponseContentFailure + AuthFailure + Send + 'static,
{
    type Request = Authed<Content, Extensions>;
    type Response = ApiResponse<Success, Failure>;

    async fn respond(
        &self,
        request: Authed<Content, Extensions>,
        next: DFnOnce<AdminOnly<Content, Extensions>, ApiResponse<Success, Failure>>,
    ) -> ApiResponse<Success, Failure> {
        if !request.user.is_admin {
            return ApiResponse::failure(Failure::forbidden());
        }
        next(AdminOnly { authed: request }).await
    }
}

// ---------------------------------------------------------------- responses

#[derive(Serialize)]
struct Profile {
    name: &'static str,
    is_admin: bool,
}

enum ProfileSuccess {
    Profile(Profile),
}

impl ApiResponseContentBase for ProfileSuccess {
    fn status_code(&self) -> StatusCode {
        StatusCode::OK
    }
}

impl ApiResponseContentSuccess for ProfileSuccess {
    type Data = Profile;
    fn identifier(&self) -> &'static str {
        "PROFILE"
    }
    fn description(&self) -> Option<String> {
        None
    }
    fn data(&self) -> &Self::Data {
        let Self::Profile(profile) = self;
        profile
    }
}

#[derive(Serialize)]
struct Stats {
    users: usize,
}

enum StatsSuccess {
    Stats(Stats),
}

impl ApiResponseContentBase for StatsSuccess {
    fn status_code(&self) -> StatusCode {
        StatusCode::OK
    }
}

impl ApiResponseContentSuccess for StatsSuccess {
    type Data = Stats;
    fn identifier(&self) -> &'static str {
        "STATS"
    }
    fn description(&self) -> Option<String> {
        None
    }
    fn data(&self) -> &Self::Data {
        let Self::Stats(stats) = self;
        stats
    }
}

/// One failure type shared by both endpoints. Implementing `AuthFailure` is
/// what makes it usable under `Auth` and `RequireAdmin`.
enum AccessFailure {
    Unauthorized,
    Forbidden,
}

impl ApiResponseContentBase for AccessFailure {
    fn status_code(&self) -> StatusCode {
        match self {
            Self::Unauthorized => StatusCode::UNAUTHORIZED,
            Self::Forbidden => StatusCode::FORBIDDEN,
        }
    }
}

impl ApiResponseContentFailure for AccessFailure {
    fn identifier(&self) -> &'static str {
        match self {
            Self::Unauthorized => "UNAUTHORIZED",
            Self::Forbidden => "FORBIDDEN",
        }
    }
    fn reason(&self) -> Option<String> {
        match self {
            Self::Unauthorized => Some("Send a valid Bearer token".to_owned()),
            Self::Forbidden => Some("Admin rights required".to_owned()),
        }
    }
}

impl AuthFailure for AccessFailure {
    fn unauthorized() -> Self {
        Self::Unauthorized
    }
    fn forbidden() -> Self {
        Self::Forbidden
    }
}

// ---------------------------------------------------------------- handlers

/// Neither `Authed` nor `ApiResponse` is named here. The request arrives as a
/// pair through the `From` impl above, and the `Result` becomes an
/// `ApiResponse` through the one that ships with [`ApiResponse`] — an `Err` is
/// a failure response, not a panic, so `?` works on the endpoint's own failure
/// type.
async fn profile((user, _content): (User, PlainContent)) -> Result<ProfileSuccess, AccessFailure> {
    // Reaching this body at all means `Auth` let the request through.
    Ok(ProfileSuccess::Profile(Profile {
        name: user.name,
        is_admin: user.is_admin,
    }))
}

/// The same endpoint spelled the other way, naming what the middleware hands it.
async fn stats(
    request: AdminOnly<PlainContent, Extensions>,
) -> ApiResponse<StatsSuccess, AccessFailure> {
    let _token = request.authed.content.bearer_token().map(str::to_owned);
    ApiResponse::success(StatsSuccess::Stats(Stats { users: 2 }))
}

async fn fallback(_: RoutedRequest<Request<Extensions>>) -> Response {
    Response {
        http: hyper::Response::builder()
            .status(StatusCode::NOT_FOUND)
            .body(screw_core::body::empty())
            .unwrap(),
    }
}

// ---------------------------------------------------------------- server

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let router = router::first::Router::with_fallback_handler(fallback).and_routes(|r| {
        r.scoped_middleware("/api", JsonApiMiddlewareConverter::default(), |r| {
            r.middleware(Auth, |r| {
                r.route(
                    Route::with_method(&Method::GET)
                        .and_path("/profile")
                        .and_handler(profile),
                )
                .scoped_middleware("/admin", RequireAdmin, |r| {
                    r.route(
                        Route::with_method(&Method::GET)
                            .and_path("/stats")
                            .and_handler(stats),
                    )
                })
            })
        })
    });

    let responder_factory = ResponderFactory::with_router(router).and_extensions(Extensions);
    let server_service = Arc::new(ServerService::with_responder_factory(responder_factory));

    let addr = SocketAddr::from(([127, 0, 0, 1], 8080));
    let listener = TcpListener::bind(addr).await?;
    println!("listening on http://{addr}");

    loop {
        let (stream, remote_addr) = listener.accept().await?;
        let session_service = server_service.make_session_service(remote_addr);
        tokio::task::spawn(async move {
            if let Err(error) = hyper::server::conn::http1::Builder::new()
                .serve_connection(TokioIo::new(stream), session_service)
                .await
            {
                eprintln!("connection error: {error}");
            }
        });
    }
}
