use super::super::*;
use hyper::body::Incoming;
use hyper::http::request::Parts;
use hyper::{header, StatusCode};
use response::ApiResponseContentBase;
use screw_components::dyn_fn::DFnOnce;
use screw_components::dyn_result::DResult;
use screw_core::body::ResponseBody;
use screw_core::request::Request;
use screw_core::response::Response;
use screw_core::routing::middleware::Middleware;
use screw_core::routing::router::RoutedRequest;
use serde::Deserialize;

#[derive(Clone, Copy, Debug)]
pub struct XmlApiMiddlewareConverter;

#[async_trait]
impl<RqContent, Extensions, RsContentSuccess, RsContentFailure>
    Middleware<
        request::ApiRequest<RqContent, Extensions>,
        response::ApiResponse<RsContentSuccess, RsContentFailure>,
    > for XmlApiMiddlewareConverter
where
    RqContent: request::ApiRequestContent<Extensions> + Send + 'static,
    <RqContent as request::ApiRequestContent<Extensions>>::Data: Sync + Send + 'static,
    Extensions: Sync + Send + 'static,
    RsContentSuccess: response::ApiResponseContentSuccess + Send + 'static,
    RsContentFailure: response::ApiResponseContentFailure + Send + 'static,
{
    type Request = RoutedRequest<Request<Extensions>>;
    type Response = Response;
    async fn respond(
        &self,
        routed_request: RoutedRequest<Request<Extensions>>,
        next: DFnOnce<
            request::ApiRequest<RqContent, Extensions>,
            response::ApiResponse<RsContentSuccess, RsContentFailure>,
        >,
    ) -> Response {
        async fn convert<Data>(parts: &Parts, body: Incoming) -> DResult<Data>
        where
            for<'de> Data: Deserialize<'de>,
        {
            use http_body_util::BodyExt;
            let content_type = match parts.headers.get(header::CONTENT_TYPE) {
                Some(header_value) => Some(header_value.to_str()?),
                None => None,
            };
            match content_type {
                Some("application/xml") => Ok(()),
                Some("") | None => Err(ApiRequestContentTypeError::Missed),
                Some(_) => Err(ApiRequestContentTypeError::Incorrect),
            }?;
            let bytes = body.collect().await?.to_bytes();
            let xml_string = String::from_utf8(bytes.to_vec())?;
            let data = quick_xml::de::from_str(xml_string.as_str())?;
            Ok(data)
        }

        let (http_parts, http_body) = routed_request.origin.http.into_parts();
        let data_result = convert(&http_parts, http_body).await;

        let request_content = RqContent::create(request::ApiRequestOriginContent {
            path: routed_request.path,
            query: routed_request.query,
            http_parts,
            remote_addr: routed_request.origin.remote_addr,
            extensions: routed_request.origin.extensions,
            data_result,
        });

        let api_request = request::ApiRequest {
            content: request_content,
            _p_e: Default::default(),
        };

        let api_response = next(api_request).await;

        let http_response_result: DResult<hyper::Response<ResponseBody>> = (|| {
            let content = api_response.content;

            let status_code = content.status_code();
            let xml_string = quick_xml::se::to_string(&content)?;

            let response = hyper::Response::builder()
                .status(status_code)
                .header(header::CONTENT_TYPE, "application/xml")
                .body(screw_core::body::full(xml_string))?;

            Ok(response)
        })();

        let http_response = http_response_result.unwrap_or_else(|_| {
            hyper::Response::builder()
                .status(StatusCode::INTERNAL_SERVER_ERROR)
                .body(screw_core::body::empty())
                .unwrap()
        });

        Response {
            http: http_response,
        }
    }
}
