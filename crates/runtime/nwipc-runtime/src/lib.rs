//! Provider-neutral control-plane registry and generation replacement routing.

use std::collections::HashMap;

use nwipc_error::{ErrorCategory, ErrorCode, ErrorReport, Recoverability};
use nwipc_session::PreparedResources;
use nwipc_session_machine::{LifecycleEvent, MachineEffect, SessionMachine};
use nwipc_state::SessionState;
use nwipc_types::{Generation, SessionId};

/// Shared-memory backend selected for future resource preparation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MemoryBackend {
    /// Deterministic in-process or process-test memory.
    ProcessTest,
    /// macOS `IOSurface` shared memory.
    IoSurface,
    /// macOS Mach memory-entry shared memory.
    Mach,
}

/// Notification backend selected for future resource preparation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SignalBackend {
    /// Correctness polling without a primary notification.
    Poll,
    /// Darwin notify hints.
    DarwinNotify,
    /// Darwin hints with correctness polling.
    Hybrid,
    /// Mach port hints.
    Mach,
}

/// Provider combination supplied to every generation preparation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProviderSelection {
    /// Shared-memory backend.
    pub memory: MemoryBackend,
    /// Notification backend.
    pub signal: SignalBackend,
}

impl ProviderSelection {
    /// Deterministic selection used by provider-neutral tests.
    pub const PROCESS_TEST: Self = Self {
        memory: MemoryBackend::ProcessTest,
        signal: SignalBackend::Poll,
    };

    /// Production macOS provider combination.
    pub const MACOS: Self = Self {
        memory: MemoryBackend::IoSurface,
        signal: SignalBackend::Hybrid,
    };

    /// Experimental capability-transferred Mach provider combination.
    pub const MACH: Self = Self {
        memory: MemoryBackend::Mach,
        signal: SignalBackend::Mach,
    };
}

/// Prepares all mappings, signals, ports, and callbacks owned by one generation.
pub trait ResourcePreparer {
    /// Builds a complete ownership set for a generation.
    ///
    /// Implementations must retain partial acquisitions in a [`PreparedResources`] value so
    /// ordinary drop cleans them when preparation fails.
    ///
    /// # Errors
    ///
    /// Returns the typed provider or resource failure.
    fn prepare(
        &mut self,
        session_id: SessionId,
        generation: Generation,
        providers: ProviderSelection,
    ) -> Result<PreparedResources, ErrorReport>;
}

/// Generation-qualified registry key used for lifecycle routing.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct SessionHandle {
    /// Stable logical session identity.
    pub session_id: SessionId,
    /// Active generation when the handle was issued.
    pub generation: Generation,
}

/// Result of routing a lifecycle event.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RouteOutcome {
    /// Handle active after routing completes.
    pub active: SessionHandle,
    /// Whether routing prepared and installed a new generation.
    pub replaced: bool,
    /// Lifecycle effect applied to the addressed generation.
    pub effect: MachineEffect,
}

struct RegistryEntry {
    machine: SessionMachine,
}

/// Owns session identity issuance, registry isolation, and replacement policy.
pub struct Runtime<Preparer> {
    providers: ProviderSelection,
    preparer: Preparer,
    sessions: HashMap<SessionId, RegistryEntry>,
    next_session_id: u128,
    next_generation: u64,
}

impl<Preparer: ResourcePreparer> Runtime<Preparer> {
    /// Creates an empty runtime using an injected provider preparation adapter.
    pub fn new(providers: ProviderSelection, preparer: Preparer) -> Self {
        Self {
            providers,
            preparer,
            sessions: HashMap::new(),
            next_session_id: 1,
            next_generation: 1,
        }
    }

    /// Returns the configured provider combination.
    pub const fn providers(&self) -> ProviderSelection {
        self.providers
    }

