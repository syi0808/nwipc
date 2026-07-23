//! Control-plane-only `WKWebView` configuration and renderer replacement tracking.

use std::collections::HashMap;
use std::path::Path;

use nwipc_capabilities::TransportTopology;
use nwipc_error::{ErrorCategory, ErrorCode, ErrorReport, Recoverability};
use nwipc_macos_artifact::MacosArtifact;
use nwipc_macos_bundle_api::InitializationData;
use nwipc_macos_spi::MacosSpi;
use nwipc_state::{SessionEvent, SessionState};
use nwipc_types::{Generation, SessionId};

/// Minimal private-WebKit configuration surface implemented by the Objective-C host adapter.
pub trait WebViewConfigurator {
    /// Sets the injected bundle URL on the process-pool configuration.
    ///
    /// # Errors
    ///
    /// Returns a platform configuration failure.
    fn set_injected_bundle(&mut self, path: &Path) -> Result<(), ErrorReport>;
    /// Copies initialization user data onto the process-pool configuration.
    ///
    /// # Errors
    ///
    /// Returns a platform configuration failure.
    fn set_initialization_data(&mut self, data: &[u8]) -> Result<(), ErrorReport>;
    /// Connects the private process-pool configuration to `WKWebViewConfiguration`.
    ///
    /// # Errors
    ///
    /// Returns a platform configuration failure.
    fn commit_process_pool_configuration(&mut self) -> Result<(), ErrorReport>;
}

/// Inputs that must be applied before creating a `WKWebView`.
pub struct WebViewPlan {
    artifact: MacosArtifact,
    initialization: InitializationData,
    spi: MacosSpi,
}

impl core::fmt::Debug for WebViewPlan {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("WebViewPlan")
            .field("bundle", &self.artifact.bundle_path())
            .field("initialization", &self.initialization)
            .field("spi", &self.spi)
            .finish()
    }
}

impl WebViewPlan {
    /// Builds a fail-closed pre-creation plan from verified platform inputs.
    ///
    /// # Errors
    ///
    /// Rejects missing initialization parameters or a relayed payload topology.
    pub fn new(
        spi: MacosSpi,
        artifact: MacosArtifact,
        initialization: &[u8],
        topology: TransportTopology,
    ) -> Result<Self, ErrorReport> {
        topology.validate()?;
        Ok(Self {
            artifact,
            initialization: InitializationData::copy_from(initialization)?,
            spi,
        })
    }

    /// Injected-bundle path passed to the process-pool configuration.
    pub fn bundle_path(&self) -> &Path {
        self.artifact.bundle_path()
    }
    /// Initialization property list copied before `WebView` creation.
    pub fn initialization_data(&self) -> &[u8] {
        self.initialization.as_bytes()
    }
    /// SPI token proving the compatibility probe completed.
    pub const fn spi(&self) -> MacosSpi {
        self.spi
    }

