//! Lifecycle orchestration and side effects for one session generation.

use nwipc_error::ErrorReport;
use nwipc_session::{EndpointKind, PreparedResources, Session};
use nwipc_state::{SessionEvent, SessionState};
use nwipc_types::{Generation, SessionId};

/// Control-plane events routed from endpoint and host lifecycle adapters.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LifecycleEvent {
    /// The renderer attached to the prepared generation.
    RendererAttached,
    /// The native peer attached to the prepared generation.
    PeerAttached,
    /// Protocol negotiation completed.
    HandshakeCompleted,
    /// The renderer document was replaced.
    DocumentReplaced,
    /// The renderer endpoint exited.
    RendererExited,
    /// The peer endpoint exited.
    PeerExited,
    /// A protocol violation invalidated the generation.
    ProtocolViolation,
    /// The host requested graceful close.
    LocalClose,
}

/// Observable result of applying one lifecycle event.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MachineEffect {
    /// State advanced without replacing or closing the generation.
    StateChanged,
    /// The old generation was cleaned and runtime policy must prepare a replacement.
    ReplaceGeneration,
    /// The generation is terminal and its resources are cleaned.
    Closed,
    /// A repeated terminal event had no additional side effect.
    None,
}

/// Executes canonical transitions and generation-scoped cleanup.
pub struct SessionMachine {
    session: Session,
}

impl SessionMachine {
    /// Creates and prepares one session generation.
    ///
    /// On failure all partially prepared resources are cleaned by their owner.
    ///
    /// # Errors
    ///
    /// Returns a lifecycle error when the canonical preparation transitions fail.
    pub fn prepare(
        identity: SessionId,
        generation: Generation,
        resources: PreparedResources,
    ) -> Result<Self, ErrorReport> {
        let mut session = Session::new(identity, generation);
        session.transition(SessionEvent::Prepare)?;
        session.install_resources(resources)?;
        Ok(Self { session })
    }

    /// Borrows the owned session aggregate.
    pub const fn session(&self) -> &Session {
        &self.session
    }

    /// Applies an endpoint or host lifecycle event and its cleanup side effects.
    ///
    /// # Errors
    ///
    /// Returns a stable lifecycle or resource-cleanup error.
    pub fn handle(&mut self, event: LifecycleEvent) -> Result<MachineEffect, ErrorReport> {
        if matches!(event, LifecycleEvent::LocalClose) {
            return self.close();
        }
        if self.session.state().is_terminal() {
            return Ok(MachineEffect::None);
        }

        match event {
            LifecycleEvent::RendererAttached => {
                self.session.transition(SessionEvent::RendererReady)?;
                self.session.attach_endpoint(EndpointKind::Renderer);
                Ok(MachineEffect::StateChanged)
            }
            LifecycleEvent::PeerAttached => {
                self.session.transition(SessionEvent::PeerReady)?;
                self.session.attach_endpoint(EndpointKind::Peer);
                Ok(MachineEffect::StateChanged)
            }
            LifecycleEvent::HandshakeCompleted => {
                self.session.transition(SessionEvent::HandshakeComplete)?;
                Ok(MachineEffect::StateChanged)
            }
            LifecycleEvent::ProtocolViolation => {
                self.session.transition(SessionEvent::Fail)?;
                self.session.cleanup()?;
                Ok(MachineEffect::ReplaceGeneration)
            }
            LifecycleEvent::DocumentReplaced
            | LifecycleEvent::RendererExited
            | LifecycleEvent::PeerExited => {
                self.session.transition(SessionEvent::Disconnect)?;
                self.session.cleanup()?;
                Ok(MachineEffect::ReplaceGeneration)
            }
            LifecycleEvent::LocalClose => unreachable!(),
        }
    }

