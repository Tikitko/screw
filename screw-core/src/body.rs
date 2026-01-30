use http_body_util::BodyExt;

pub type ResponseBody =
    http_body_util::combinators::BoxBody<bytes::Bytes, std::convert::Infallible>;

pub fn empty() -> ResponseBody {
    http_body_util::Empty::<bytes::Bytes>::new().boxed()
}

pub fn full(data: impl Into<bytes::Bytes>) -> ResponseBody {
    http_body_util::Full::new(data.into()).boxed()
}