    /// Creates, prepares, and registers an isolated logical session.
    ///
    /// # Errors
    ///
    /// Returns a provider preparation or monotonic identifier exhaustion error.
    pub fn create_session(&mut self) -> Result<SessionHandle, ErrorReport> {
        let session_id = self.issue_session_id()?;
        let generation = self.issue_generation()?;
        let machine = self.prepare_machine(session_id, generation)?;
        let handle = SessionHandle {
            session_id,
            generation,
        };
        self.sessions.insert(session_id, RegistryEntry { machine });
        Ok(handle)
    }

    /// Routes an event only when both session identity and generation are active.
    ///
    /// Replacement events clean the old generation before preparing the next one.
    ///
    /// # Errors
    ///
    /// Returns `StaleGeneration` for an unknown or replaced handle, plus lifecycle/provider
    /// failures from the active session.
    pub fn route(
        &mut self,
        handle: SessionHandle,
        event: LifecycleEvent,
    ) -> Result<RouteOutcome, ErrorReport> {
        let effect = {
            let entry = self.active_entry_mut(handle)?;
            entry.machine.handle(event)?
        };
        if effect == MachineEffect::ReplaceGeneration {
            let active = self.replace_session(handle.session_id)?;
            Ok(RouteOutcome {
                active,
                replaced: true,
                effect,
            })
        } else {
            Ok(RouteOutcome {
                active: handle,
                replaced: false,
                effect,
            })
        }
    }

    /// Retries replacement of an invalidated session with a fresh generation.
    ///
    /// Failed preparation generations are consumed and never reused.
    ///
    /// # Errors
    ///
    /// Returns a stale-session, state, provider, or identifier exhaustion error.
    pub fn replace(&mut self, session_id: SessionId) -> Result<SessionHandle, ErrorReport> {
        let state = self
            .sessions
            .get(&session_id)
            .ok_or_else(stale_generation)?
            .machine
            .session()
            .state();
        if !state.can_replace() {
            return Err(lifecycle_error("replace active generation"));
        }
        self.replace_session(session_id)
    }

    /// Idempotently closes the addressed active generation.
    ///
    /// # Errors
    ///
    /// Returns `StaleGeneration` when another generation is active.
    pub fn close(&mut self, handle: SessionHandle) -> Result<MachineEffect, ErrorReport> {
        let entry = self.active_entry_mut(handle)?;
        entry.machine.handle(LifecycleEvent::LocalClose)
    }

    /// Returns the active handle for a logical session.
    pub fn active_handle(&self, session_id: SessionId) -> Option<SessionHandle> {
        let entry = self.sessions.get(&session_id)?;
        Some(SessionHandle {
            session_id,
            generation: entry.machine.session().generation(),
        })
    }

    /// Returns the active generation state without exposing owned resources.
    ///
    /// # Errors
    ///
    /// Returns `StaleGeneration` unless the handle identifies the active generation.
    pub fn state(&self, handle: SessionHandle) -> Result<SessionState, ErrorReport> {
        let entry = self.active_entry(handle)?;
        Ok(entry.machine.session().state())
    }

    /// Number of isolated logical sessions retained by the registry.
    pub fn session_count(&self) -> usize {
        self.sessions.len()
    }

    fn replace_session(&mut self, session_id: SessionId) -> Result<SessionHandle, ErrorReport> {
        if !self.sessions.contains_key(&session_id) {
            return Err(stale_generation());
        }
        let generation = self.issue_generation()?;
        let machine = self.prepare_machine(session_id, generation)?;
        self.sessions.insert(session_id, RegistryEntry { machine });
        Ok(SessionHandle {
            session_id,
            generation,
        })
    }

    fn prepare_machine(
        &mut self,
        session_id: SessionId,
        generation: Generation,
    ) -> Result<SessionMachine, ErrorReport> {
        let resources = self
            .preparer
            .prepare(session_id, generation, self.providers)?;
        SessionMachine::prepare(session_id, generation, resources)
    }

