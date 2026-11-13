//! The sans-IO protocol core.
//!
//! The core looks up one generated method, checks the negotiated session,
//! calls the handler, and returns the opaque response messages. It does not
//! read a socket, a clock, or a file.
//!
//! The core does not decode a sync frame. It does not decode a policy AST. It
//! passes both payloads to the handler without a change.

use crate::error::ProtocolError;
use crate::generator::{GeneratedMethod, GeneratedSurface};
use crate::session::{NegotiatedSession, Session, SessionConfig, SessionHelloFacts};

/// A handler for one generated method.
pub trait CallHandler {
    /// Handles one method call.
    ///
    /// The request bytes are opaque. A sync frame or a policy payload passes
    /// without a decode.
    ///
    /// # Errors
    ///
    /// Returns a named [`ProtocolError`] when the call fails.
    fn handle(
        &mut self,
        method: &GeneratedMethod,
        request: &[u8],
    ) -> Result<Vec<Vec<u8>>, ProtocolError>;
}

/// The opaque messages of one successful call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallOutcome {
    messages: Vec<Vec<u8>>,
}

impl CallOutcome {
    /// Creates a call outcome.
    #[must_use]
    pub const fn new(messages: Vec<Vec<u8>>) -> Self {
        Self { messages }
    }

    /// Returns the messages.
    #[must_use]
    pub fn messages(&self) -> &[Vec<u8>] {
        &self.messages
    }

    /// Consumes the outcome and returns the messages.
    #[must_use]
    pub fn into_messages(self) -> Vec<Vec<u8>> {
        self.messages
    }

    /// Returns the number of messages.
    #[must_use]
    pub fn len(&self) -> usize {
        self.messages.len()
    }

    /// Returns true when the outcome has no messages.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.messages.is_empty()
    }
}

/// The protocol core for one generated surface and one handler.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProtocolCore<H> {
    surface: GeneratedSurface,
    session: Session,
    handler: H,
}

impl<H> ProtocolCore<H> {
    /// Creates a protocol core.
    #[must_use]
    pub const fn new(surface: GeneratedSurface, config: SessionConfig, handler: H) -> Self {
        Self {
            surface,
            session: Session::new(config),
            handler,
        }
    }

    /// Returns the generated surface.
    #[must_use]
    pub const fn surface(&self) -> &GeneratedSurface {
        &self.surface
    }

    /// Returns the session.
    #[must_use]
    pub const fn session(&self) -> &Session {
        &self.session
    }

    /// Returns the mutable session.
    #[must_use]
    pub const fn session_mut(&mut self) -> &mut Session {
        &mut self.session
    }

    /// Negotiates the session.
    ///
    /// # Errors
    ///
    /// Returns the error from [`Session::negotiate`].
    pub fn negotiate(
        &mut self,
        facts: &SessionHelloFacts,
    ) -> Result<NegotiatedSession, ProtocolError> {
        self.session.negotiate(facts)
    }
}

impl<H: CallHandler> ProtocolCore<H> {
    /// Calls one generated method.
    ///
    /// # Errors
    ///
    /// Returns `rpc.unknown_method` when the path is not generated.
    /// Returns `sync.negotiation_required` before negotiation.
    /// Returns the handler error when the handler fails.
    /// Returns `internal.invariant_violation` when a unary handler returns a
    /// message count that is not one.
    pub fn call(
        &mut self,
        method_path: &str,
        request: &[u8],
    ) -> Result<CallOutcome, ProtocolError> {
        let method =
            self.surface
                .method(method_path)
                .ok_or_else(|| ProtocolError::UnknownMethod {
                    method: method_path.to_owned(),
                })?;
        self.session.require_negotiated()?;
        let messages = self.handler.handle(method, request)?;
        if method.streaming() == crate::descriptor::StreamingShape::Unary && messages.len() != 1 {
            return Err(ProtocolError::HandlerViolation {
                detail: format!(
                    "The unary method {method_path} returned {} messages.",
                    messages.len()
                ),
            });
        }
        Ok(CallOutcome::new(messages))
    }

    /// Returns the handler.
    #[must_use]
    pub const fn handler(&self) -> &H {
        &self.handler
    }