    fn close(&mut self) -> Result<MachineEffect, ErrorReport> {
        match self.session.state() {
            SessionState::Closed | SessionState::Failed => return Ok(MachineEffect::None),
            SessionState::Open => self.session.transition(SessionEvent::BeginDrain)?,
            SessionState::Draining | SessionState::Disconnected => {}
            _ => {
                self.session.transition(SessionEvent::Fail)?;
                self.session.cleanup()?;
                return Ok(MachineEffect::Closed);
            }
        }
        self.session.cleanup()?;
        self.session.transition(SessionEvent::Close)?;
        Ok(MachineEffect::Closed)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use nwipc_session::OwnedResource;

    use super::*;

    struct CleanupCounter(Arc<Mutex<u32>>);

    impl OwnedResource for CleanupCounter {
        fn cleanup(&mut self) -> Result<(), ErrorReport> {
            *self.0.lock().unwrap() += 1;
            Ok(())
        }
    }

    fn machine(counter: &Arc<Mutex<u32>>) -> SessionMachine {
        let mut resources = PreparedResources::new();
        resources.push(CleanupCounter(Arc::clone(counter)));
        SessionMachine::prepare(
            SessionId::from_u128(1).unwrap(),
            Generation::new(1).unwrap(),
            resources,
        )
        .unwrap()
    }

    fn open(machine: &mut SessionMachine) {
        for event in [
            LifecycleEvent::RendererAttached,
            LifecycleEvent::PeerAttached,
            LifecycleEvent::HandshakeCompleted,
        ] {
            machine.handle(event).unwrap();
        }
    }

    #[test]
    fn attachment_sequence_reaches_open() {
        let counter = Arc::new(Mutex::new(0));
        let mut machine = machine(&counter);
        open(&mut machine);
        assert_eq!(machine.session().state(), SessionState::Open);
        assert_eq!(
            machine.session().endpoint_status(EndpointKind::Renderer),
            nwipc_session::EndpointStatus::Attached
        );
    }

    #[test]
    fn endpoint_exit_invalidates_and_cleans_before_replacement() {
        let counter = Arc::new(Mutex::new(0));
        let mut machine = machine(&counter);
        open(&mut machine);

        assert_eq!(
            machine.handle(LifecycleEvent::PeerExited).unwrap(),
            MachineEffect::ReplaceGeneration
        );
        assert_eq!(machine.session().state(), SessionState::Disconnected);
        assert_eq!(*counter.lock().unwrap(), 1);
        assert!(machine.session().resources_cleaned());
    }

    #[test]
    fn document_replacement_and_protocol_failure_are_terminal_for_resources() {
        for (event, state) in [
            (LifecycleEvent::DocumentReplaced, SessionState::Disconnected),
            (LifecycleEvent::ProtocolViolation, SessionState::Failed),
        ] {
            let counter = Arc::new(Mutex::new(0));
            let mut machine = machine(&counter);
            open(&mut machine);

            assert_eq!(
                machine.handle(event).unwrap(),
                MachineEffect::ReplaceGeneration
            );
            assert_eq!(machine.session().state(), state);
            assert_eq!(*counter.lock().unwrap(), 1);
        }
    }

    #[test]
    fn duplicate_close_has_one_cleanup_side_effect() {
        let counter = Arc::new(Mutex::new(0));
        let mut machine = machine(&counter);
        open(&mut machine);

        assert_eq!(
            machine.handle(LifecycleEvent::LocalClose).unwrap(),
            MachineEffect::Closed
        );
        assert_eq!(
            machine.handle(LifecycleEvent::LocalClose).unwrap(),
            MachineEffect::None
        );
        assert_eq!(*counter.lock().unwrap(), 1);
        assert_eq!(machine.session().state(), SessionState::Closed);
    }

    #[test]
    fn close_during_partial_attach_cleans_resources() {
        let counter = Arc::new(Mutex::new(0));
        let mut machine = machine(&counter);
        machine.handle(LifecycleEvent::RendererAttached).unwrap();

        assert_eq!(
            machine.handle(LifecycleEvent::LocalClose).unwrap(),
            MachineEffect::Closed
        );
        assert_eq!(machine.session().state(), SessionState::Failed);
        assert_eq!(*counter.lock().unwrap(), 1);
    }
}
