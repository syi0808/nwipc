//! Provider-erased public configuration, session, renderer, peer-bootstrap, and diagnostics API.

use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Write};
use std::sync::{Arc, Mutex};

use nwipc_bootstrap_schema::{BootstrapEnvelope, BootstrapSecret, EndpointRole, ProtocolRange};
use nwipc_capabilities::TransportTopology;
use nwipc_diagnostics::{
    DiagnosticsSnapshot, MemoryBackend as DiagnosticMemoryBackend, SessionDiagnostics,
    SignalBackend as DiagnosticSignalBackend,
};
use nwipc_error::{ErrorCategory, ErrorCode, ErrorReport, Recoverability};
use nwipc_macos_transport::{
    ChannelConfiguration, MacosRendererTransportFactory, PreparedMacosTransport, ensure_available,
    production_capabilities,
};
use nwipc_metrics::Metrics;
use nwipc_peer_bootstrap::write_envelope;
use nwipc_renderer_api::{RendererTransport, SendDisposition, TransportEvent};
use nwipc_renderer_bootstrap::RendererBootstrap;
use nwipc_runtime::{ProviderSelection, ResourcePreparer, Runtime, SessionHandle};
use nwipc_session::{OwnedResource, PreparedResources};
use nwipc_session_machine::LifecycleEvent;
use nwipc_state::SessionState;
use nwipc_types::{Generation, SessionId};

const SECRET_LENGTH: usize = 32;

/// Provider-neutral production configuration.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Configuration {
    /// Bytes in each unidirectional ring.
    pub channel_capacity: u32,
    /// Largest record payload before fragmentation.
    pub maximum_inline_message: u32,
    /// Largest logical application message.
    pub maximum_message: u32,
    /// Writable recovery low watermark.
    pub low_watermark: u32,
    /// Backpressure high watermark.
    pub high_watermark: u32,
    /// Selected protocol major version.
    pub protocol: u16,
}

impl Default for Configuration {
    fn default() -> Self {
        let channel = ChannelConfiguration::default();
        Self {
            channel_capacity: channel.capacity,
            maximum_inline_message: channel.maximum_inline_message,
            maximum_message: channel.maximum_message,
            low_watermark: channel.low_watermark,
            high_watermark: channel.high_watermark,
            protocol: 1,
        }
    }
}

impl Configuration {
    /// Validates public limits without allocating provider resources.
    ///
    /// # Errors
    ///
    /// Returns `InvalidRange` for inconsistent limits or a zero protocol.
    pub fn validate(self) -> Result<Self, ErrorReport> {
        if self.protocol == 0 || u8::try_from(self.protocol).is_err() {
            return Err(configuration_error(
                ErrorCode::InvalidRange,
                "public protocol version",
            ));
        }
        self.channel_configuration().validate()?;
        Ok(self)
    }

    const fn channel_configuration(self) -> ChannelConfiguration {
        ChannelConfiguration {
            capacity: self.channel_capacity,
            maximum_inline_message: self.maximum_inline_message,
            maximum_message: self.maximum_message,
            low_watermark: self.low_watermark,
            high_watermark: self.high_watermark,
        }
    }
}

struct BootstrapPair {
    peer: BootstrapEnvelope,
    renderer: BootstrapEnvelope,
}

type BootstrapCatalog = Arc<Mutex<HashMap<(SessionId, Generation), BootstrapPair>>>;

struct MacosPreparer {
    configuration: Configuration,
    catalog: BootstrapCatalog,
}

impl ResourcePreparer for MacosPreparer {
    fn prepare(
        &mut self,
        session_id: SessionId,
        generation: Generation,
        providers: ProviderSelection,
    ) -> Result<PreparedResources, ErrorReport> {
        if providers != ProviderSelection::MACOS {
            return Err(configuration_error(
                ErrorCode::Unsupported,
                "public provider selection",
            ));
        }
        let transport = PreparedMacosTransport::prepare(
            session_id,
            generation,
            self.configuration.channel_configuration(),
        )?;
        let memory = transport.memory_descriptor()?;
        let signal = transport.signal_descriptor()?;
        let mut secret = [0; SECRET_LENGTH];
        File::open("/dev/urandom")
            .and_then(|mut source| source.read_exact(&mut secret))
            .map_err(|_| configuration_error(ErrorCode::Internal, "bootstrap randomness"))?;
        let protocols =
            ProtocolRange::new(self.configuration.protocol, self.configuration.protocol)?;
        let peer = BootstrapEnvelope::new(
            session_id,
            generation,
            protocols,
            EndpointRole::Peer,
            memory.clone(),
            signal.clone(),
            BootstrapSecret::new(secret.to_vec())?,
        )?;
        let renderer = BootstrapEnvelope::new(
            session_id,
            generation,
            protocols,
            EndpointRole::Renderer,
            memory,
            signal,
            BootstrapSecret::new(secret.to_vec())?,
        )?;
        secret.fill(0);
        self.catalog
            .lock()
            .map_err(|_| configuration_error(ErrorCode::Internal, "bootstrap catalog"))?
            .insert((session_id, generation), BootstrapPair { peer, renderer });
        let mut resources = PreparedResources::new();
        resources.push(TransportResource(Some(transport)));
        Ok(resources)
    }
}

