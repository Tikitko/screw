//! A JSON API built with `JsonApiMiddlewareConverter`.
//!
//! The converter parses the request body into `ApiRequestContent::Data` and
//! serializes `ApiResponse` back out, so handlers only deal with their own
//! types.
//!
//! ```sh
//! cargo run -p screw-api --example json_api --features json
//!
//! curl -s localhost:8080/api/notes/1
//! curl -s -X POST localhost:8080/api/notes \
//!     -H 'Content-Type: application/json' -d '{"title":"first","body":"hi"}'
//! curl -s -X POST localhost:8080/api/notes \
//!     -H 'Content-Type: application/json' -d '{"title":"","body":"hi"}'
//! curl -s -X POST localhost:8080/api/notes -d 'not json'   # wrong Content-Type
//! ```

use hyper::{Method, StatusCode};
use hyper_util::rt::TokioIo;
use screw_api::json::JsonApiMiddlewareConverter;
use screw_api::request::{ApiRequest, ApiRequestContent, ApiRequestOriginContent};
use screw_api::response::{
    ApiResponse, ApiResponseContentBase, ApiResponseContentFailure, ApiResponseContentSuccess,
};
use screw_components::dyn_result::DResult;
use screw_core::request::Request;
use screw_core::responder_factory::ResponderFactory;
use screw_core::response::Response;
use screw_core::routing::route::first::Route;
use screw_core::routing::router::{self, RoutedRequest};
use screw_core::server::ServerService;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use tokio::net::TcpListener;

// ---------------------------------------------------------------- state

#[derive(Clone, Serialize)]
struct Note {
    id: u64,
    title: String,
    body: String,
}

/// Shared application state. It is handed out as `Arc<Extensions>`, so anything
/// mutable needs its own synchronization.
#[derive(Default)]
struct Extensions {
    notes: Mutex<HashMap<u64, Note>>,
    next_id: Mutex<u64>,
}

// ---------------------------------------------------------------- requests

/// `GET /api/notes/{id}` — no body, so `Data` is `()`.
struct ReadNoteContent {
    id: Option<u64>,
    extensions: Arc<Extensions>,
}

impl ApiRequestContent<Extensions> for ReadNoteContent {
    type Data = ();
    fn create(origin: ApiRequestOriginContent<Self::Data, Extensions>) -> Self {
        Self {
            id: origin.path.get("id").and_then(|id| id.parse().ok()),
            extensions: origin.extensions,
        }
    }
}

#[derive(Deserialize)]
struct CreateNoteData {
    title: String,
    body: String,
}

/// `POST /api/notes` — the parsed body arrives as a `DResult`, so a malformed
/// request is something the handler answers rather than a hard failure.
struct CreateNoteContent {
    data: DResult<CreateNoteData>,
    extensions: Arc<Extensions>,
}

impl ApiRequestContent<Extensions> for CreateNoteContent {
    type Data = CreateNoteData;
    fn create(origin: ApiRequestOriginContent<Self::Data, Extensions>) -> Self {
        Self {
            data: origin.data_result,
            extensions: origin.extensions,
        }
    }
}

// ---------------------------------------------------------------- responses

enum NoteSuccess {
    Found(Note),
    Created(Note),
}

impl ApiResponseContentBase for NoteSuccess {
    fn status_code(&self) -> StatusCode {
        match self {
            Self::Found(_) => StatusCode::OK,
            Self::Created(_) => StatusCode::CREATED,
        }
    }
}

impl ApiResponseContentSuccess for NoteSuccess {
    type Data = Note;
    fn identifier(&self) -> &'static str {
        match self {
            Self::Found(_) => "NOTE_FOUND",
            Self::Created(_) => "NOTE_CREATED",
        }
    }
    fn description(&self) -> Option<String> {
        match self {
            Self::Found(_) => None,
            Self::Created(note) => Some(format!("Note {} created", note.id)),
        }
    }
    fn data(&self) -> &Self::Data {
        match self {
            Self::Found(note) | Self::Created(note) => note,
        }
    }
}

enum NoteFailure {
    BadId,
    NotFound,
    MalformedBody(String),
    EmptyTitle,
}

impl ApiResponseContentBase for NoteFailure {
    fn status_code(&self) -> StatusCode {
        match self {
            Self::BadId | Self::MalformedBody(_) | Self::EmptyTitle => StatusCode::BAD_REQUEST,
            Self::NotFound => StatusCode::NOT_FOUND,
        }
    }
}

