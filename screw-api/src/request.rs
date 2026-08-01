use crate::response::ApiResponseContentFailure;
use hyper::http::request::Parts;
use screw_components::dyn_result::DResult;
use screw_core::routing::actix::Path;
use serde::Deserialize;
use std::future::Future;
use std::marker::PhantomData;
use std::net::SocketAddr;
use std::sync::Arc;

/// Everything the converter could extract from the request, handed to
/// [`ApiRequestContent::create`] to build the handler's own request type.
pub struct ApiRequestOriginContent<Data, Extensions>
where
    Data: for<'de> Deserialize<'de>,
{
    pub path: Path<String>,
    pub query: screw_core::routing::query::Query,
    pub http_parts: Parts,
    pub remote_addr: SocketAddr,
    pub extensions: Arc<Extensions>,
    pub data_result: DResult<Data>,
}

/// What a handler wants out of the request, and how to build it.
///
/// `Data` is the body, deserialized by the format converter. Note that
/// `data_result` is a `Result`: a missing or wrong `Content-Type`, a malformed
/// body and a body over the size limit all arrive here as an error rather than
/// failing the request, so the handler decides what a bad body means for that
/// endpoint.
///
/// `create` may also refuse to build the content at all. `Failure` is the
/// failure half of the response the handler answers with, so an `Err` is an
/// ordinary failure response and the handler is never called.
///
/// It is `async`, so a content may reach for whatever it needs to answer that
/// question -- `extensions` is right there -- rather than only for what the
/// request itself carries.
pub trait ApiRequestContent<Extensions, Failure>: Sized
where
    Failure: ApiResponseContentFailure,
{
    type Data: for<'de> Deserialize<'de>;
    fn create(
        origin_content: ApiRequestOriginContent<Self::Data, Extensions>,
    ) -> impl Future<Output = Result<Self, Failure>> + Send;
}

impl<Extensions, Failure> ApiRequestContent<Extensions, Failure> for ()
where
    Extensions: Send + Sync,
    Failure: ApiResponseContentFailure,
{
    type Data = ();
    async fn create(
        _origin_content: ApiRequestOriginContent<Self::Data, Extensions>,
    ) -> Result<Self, Failure> {
        Ok(())
    }
}

pub struct ApiRequest<Content, Extensions> {
    pub content: Content,
    pub(super) _p_e: PhantomData<Extensions>,
}

impl<Content, Extensions> From<ApiRequest<Content, Extensions>> for (Content,) {
    fn from(value: ApiRequest<Content, Extensions>) -> Self {
        (value.content,)
    }
}