struct TransportResource(Option<PreparedMacosTransport>);

impl OwnedResource for TransportResource {
    fn cleanup(&mut self) -> Result<(), ErrorReport> {
        self.0.take();
        Ok(())
    }
}

struct ObservedRendererTransport<Transport> {
    inner: Transport,
    metrics: Metrics,
}

impl<Transport: RendererTransport> RendererTransport for ObservedRendererTransport<Transport> {
    fn send(&mut self, payload: &[u8]) -> Result<SendDisposition, ErrorReport> {
        match self.inner.send(payload) {
            Ok(disposition) => {
                self.metrics.record_sent(payload.len());
                if disposition == SendDisposition::Backpressured {
                    self.metrics.record_backpressure();
                }
                Ok(disposition)
            }
            Err(error) => {
                if error.code() == ErrorCode::Backpressured {
                    self.metrics.record_backpressure();
                } else {
                    self.metrics.record_failure();
                }
                Err(error)
            }
        }
    }

    fn buffered_amount(&self) -> Result<u32, ErrorReport> {
        self.inner.buffered_amount()
    }

    fn poll(&mut self) -> Result<Option<TransportEvent>, ErrorReport> {
        match self.inner.poll() {
            Ok(Some(TransportEvent::Message(payload))) => {
                self.metrics.record_received(payload.len());
                Ok(Some(TransportEvent::Message(payload)))
            }
            Ok(Some(TransportEvent::Writable)) => {
                self.metrics.record_writable();
                Ok(Some(TransportEvent::Writable))
            }
            Ok(Some(TransportEvent::Error(error))) => {
                self.metrics.record_failure();
                Ok(Some(TransportEvent::Error(error)))
            }
            Ok(event) => Ok(event),
            Err(error) => {
                self.metrics.record_failure();
                Err(error)
            }
        }
    }

    fn close(&mut self) -> Result<(), ErrorReport> {
        self.inner.close().inspect_err(|_| {
            self.metrics.record_failure();
        })
    }
}

/// Opaque public handle for one logical session generation.
pub struct Session {
    handle: SessionHandle,
    protocol: u16,
    peer_bootstrap: Option<BootstrapEnvelope>,
    renderer_bootstrap: Option<BootstrapEnvelope>,
}

impl Session {
    /// Stable logical session identity.
    pub const fn id(&self) -> SessionId {
        self.handle.session_id
    }

    /// Active resource generation.
    pub const fn generation(&self) -> Generation {
        self.handle.generation
    }

    /// Environment values required by [`nwipc_peer::Peer::initialize`](https://docs.rs/nwipc-peer).
    pub fn peer_environment(&self) -> PeerEnvironment {
        PeerEnvironment {
            session_id: encode_session(self.id()),
            generation: self.generation().get().to_string(),
            protocol: self.protocol.to_string(),
        }
    }

    /// Writes the one-shot inherited peer bootstrap and erases the local secret copy.
    ///
    /// This stream contains bootstrap only; application payload never uses it.
    ///
    /// # Errors
    ///
    /// Returns `Closed` after consumption or a typed bootstrap I/O error.
    pub fn write_peer_bootstrap(&mut self, writer: &mut impl Write) -> Result<(), ErrorReport> {
        let envelope = self
            .peer_bootstrap
            .take()
            .ok_or_else(|| configuration_error(ErrorCode::Closed, "consume peer bootstrap"))?;
        write_envelope(writer, &envelope)
    }

    /// Writes the one-shot canonical renderer bootstrap for a process-pool property-list value.
    ///
    /// This is control-plane data for the injected bundle. The host must pass it through without
    /// decoding it and must never use the returned bytes as an application payload channel.
    ///
    /// # Errors
    ///
    /// Returns `Closed` after consumption or a typed bootstrap encoding error.
    pub fn write_renderer_bootstrap(&mut self, writer: &mut impl Write) -> Result<(), ErrorReport> {
        let envelope = self
            .renderer_bootstrap
            .take()
            .ok_or_else(|| configuration_error(ErrorCode::Closed, "consume renderer bootstrap"))?;
        let encoded = nwipc_bootstrap_codec::encode(&envelope)?;
        writer
            .write_all(&encoded)
            .map_err(|_| configuration_error(ErrorCode::Closed, "write renderer bootstrap"))
    }
}

