use hyper::body::Incoming;
use std::net::SocketAddr;
use std::sync::Arc;

pub struct Request<Extensions> {
    pub remote_addr: SocketAddr,
    pub extensions: Arc<Extensions>,
    pub http: hyper::Request<Incoming>,
}

impl<Extensions> AsRef<hyper::Request<Incoming>> for Request<Extensions> {
    fn as_ref(&self) -> &hyper::Request<Incoming> {
        &self.http
    }
}
