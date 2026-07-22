//! Generation-scoped session identity, endpoint state, and resource ownership.

use nwipc_error::ErrorReport;
use nwipc_state::{SessionEvent, SessionState};
use nwipc_types::{Generation, SessionId};

/// Endpoint participating in a session generation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EndpointKind {
    /// Web renderer endpoint.
    Renderer,
    /// Native peer endpoint.
    Peer,
}

/// Attachment status of one endpoint.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EndpointStatus {
    /// The endpoint has not attached.
    Detached,
    /// The endpoint is attached to this generation.
    Attached,
    /// The generation was invalidated and the endpoint must not be reused.
    Invalidated,
}

/// A generation-bound resource with an idempotent release operation.
pub trait OwnedResource: Send + 'static {
    /// Releases the native or callback resource.
    ///
    /// Implementations must accept repeated calls. The owner also guarantees that it invokes
    /// this method at most once.
    ///
    /// # Errors
    ///
    /// Returns a typed provider error when release fails.
    fn cleanup(&mut self) -> Result<(), ErrorReport>;
}

/// Resources prepared together for one generation.
#[derive(Default)]
pub struct PreparedResources {
    resources: Vec<Box<dyn OwnedResource>>,
    cleaned: bool,
}

impl PreparedResources {
    /// Creates an empty preparation set.
    pub const fn new() -> Self {
        Self {
            resources: Vec::new(),
            cleaned: false,
        }
    }

    /// Adds a resource to the generation ownership set.
    pub fn push(&mut self, resource: impl OwnedResource) {
        self.resources.push(Box::new(resource));
    }

    /// Returns the number of owned resources.
    pub fn len(&self) -> usize {
        self.resources.len()
    }

    /// Returns whether the ownership set is empty.
    pub fn is_empty(&self) -> bool {
        self.resources.is_empty()
    }

    /// Releases every resource exactly once and returns the first cleanup failure.
    ///
    /// # Errors
    ///
    /// Returns the first provider failure after attempting to release every resource.
    pub fn cleanup(&mut self) -> Result<(), ErrorReport> {
        if self.cleaned {
            return Ok(());
        }
        self.cleaned = true;
        let mut first_error = None;
        for resource in self.resources.iter_mut().rev() {
            if let Err(error) = resource.cleanup() {
                if first_error.is_none() {
                    first_error = Some(error);
                }
            }
        }
        first_error.map_or(Ok(()), Err)
    }

    /// Returns whether cleanup has already run.
    pub const fn is_cleaned(&self) -> bool {
        self.cleaned
    }
}

impl Drop for PreparedResources {
    fn drop(&mut self) {
        let _ = self.cleanup();
    }
}

/// Aggregate owned by the control plane for one session generation.
pub struct Session {
    identity: SessionId,
    generation: Generation,
    state: SessionState,
    renderer: EndpointStatus,
    peer: EndpointStatus,
    resources: Option<PreparedResources>,
}

impl Session {
    /// Creates an unprepared generation.
    pub const fn new(identity: SessionId, generation: Generation) -> Self {
        Self {
            identity,
            generation,
            state: SessionState::Created,
            renderer: EndpointStatus::Detached,
            peer: EndpointStatus::Detached,
            resources: None,
        }
    }

    /// Session identity shared by all generations.
    pub const fn identity(&self) -> SessionId {
        self.identity
    }

    /// Active resource generation.
    pub const fn generation(&self) -> Generation {
        self.generation
    }

    /// Current canonical lifecycle state.
    pub const fn state(&self) -> SessionState {
        self.state
    }

    /// Returns one endpoint's generation-scoped status.
    pub const fn endpoint_status(&self, endpoint: EndpointKind) -> EndpointStatus {
        match endpoint {
            EndpointKind::Renderer => self.renderer,
            EndpointKind::Peer => self.peer,
        }
    }

    /// Applies a canonical state transition.
    ///
    /// # Errors
    ///
    /// Returns the stable lifecycle error for an invalid transition.
    pub fn transition(&mut self, event: SessionEvent) -> Result<(), ErrorReport> {
        self.state = self.state.transition(event)?;
        Ok(())
    }

    /// Installs resources after preparation has begun.
    ///
    /// On failure the passed ownership set is dropped and cleaned.
    ///
    /// # Errors
    ///
    /// Returns a lifecycle error unless the session is preparing and has no resources.
    pub fn install_resources(&mut self, resources: PreparedResources) -> Result<(), ErrorReport> {
        self.transition(SessionEvent::ResourcesReady)?;
        self.resources = Some(resources);
        Ok(())
    }

    /// Marks an endpoint attached to this generation.
    pub fn attach_endpoint(&mut self, endpoint: EndpointKind) {
        match endpoint {
            EndpointKind::Renderer => self.renderer = EndpointStatus::Attached,
            EndpointKind::Peer => self.peer = EndpointStatus::Attached,
        }
    }

    /// Invalidates both endpoint identities before resources are released.
    pub fn invalidate_endpoints(&mut self) {
        self.renderer = EndpointStatus::Invalidated;
        self.peer = EndpointStatus::Invalidated;
    }

    /// Idempotently invalidates endpoints and releases generation resources.
    ///
    /// # Errors
    ///
    /// Returns the first resource cleanup failure after attempting all releases.
    pub fn cleanup(&mut self) -> Result<(), ErrorReport> {
        self.invalidate_endpoints();
        self.resources
            .as_mut()
            .map_or(Ok(()), PreparedResources::cleanup)
    }

    /// Returns whether all installed generation resources have been cleaned.
    pub fn resources_cleaned(&self) -> bool {
        self.resources
            .as_ref()
            .is_none_or(PreparedResources::is_cleaned)
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = self.cleanup();
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;

    struct CountedResource(Arc<Mutex<Vec<u8>>>, u8);

    impl OwnedResource for CountedResource {
        fn cleanup(&mut self) -> Result<(), ErrorReport> {
            self.0.lock().unwrap().push(self.1);
            Ok(())
        }
    }

    fn session() -> Session {
        Session::new(
            SessionId::from_u128(1).unwrap(),
            Generation::new(1).unwrap(),
        )
    }

    #[test]
    fn cleanup_is_reverse_ordered_and_idempotent() {
        let cleaned = Arc::new(Mutex::new(Vec::new()));
        let mut resources = PreparedResources::new();
        resources.push(CountedResource(Arc::clone(&cleaned), 1));
        resources.push(CountedResource(Arc::clone(&cleaned), 2));
        let mut session = session();
        session.transition(SessionEvent::Prepare).unwrap();
        session.install_resources(resources).unwrap();

        session.cleanup().unwrap();
        session.cleanup().unwrap();

        assert_eq!(*cleaned.lock().unwrap(), [2, 1]);
        assert!(session.resources_cleaned());
        assert_eq!(
            session.endpoint_status(EndpointKind::Renderer),
            EndpointStatus::Invalidated
        );
    }

    #[test]
    fn rejected_install_cleans_partial_preparation() {
        let cleaned = Arc::new(Mutex::new(Vec::new()));
        let mut resources = PreparedResources::new();
        resources.push(CountedResource(Arc::clone(&cleaned), 1));

        session().install_resources(resources).unwrap_err();

        assert_eq!(*cleaned.lock().unwrap(), [1]);
    }
}