/// Redacted child-process launch values. No provider descriptor is exposed.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PeerEnvironment {
    session_id: String,
    generation: String,
    protocol: String,
}

impl PeerEnvironment {
    /// Applies the stable environment variables to a child command.
    pub fn apply(&self, command: &mut std::process::Command) {
        command
            .env("NWIPC_SESSION_ID", &self.session_id)
            .env("NWIPC_GENERATION", &self.generation)
            .env("NWIPC_PROTOCOL", &self.protocol);
    }
}

/// Top-level owner of production runtime resources and operational state.
pub struct Nwipc {
    configuration: Configuration,
    runtime: Runtime<MacosPreparer>,
    catalog: BootstrapCatalog,
    metrics: Metrics,
    sessions: HashMap<SessionId, SessionHandle>,
}

impl Nwipc {
    /// Initializes the supported production provider combination with default limits.
    ///
    /// # Errors
    ///
    /// Returns explicit `Unsupported` when `IOSurface`/Darwin providers are unavailable.
    pub fn initialize() -> Result<Self, ErrorReport> {
        Self::with_configuration(Configuration::default())
    }

    /// Initializes the production runtime after validating all public limits.
    ///
    /// # Errors
    ///
    /// Returns a typed configuration or provider availability error.
    pub fn with_configuration(configuration: Configuration) -> Result<Self, ErrorReport> {
        let configuration = configuration.validate()?;
        ensure_available()?;
        let catalog = Arc::new(Mutex::new(HashMap::new()));
        let preparer = MacosPreparer {
            configuration,
            catalog: Arc::clone(&catalog),
        };
        Ok(Self {
            configuration,
            runtime: Runtime::new(ProviderSelection::MACOS, preparer),
            catalog,
            metrics: Metrics::new(),
            sessions: HashMap::new(),
        })
    }

    /// Validated runtime configuration.
    pub const fn configuration(&self) -> Configuration {
        self.configuration
    }

    /// Allocates both directions and returns an opaque public session handle.
    ///
    /// # Errors
    ///
    /// Propagates provider allocation, layout, randomness, and runtime failures.
    pub fn create_session(&mut self) -> Result<Session, ErrorReport> {
        let handle = self.runtime.create_session().inspect_err(|_| {
            self.metrics.record_failure();
        })?;
        let pair = self
            .catalog
            .lock()
            .map_err(|_| configuration_error(ErrorCode::Internal, "bootstrap catalog"))?
            .remove(&(handle.session_id, handle.generation))
            .ok_or_else(|| configuration_error(ErrorCode::Internal, "prepared bootstrap"))?;
        self.metrics.record_session_created();
        self.sessions.insert(handle.session_id, handle);
        Ok(Session {
            handle,
            protocol: self.configuration.protocol,
            peer_bootstrap: Some(pair.peer),
            renderer_bootstrap: Some(pair.renderer),
        })
    }

    /// Attaches the renderer production channel and completes `HELLO`/`ACK` with the native peer.
    ///
    /// # Errors
    ///
    /// Returns provider, bootstrap, handshake, or stale-generation failures.
    pub fn open_renderer(
        &mut self,
        session: &mut Session,
    ) -> Result<Box<dyn RendererTransport>, ErrorReport> {
        let envelope = session
            .renderer_bootstrap
            .take()
            .ok_or_else(|| configuration_error(ErrorCode::Closed, "consume renderer bootstrap"))?;
        let transport = RendererBootstrap::open_transport(
            envelope,
            session.id(),
            session.generation(),
            self.configuration.protocol,
            &mut MacosRendererTransportFactory::default(),
        )
        .inspect_err(|_| self.metrics.record_failure())?;
        for event in [
            LifecycleEvent::RendererAttached,
            LifecycleEvent::PeerAttached,
            LifecycleEvent::HandshakeCompleted,
        ] {
            self.runtime.route(session.handle, event)?;
        }
        Ok(Box::new(ObservedRendererTransport {
            inner: transport,
            metrics: self.metrics.clone(),
        }))
    }

