use super::*;
use std::net::SocketAddr;

pub struct ServerService<F, R>
where
    F: ResponderFactory<Responder = R>,
    R: Responder,
    R::ResponseFuture: Send + 'static,
{
    responder_factory: F,
}

impl<F, R> ServerService<F, R>
where
    F: ResponderFactory<Responder = R>,
    R: Responder,
    R::ResponseFuture: Send + 'static,
{
    pub fn with_responder_factory(responder_factory: F) -> Self {
        Self { responder_factory }
    }

    pub fn make_session_service(&self, remote_addr: SocketAddr) -> SessionService<R> {
        let responder = self.responder_factory.make_responder(remote_addr);
        SessionService { responder }
    }
}