    fn issue_session_id(&mut self) -> Result<SessionId, ErrorReport> {
        let value = self.next_session_id;
        self.next_session_id = value.checked_add(1).ok_or_else(identifier_exhausted)?;
        SessionId::from_u128(value).ok_or_else(identifier_exhausted)
    }

    fn issue_generation(&mut self) -> Result<Generation, ErrorReport> {
        let value = self.next_generation;
        self.next_generation = value.checked_add(1).ok_or_else(identifier_exhausted)?;
        Generation::new(value).ok_or_else(identifier_exhausted)
    }

    fn active_entry(&self, handle: SessionHandle) -> Result<&RegistryEntry, ErrorReport> {
        let entry = self
            .sessions
            .get(&handle.session_id)
            .ok_or_else(stale_generation)?;
        if entry.machine.session().generation() != handle.generation {
            return Err(stale_generation());
        }
        Ok(entry)
    }

    fn active_entry_mut(
        &mut self,
        handle: SessionHandle,
    ) -> Result<&mut RegistryEntry, ErrorReport> {
        let entry = self
            .sessions
            .get_mut(&handle.session_id)
            .ok_or_else(stale_generation)?;
        if entry.machine.session().generation() != handle.generation {
            return Err(stale_generation());
        }
        Ok(entry)
    }
}

fn stale_generation() -> ErrorReport {
    ErrorReport::new(
        ErrorCategory::Lifecycle,
        ErrorCode::StaleGeneration,
        Recoverability::ReplaceEndpoint,
        "route session generation",
    )
}

fn lifecycle_error(operation: &'static str) -> ErrorReport {
    ErrorReport::new(
        ErrorCategory::Lifecycle,
        ErrorCode::InvalidStateTransition,
        Recoverability::ReplaceEndpoint,
        operation,
    )
}