    /// Records completion of an externally hosted renderer/peer handshake.
    ///
    /// Use this after an injected-bundle host observes the production transport completion. The
    /// host reports lifecycle only and does not inspect application payload bytes.
    ///
    /// # Errors
    ///
    /// Returns a stale-generation or invalid lifecycle transition error.
    pub fn observe_external_connection(&mut self, session: &Session) -> Result<(), ErrorReport> {
        for event in [
            LifecycleEvent::RendererAttached,
            LifecycleEvent::PeerAttached,
            LifecycleEvent::HandshakeCompleted,
        ] {
            self.runtime.route(session.handle, event)?;
        }
        Ok(())
    }

    /// Invalidates a terminated `WebContent` generation and prepares its replacement.
    ///
    /// The returned handle keeps the logical session identity but owns fresh mappings, signals,
    /// secret, bootstrap values, and generation.
    ///
    /// # Errors
    ///
    /// Returns a stale-generation, lifecycle, provider preparation, or bootstrap catalog error.
    pub fn replace_renderer(&mut self, session: &Session) -> Result<Session, ErrorReport> {
        let outcome = self
            .runtime
            .route(session.handle, LifecycleEvent::RendererExited)
            .inspect_err(|_| self.metrics.record_failure())?;
        if !outcome.replaced {
            self.metrics.record_failure();
            return Err(configuration_error(
                ErrorCode::InvalidStateTransition,
                "replace external renderer generation",
            ));
        }
        let pair = self
            .catalog
            .lock()
            .map_err(|_| configuration_error(ErrorCode::Internal, "bootstrap catalog"))?
            .remove(&(outcome.active.session_id, outcome.active.generation))
            .ok_or_else(|| configuration_error(ErrorCode::Internal, "replacement bootstrap"))?;
        self.sessions
            .insert(outcome.active.session_id, outcome.active);
        self.metrics.record_replacement();
        Ok(Session {
            handle: outcome.active,
            protocol: self.configuration.protocol,
            peer_bootstrap: Some(pair.peer),
            renderer_bootstrap: Some(pair.renderer),
        })
    }

    /// Idempotently closes and releases a session generation.
    ///
    /// # Errors
    ///
    /// Returns `StaleGeneration` for a replaced handle or a provider cleanup failure.
    pub fn close(&mut self, session: &Session) -> Result<(), ErrorReport> {
        let was_closed = self
            .runtime
            .state(session.handle)
            .is_ok_and(SessionState::is_terminal);
        self.runtime.close(session.handle)?;
        if !was_closed {
            self.metrics.record_session_closed();
        }
        Ok(())
    }

    /// Returns a redacted operational snapshot with no payload, secret, name, ID, or native handle.
    pub fn diagnostics(&self) -> DiagnosticsSnapshot {
        let mut sessions = Vec::new();
        for handle in self.sessions.values() {
            if let Ok(state) = self.runtime.state(*handle) {
                sessions.push(diagnostic_entry(
                    handle.session_id,
                    handle.generation,
                    state,
                    state.is_terminal(),
                ));
            }
        }
        DiagnosticsSnapshot::new(self.metrics.snapshot(), sessions)
    }

    /// Returns diagnostics for an active public handle.
    ///
    /// # Errors
    ///
    /// Returns `StaleGeneration` unless the handle is active.
    pub fn session_diagnostics(
        &self,
        session: &Session,
    ) -> Result<SessionDiagnostics, ErrorReport> {
        let state = self.runtime.state(session.handle)?;
        Ok(diagnostic_entry(
            session.id(),
            session.generation(),
            state,
            state.is_terminal(),
        ))
    }
}

fn diagnostic_entry(
    session_id: SessionId,
    generation: Generation,
    state: SessionState,
    resources_cleaned: bool,
) -> SessionDiagnostics {
    SessionDiagnostics {
        session_id,
        generation,
        state,
        topology: TransportTopology::direct(),
        capabilities: production_capabilities(),
        memory_backend: DiagnosticMemoryBackend::IoSurface,
        signal_backend: DiagnosticSignalBackend::Hybrid,
        last_error: None,
        resources_cleaned,
    }
}

fn encode_session(session_id: SessionId) -> String {
    use std::fmt::Write as _;
    let mut encoded = String::with_capacity(32);
    for byte in session_id.to_bytes() {
        write!(encoded, "{byte:02x}").expect("writing to String cannot fail");
    }
    encoded
}

fn configuration_error(code: ErrorCode, operation: &'static str) -> ErrorReport {
    ErrorReport::new(
        match code {
            ErrorCode::Unsupported => ErrorCategory::Unsupported,
            ErrorCode::Closed => ErrorCategory::Closed,
            _ => ErrorCategory::Configuration,
        },
        code,
        Recoverability::Terminal,
        operation,
    )
}

