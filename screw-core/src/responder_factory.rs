pub type ResponderFactory<Extensions> = first::ResponderFactory<Extensions>;
pub type FResponderFactory<Extensions> = second::ResponderFactory<Extensions>;

use super::*;
use crate::body::ResponseBody;
use crate::catch_unwind::catch_unwind;
use hyper::StatusCode;
use hyper::body::Incoming;
use std::future::Future;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;

pub mod first {
    use super::*;
    use std::sync::Arc;

    pub struct ResponderFactory<Extensions>
    where
        Extensions: Sync + Send + 'static,
    {
        router:
            Arc<routing::router::second::Router<request::Request<Extensions>, response::Response>>,
    }

    impl<Extensions> ResponderFactory<Extensions>
    where
        Extensions: Sync + Send + 'static,
    {
        pub fn with_router(
            router: routing::router::second::Router<
                request::Request<Extensions>,
                response::Response,
            >,
        ) -> Self {
            Self {
                router: Arc::new(router),
            }
        }

        pub fn and_extensions(
            self,
            extensions: Extensions,
        ) -> second::ResponderFactory<Extensions> {
            second::ResponderFactory {
                router: self.router,
                extensions: Arc::new(extensions),
            }
        }
    }
}

pub mod second {
    use super::*;
    use std::net::SocketAddr;
    use std::sync::Arc;

    pub struct ResponderFactory<Extensions>
    where
        Extensions: Sync + Send + 'static,
    {
        pub(super) router:
            Arc<routing::router::second::Router<request::Request<Extensions>, response::Response>>,
        pub(super) extensions: Arc<Extensions>,
    }

    impl<Extensions> server::ResponderFactory for ResponderFactory<Extensions>
    where
        Extensions: Sync + Send + 'static,
    {
        type Responder = Responder<Extensions>;
        fn make_responder(&self, remote_addr: SocketAddr) -> Self::Responder {
            Responder {
                remote_addr,
                router: self.router.clone(),
                extensions: self.extensions.clone(),
            }
        }
    }
}

pub struct Responder<Extensions>
where
    Extensions: Sync + Send + 'static,
{
    remote_addr: SocketAddr,
    router: Arc<routing::router::second::Router<request::Request<Extensions>, response::Response>>,
    extensions: Arc<Extensions>,
}

impl<Extensions> server::Responder for Responder<Extensions>
where
    Extensions: Sync + Send + 'static,
{
    type ResponseFuture = Pin<Box<dyn Future<Output = hyper::Response<ResponseBody>> + Send>>;

    fn response(&self, http_request: hyper::Request<Incoming>) -> Self::ResponseFuture {
        let remote_addr = self.remote_addr;
        let router = self.router.clone();
        let extensions = self.extensions.clone();
        Box::pin(async move {
            let request = request::Request {
                remote_addr,
                extensions,
                http: http_request,
            };
            match catch_unwind(router.process(request)).await {
                Ok(response) => response.http,
                Err(_) => hyper::Response::builder()
                    .status(StatusCode::INTERNAL_SERVER_ERROR)
                    .body(crate::body::empty())
                    .unwrap(),
            }
        })
    }
}
