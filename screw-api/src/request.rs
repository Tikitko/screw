use hyper::http::request::Parts;
use screw_components::dyn_result::DResult;
use screw_core::routing::actix::Path;
use serde::Deserialize;
use std::collections::HashMap;
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
    pub query: HashMap<String, String>,
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
pub trait ApiRequestContent<Extensions> {
    type Data: for<'de> Deserialize<'de>;
    fn create(origin_content: ApiRequestOriginContent<Self::Data, Extensions>) -> Self;
}

impl<Extensions> ApiRequestContent<Extensions> for () {
    type Data = ();
    fn create(_origin_content: ApiRequestOriginContent<Self::Data, Extensions>) -> Self {}
}

pub struct ApiRequest<Content, Extensions>
where
    Content: ApiRequestContent<Extensions>,
{
    pub content: Content,
    pub(super) _p_e: PhantomData<Extensions>,
}

impl<Content, Extensions> From<ApiRequest<Content, Extensions>> for (Content,)
where
    Content: ApiRequestContent<Extensions>,
{
    fn from(value: ApiRequest<Content, Extensions>) -> Self {
        (value.content,)
    }
}