    /// Applies the complete configuration in the required order before `WebView` creation.
    ///
    /// # Errors
    ///
    /// Propagates a platform configuration failure without creating a partial `WebView`.
    pub fn apply(&self, configurator: &mut impl WebViewConfigurator) -> Result<(), ErrorReport> {
        configurator.set_injected_bundle(self.bundle_path())?;
        configurator.set_initialization_data(self.initialization_data())?;
        configurator.commit_process_pool_configuration()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct HostedSession {
    generation: Generation,
    state: SessionState,
}

/// Registry routing renderer lifecycle events without exposing payload operations.
pub struct MacosHost {
    plan: WebViewPlan,
    sessions: HashMap<SessionId, HostedSession>,
}

impl MacosHost {
    /// Creates the host before any `WKWebView` exists.
    pub fn new(plan: WebViewPlan) -> Self {
        Self {
            plan,
            sessions: HashMap::new(),
        }
    }
    /// Read-only `WebView` creation plan.
    pub const fn plan(&self) -> &WebViewPlan {
        &self.plan
    }

    /// Registers a prepared session generation waiting for its renderer.
    ///
    /// # Errors
    ///
    /// Rejects duplicate identities.
    pub fn register(
        &mut self,
        session: SessionId,
        generation: Generation,
    ) -> Result<(), ErrorReport> {
        if self.sessions.contains_key(&session) {
            return Err(host_error(
                ErrorCode::InvalidStateTransition,
                "duplicate host session",
            ));
        }
        let state = SessionState::Created
            .transition(SessionEvent::Prepare)?
            .transition(SessionEvent::ResourcesReady)?;
        self.sessions
            .insert(session, HostedSession { generation, state });
        Ok(())
    }

    /// Marks the active renderer attached.
    ///
    /// # Errors
    ///
    /// Rejects unknown, stale, and invalid lifecycle transitions.
    pub fn renderer_attached(
        &mut self,
        session: SessionId,
        generation: Generation,
    ) -> Result<(), ErrorReport> {
        let hosted = self.active_mut(session, generation)?;
        hosted.state = hosted.state.transition(SessionEvent::RendererReady)?;
        Ok(())
    }

    /// Invalidates the old endpoint and allocates generation N+1 after reload or process exit.
    ///
    /// # Errors
    ///
    /// Rejects unknown, stale, and invalid lifecycle transitions.
    pub fn replace_renderer(
        &mut self,
        session: SessionId,
        generation: Generation,
    ) -> Result<Generation, ErrorReport> {
        let hosted = self.active_mut(session, generation)?;
        hosted.state = hosted
            .state
            .transition(SessionEvent::Disconnect)?
            .transition(SessionEvent::Close)?;
        let next = hosted
            .generation
            .checked_next()
            .ok_or_else(|| host_error(ErrorCode::Internal, "host generation allocation"))?;
        hosted.generation = next;
        hosted.state = SessionState::Created
            .transition(SessionEvent::Prepare)?
            .transition(SessionEvent::ResourcesReady)?;
        Ok(next)
    }

    /// Current active generation for diagnostics and event routing.
    pub fn generation(&self, session: SessionId) -> Option<Generation> {
        self.sessions
            .get(&session)
            .map(|session| session.generation)
    }

    /// Invalidates and removes one hosted session during framework teardown.
    ///
    /// # Errors
    ///
    /// Rejects unknown and stale session generations.
    pub fn unregister(
        &mut self,
        session: SessionId,
        generation: Generation,
    ) -> Result<(), ErrorReport> {
        let hosted = self.active_mut(session, generation)?;
        hosted.state = hosted
            .state
            .transition(SessionEvent::Disconnect)?
            .transition(SessionEvent::Close)?;
        self.sessions.remove(&session);
        Ok(())
    }

    fn active_mut(
        &mut self,
        session: SessionId,
        generation: Generation,
    ) -> Result<&mut HostedSession, ErrorReport> {
        let hosted = self
            .sessions
            .get_mut(&session)
            .ok_or_else(|| host_error(ErrorCode::Closed, "unknown host session"))?;
        if hosted.generation != generation {
            return Err(host_error(
                ErrorCode::StaleGeneration,
                "stale host generation",
            ));
        }
        Ok(hosted)
    }
}

fn host_error(code: ErrorCode, operation: &'static str) -> ErrorReport {
    ErrorReport::new(
        ErrorCategory::Lifecycle,
        code,
        Recoverability::ReplaceEndpoint,
        operation,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replacement_rejects_old_generation() {
        let mut sessions = HashMap::new();
        let session = SessionId::from_u128(1).unwrap();
        sessions.insert(
            session,
            HostedSession {
                generation: Generation::new(4).unwrap(),
                state: SessionState::WaitingForRenderer,
            },
        );
        let hosted = sessions.get_mut(&session).unwrap();
        hosted.state = hosted
            .state
            .transition(SessionEvent::RendererReady)
            .unwrap();
        hosted.state = hosted.state.transition(SessionEvent::Disconnect).unwrap();
        hosted.state = hosted.state.transition(SessionEvent::Close).unwrap();
        hosted.generation = hosted.generation.checked_next().unwrap();
        assert_eq!(hosted.generation.get(), 5);
    }

    #[test]
    fn unregister_closes_and_removes_generation() {
        let session = SessionId::from_u128(9).unwrap();
        let generation = Generation::new(2).unwrap();
        let mut sessions = HashMap::new();
        sessions.insert(
            session,
            HostedSession {
                generation,
                state: SessionState::WaitingForRenderer,
            },
        );
        let hosted = sessions.get_mut(&session).unwrap();
        hosted.state = hosted.state.transition(SessionEvent::Disconnect).unwrap();
        hosted.state = hosted.state.transition(SessionEvent::Close).unwrap();
        sessions.remove(&session);
        assert!(!sessions.contains_key(&session));
    }

    #[test]
    fn webview_configuration_surface_has_no_payload_operation() {
        struct Configuration(Vec<&'static str>);
        impl WebViewConfigurator for Configuration {
            fn set_injected_bundle(&mut self, _: &Path) -> Result<(), ErrorReport> {
                self.0.push("bundle");
                Ok(())
            }
            fn set_initialization_data(&mut self, _: &[u8]) -> Result<(), ErrorReport> {
                self.0.push("bootstrap");
                Ok(())
            }
            fn commit_process_pool_configuration(&mut self) -> Result<(), ErrorReport> {
                self.0.push("commit");
                Ok(())
            }
        }
        let mut configuration = Configuration(Vec::new());
        configuration
            .set_injected_bundle(Path::new("bundle"))
            .unwrap();
        configuration.set_initialization_data(b"bootstrap").unwrap();
        configuration.commit_process_pool_configuration().unwrap();
        assert_eq!(configuration.0, ["bundle", "bootstrap", "commit"]);
    }
}