#[cfg(test)]
mod tests {
    #[cfg(target_os = "macos")]
    use std::io::Cursor;
    #[cfg(target_os = "macos")]
    use std::time::Duration;

    use super::*;

    #[test]
    fn invalid_public_configuration_fails_before_provider_initialization() {
        let invalid = Configuration {
            protocol: 0,
            ..Configuration::default()
        };
        assert_eq!(
            invalid.validate().unwrap_err().code(),
            ErrorCode::InvalidRange
        );
    }

    #[test]
    fn peer_environment_is_provider_erased() {
        let environment = PeerEnvironment {
            session_id: "01".repeat(16),
            generation: "2".into(),
            protocol: "1".into(),
        };
        let debug = format!("{environment:?}");
        assert!(!debug.contains("IOSurface"));
        assert!(!debug.contains("Darwin"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn renderer_bootstrap_is_canonical_and_one_shot() {
        let mut nwipc = Nwipc::initialize().unwrap();
        let mut session = nwipc.create_session().unwrap();
        let expected_session = session.id();
        let expected_generation = session.generation();
        let mut encoded = Vec::new();
        session.write_renderer_bootstrap(&mut encoded).unwrap();
        let envelope = nwipc_bootstrap_codec::decode(&encoded).unwrap();
        envelope
            .validate_for(
                EndpointRole::Renderer,
                expected_session,
                expected_generation,
                Configuration::default().protocol,
            )
            .unwrap();
        assert!(session.write_renderer_bootstrap(&mut Vec::new()).is_err());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn external_renderer_exit_replaces_public_generation_and_rejects_stale_handle() {
        let mut nwipc = Nwipc::initialize().unwrap();
        let session = nwipc.create_session().unwrap();
        let identity = session.id();
        let generation = session.generation();
        nwipc.observe_external_connection(&session).unwrap();

        let replacement = nwipc.replace_renderer(&session).unwrap();
        assert_eq!(replacement.id(), identity);
        assert_ne!(replacement.generation(), generation);
        assert_eq!(
            nwipc.session_diagnostics(&session).unwrap_err().code(),
            ErrorCode::StaleGeneration
        );
        assert_eq!(
            nwipc.session_diagnostics(&replacement).unwrap().state,
            SessionState::WaitingForRenderer
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn public_facade_connects_renderer_and_peer_without_payload_stream() {
        use nwipc_macos_transport::{MacosEndpointTransport, production_capabilities};
        use nwipc_peer_bootstrap::consume;
        use nwipc_peer_core::{NativePort, PeerExpectation, PortEvent};

        let mut nwipc = Nwipc::initialize().unwrap();
        let mut session = nwipc.create_session().unwrap();
        let expectation = PeerExpectation {
            session_id: session.id(),
            generation: session.generation(),
            protocol: session.protocol,
        };
        let mut bootstrap = Vec::new();
        session.write_peer_bootstrap(&mut bootstrap).unwrap();
        let peer = std::thread::spawn(move || {
            let envelope = consume(Cursor::new(bootstrap)).unwrap();
            let raw = MacosEndpointTransport::attach(&envelope, EndpointRole::Peer).unwrap();
            let mut port = NativePort::accept(
                envelope,
                expectation,
                raw,
                Configuration::default().maximum_message as usize,
                production_capabilities(),
            )
            .unwrap();
            match port.try_receive().unwrap() {
                Some(PortEvent::Message(payload)) => port.try_send(&payload).unwrap(),
                event => panic!("unexpected peer event: {event:?}"),
            }
            assert_eq!(port.try_receive().unwrap(), Some(PortEvent::Closed));
        });
        let mut renderer = nwipc.open_renderer(&mut session).unwrap();
        assert_eq!(
            renderer.send(b"production").unwrap(),
            nwipc_renderer_api::SendDisposition::Sent
        );
        let response = (0..100).find_map(|_| {
            if let Some(event) = renderer.poll().unwrap() {
                Some(event)
            } else {
                std::thread::sleep(Duration::from_millis(1));
                None
            }
        });
        assert_eq!(
            response,
            Some(nwipc_renderer_api::TransportEvent::Message(
                b"production".to_vec()
            ))
        );
        renderer.close().unwrap();
        peer.join().unwrap();
        assert_eq!(
            nwipc.session_diagnostics(&session).unwrap().state,
            SessionState::Open
        );
        nwipc.close(&session).unwrap();
        let diagnostics = nwipc.diagnostics();
        assert!(diagnostics.sessions[0].resources_cleaned);
        assert_eq!(diagnostics.metrics.messages_sent, 1);
        assert_eq!(diagnostics.metrics.messages_received, 1);
    }
}
