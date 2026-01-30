use crate::body::ResponseBody;

pub struct Response {
    pub http: hyper::Response<ResponseBody>,
}