impl ApiResponseContentFailure for NoteFailure {
    fn identifier(&self) -> &'static str {
        match self {
            Self::BadId => "BAD_ID",
            Self::NotFound => "NOT_FOUND",
            Self::MalformedBody(_) => "MALFORMED_BODY",
            Self::EmptyTitle => "EMPTY_TITLE",
        }
    }
    fn reason(&self) -> Option<String> {
        match self {
            Self::BadId => Some("Note id must be a number".to_owned()),
            Self::NotFound => Some("No note with that id".to_owned()),
            Self::MalformedBody(error) => Some(error.clone()),
            Self::EmptyTitle => Some("Title must not be empty".to_owned()),
        }
    }
}

// ---------------------------------------------------------------- handlers

async fn read_note(
    request: ApiRequest<ReadNoteContent, Extensions>,
) -> ApiResponse<NoteSuccess, NoteFailure> {
    let content = request.content;

    let Some(id) = content.id else {
        return ApiResponse::failure(NoteFailure::BadId);
    };

    let note = content.extensions.notes.lock().unwrap().get(&id).cloned();

    // `ApiResponse` also converts from a `Result`, which is often tidier.
    note.map(NoteSuccess::Found)
        .ok_or(NoteFailure::NotFound)
        .into()
}

async fn create_note(
    request: ApiRequest<CreateNoteContent, Extensions>,
) -> ApiResponse<NoteSuccess, NoteFailure> {
    let content = request.content;

    let data = match content.data {
        Ok(data) => data,
        // Wrong `Content-Type`, oversized, or unparseable body all land here.
        Err(error) => return ApiResponse::failure(NoteFailure::MalformedBody(error.to_string())),
    };

    if data.title.trim().is_empty() {
        return ApiResponse::failure(NoteFailure::EmptyTitle);
    }

    let id = {
        let mut next_id = content.extensions.next_id.lock().unwrap();
        *next_id += 1;
        *next_id
    };
    let note = Note {
        id,
        title: data.title,
        body: data.body,
    };
    content
        .extensions
        .notes
        .lock()
        .unwrap()
        .insert(id, note.clone());

    ApiResponse::success(NoteSuccess::Created(note))
}

/// An endpoint that cannot fail uses `Infallible` as its failure type.
struct HealthSuccess;

impl ApiResponseContentBase for HealthSuccess {
    fn status_code(&self) -> StatusCode {
        StatusCode::OK
    }
}

impl ApiResponseContentSuccess for HealthSuccess {
    type Data = ();
    fn identifier(&self) -> &'static str {
        "OK"
    }
    fn description(&self) -> Option<String> {
        None
    }
    fn data(&self) -> &Self::Data {
        &()
    }
}

async fn health(_: ApiRequest<(), Extensions>) -> ApiResponse<HealthSuccess, Infallible> {
    ApiResponse::success(HealthSuccess)
}

async fn fallback(request: RoutedRequest<Request<Extensions>>) -> Response {
    let mut builder = hyper::Response::builder();
    if request.allowed_methods.is_empty() {
        builder = builder.status(StatusCode::NOT_FOUND);
    } else {
        let allow = request
            .allowed_methods
            .iter()
            .map(|method| method.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        builder = builder
            .status(StatusCode::METHOD_NOT_ALLOWED)
            .header(hyper::header::ALLOW, allow);
    }
    Response {
        http: builder.body(screw_core::body::empty()).unwrap(),
    }
}

// ---------------------------------------------------------------- server

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let router = router::first::Router::with_fallback_handler(fallback).and_routes(|r| {
        r.scoped_middleware(
            "/api",
            JsonApiMiddlewareConverter {
                pretty_printed: true,
                // Notes are small; refuse anything larger long before it is parsed.
                max_body_size: 64 * 1024,
            },
            |r| {
                r.route(
                    Route::with_method(&Method::GET)
                        .and_path("/health")
                        .and_handler(health),
                )
                .scoped("/notes", |r| {
                    r.route(
                        Route::with_method(&Method::GET)
                            .and_path("/{id}")
                            .and_handler(read_note),
                    )
                    .route(
                        Route::with_method(&Method::POST)
                            .and_path("")
                            .and_handler(create_note),
                    )
                })
            },
        )
    });

    let extensions = Extensions::default();
    extensions.notes.lock().unwrap().insert(
        1,
        Note {
            id: 1,
            title: "welcome".to_owned(),
            body: "the first note".to_owned(),
        },
    );
    *extensions.next_id.lock().unwrap() = 1;

    let responder_factory = ResponderFactory::with_router(router).and_extensions(extensions);
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