fn identifier_exhausted() -> ErrorReport {
    ErrorReport::new(
        ErrorCategory::Resource,
        ErrorCode::Internal,
        Recoverability::Terminal,
        "issue runtime identity",
    )
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use nwipc_session::OwnedResource;

    use super::*;

    #[derive(Default)]
    struct PreparationLog {
        prepared: Vec<(SessionId, Generation, ProviderSelection)>,
        cleaned: Vec<Generation>,
    }

    struct FakePreparer {
        log: Arc<Mutex<PreparationLog>>,
        fail_next: bool,
    }

    struct FakeResource {
        generation: Generation,
        log: Arc<Mutex<PreparationLog>>,
    }

    impl OwnedResource for FakeResource {
        fn cleanup(&mut self) -> Result<(), ErrorReport> {
            self.log.lock().unwrap().cleaned.push(self.generation);
            Ok(())
        }
    }

    impl ResourcePreparer for FakePreparer {
        fn prepare(
            &mut self,
            session_id: SessionId,
            generation: Generation,
            providers: ProviderSelection,
        ) -> Result<PreparedResources, ErrorReport> {
            self.log
                .lock()
                .unwrap()
                .prepared
                .push((session_id, generation, providers));
            let mut resources = PreparedResources::new();
            resources.push(FakeResource {
                generation,
                log: Arc::clone(&self.log),
            });
            if self.fail_next {
                self.fail_next = false;
                return Err(ErrorReport::new(
                    ErrorCategory::Resource,
                    ErrorCode::Internal,
                    Recoverability::ReplaceEndpoint,
                    "fake preparation",
                ));
            }
            Ok(resources)
        }
    }

    fn runtime() -> (Runtime<FakePreparer>, Arc<Mutex<PreparationLog>>) {
        let log = Arc::new(Mutex::new(PreparationLog::default()));
        (
            Runtime::new(
                ProviderSelection::PROCESS_TEST,
                FakePreparer {
                    log: Arc::clone(&log),
                    fail_next: false,
                },
            ),
            log,
        )
    }

    fn open(runtime: &mut Runtime<FakePreparer>, handle: SessionHandle) {
        for event in [
            LifecycleEvent::RendererAttached,
            LifecycleEvent::PeerAttached,
            LifecycleEvent::HandshakeCompleted,
        ] {
            runtime.route(handle, event).unwrap();
        }
    }

    #[test]
    fn sessions_receive_unique_identities_and_generations() {
        let (mut runtime, _) = runtime();
        let first = runtime.create_session().unwrap();
        let second = runtime.create_session().unwrap();
        assert_ne!(first.session_id, second.session_id);
        assert_ne!(first.generation, second.generation);
        assert_eq!(runtime.session_count(), 2);
    }

    #[test]
    fn mach_selection_is_explicit_and_provider_neutral() {
        assert_eq!(ProviderSelection::MACH.memory, MemoryBackend::Mach);
        assert_eq!(ProviderSelection::MACH.signal, SignalBackend::Mach);
        assert_ne!(ProviderSelection::MACH, ProviderSelection::MACOS);
    }

    #[test]
    fn endpoint_exit_replaces_after_old_resource_cleanup() {
        let (mut runtime, log) = runtime();
        let old = runtime.create_session().unwrap();
        open(&mut runtime, old);

        let outcome = runtime.route(old, LifecycleEvent::RendererExited).unwrap();

        assert!(outcome.replaced);
        assert_ne!(outcome.active.generation, old.generation);
        assert_eq!(log.lock().unwrap().cleaned, [old.generation]);
        assert_eq!(
            runtime
                .route(old, LifecycleEvent::RendererAttached)
                .unwrap_err()
                .code(),
            ErrorCode::StaleGeneration
        );
    }

    #[test]
    fn protocol_violation_routes_to_a_clean_generation() {
        let (mut runtime, log) = runtime();
        let old = runtime.create_session().unwrap();
        open(&mut runtime, old);

        let outcome = runtime
            .route(old, LifecycleEvent::ProtocolViolation)
            .unwrap();

        assert!(outcome.replaced);
        assert_eq!(
            runtime.state(outcome.active).unwrap(),
            SessionState::WaitingForRenderer
        );
        assert_eq!(log.lock().unwrap().cleaned, [old.generation]);
    }

    #[test]
    fn failed_replacement_generation_is_not_reused() {
        let (mut runtime, log) = runtime();
        let old = runtime.create_session().unwrap();
        open(&mut runtime, old);
        runtime.preparer.fail_next = true;

        runtime.route(old, LifecycleEvent::PeerExited).unwrap_err();
        let failed_generation = log.lock().unwrap().prepared.last().unwrap().1;
        let replacement = runtime.replace(old.session_id).unwrap();

        assert_ne!(replacement.generation, failed_generation);
        assert!(log.lock().unwrap().cleaned.contains(&failed_generation));
    }

    #[test]
    fn routing_and_close_are_isolated_per_session() {
        let (mut runtime, _) = runtime();
        let first = runtime.create_session().unwrap();
        let second = runtime.create_session().unwrap();
        runtime
            .route(first, LifecycleEvent::RendererAttached)
            .unwrap();

        assert_eq!(runtime.state(first).unwrap(), SessionState::WaitingForPeer);
        assert_eq!(
            runtime.state(second).unwrap(),
            SessionState::WaitingForRenderer
        );
        assert_eq!(runtime.close(first).unwrap(), MachineEffect::Closed);
        assert_eq!(runtime.close(first).unwrap(), MachineEffect::None);
        assert_eq!(
            runtime.state(second).unwrap(),
            SessionState::WaitingForRenderer
        );
    }

    #[test]
    fn runtime_drop_cleans_every_active_session() {
        let (mut runtime, log) = runtime();
        let first = runtime.create_session().unwrap();
        let second = runtime.create_session().unwrap();
        drop(runtime);

        let cleaned = &log.lock().unwrap().cleaned;
        assert!(cleaned.contains(&first.generation));
        assert!(cleaned.contains(&second.generation));
    }
}