    /// Returns the mutable handler.
    #[must_use]
    pub const fn handler_mut(&mut self) -> &mut H {
        &mut self.handler
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::descriptor::StreamingShape;
    use crate::generator::reference_surface;
    use crate::session::SessionConfig;
    use oxkv_contracts::ErrorCode;
    use oxkv_contracts::version::SyncWireVersion;

    #[derive(Debug, Clone, Default, PartialEq, Eq)]
    struct EchoHandler {
        seen: Vec<(String, Vec<u8>)>,
        unary_message_count: Option<usize>,
    }

    impl CallHandler for EchoHandler {
        fn handle(
            &mut self,
            method: &GeneratedMethod,
            request: &[u8],
        ) -> Result<Vec<Vec<u8>>, ProtocolError> {
            self.seen.push((method.path().to_owned(), request.to_vec()));
            let count = self
                .unary_message_count
                .unwrap_or(usize::from(method.streaming() == StreamingShape::Unary));
            Ok((0..count).map(|_| request.to_vec()).collect())
        }
    }

    fn core() -> ProtocolCore<EchoHandler> {
        let surface = reference_surface().expect("the surface compiles");
        let identity = surface.identity();
        let config = SessionConfig::new(
            identity,
            SyncWireVersion::new(1),
            vec![crate::connect::ConnectCodec::Json],
        );
        ProtocolCore::new(surface, config, EchoHandler::default())
    }

    fn facts(core: &ProtocolCore<EchoHandler>) -> SessionHelloFacts {
        SessionHelloFacts::new(
            core.surface().identity().descriptor_digest(),
            core.surface().identity().manifest_digest(),
            SyncWireVersion::new(1),
            crate::connect::ConnectCodec::Json,
        )
    }

    #[test]
    fn a_data_call_before_negotiation_fails() {
        let mut core = core();
        let error = core.call("data/get", &[]).unwrap_err();
        assert_eq!(error.code().as_str(), "sync.negotiation_required");
    }

    #[test]
    fn a_generated_call_returns_the_handler_messages() {
        let mut core = core();
        core.negotiate(&facts(&core))
            .expect("the facts are compatible");
        let outcome = core
            .call("data/get", &[1, 2, 3])
            .expect("the call succeeds");
        assert_eq!(outcome.messages(), &[vec![1, 2, 3]]);
        assert_eq!(core.handler().seen.len(), 1);
    }

    #[test]
    fn an_unknown_method_has_the_named_error() {
        let mut core = core();
        core.negotiate(&facts(&core))
            .expect("the facts are compatible");
        let error = core.call("data/unknown", &[]).unwrap_err();
        assert_eq!(error.code().as_str(), "rpc.unknown_method");
    }

    #[test]
    fn a_handler_error_passes_through() {
        #[derive(Debug, Clone)]
        struct Failing;
        impl CallHandler for Failing {
            fn handle(
                &mut self,
                _method: &GeneratedMethod,
                _request: &[u8],
            ) -> Result<Vec<Vec<u8>>, ProtocolError> {
                let code = ErrorCode::parse("sync.cursor_expired").expect("the code is registered");
                Err(ProtocolError::handler(code))
            }
        }
        let surface = reference_surface().expect("the surface compiles");
        let config = SessionConfig::new(
            surface.identity(),
            SyncWireVersion::new(1),
            vec![crate::connect::ConnectCodec::Json],
        );
        let mut core = ProtocolCore::new(surface, config, Failing);
        let hello = SessionHelloFacts::new(
            core.surface().identity().descriptor_digest(),
            core.surface().identity().manifest_digest(),
            SyncWireVersion::new(1),
            crate::connect::ConnectCodec::Json,
        );
        core.negotiate(&hello).expect("the facts are compatible");
        let error = core.call("sync/subscribe", &[]).unwrap_err();
        assert_eq!(error.code().as_str(), "sync.cursor_expired");
    }

    #[test]
    fn a_unary_handler_must_return_one_message() {
        let mut core = core();
        core.negotiate(&facts(&core))
            .expect("the facts are compatible");
        core.handler_mut().unary_message_count = Some(2);
        let error = core.call("data/get", &[]).unwrap_err();
        assert_eq!(error.code().as_str(), "internal.invariant_violation");
    }

    #[test]
    fn a_server_stream_can_return_no_message() {
        let mut core = core();
        core.negotiate(&facts(&core))
            .expect("the facts are compatible");
        core.handler_mut().unary_message_count = Some(0);
        let outcome = core
            .call("sync/subscribe", &[4, 5])
            .expect("the stream call succeeds");
        assert!(outcome.is_empty());
    }
}
