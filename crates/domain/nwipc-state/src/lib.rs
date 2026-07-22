//! Session state and the canonical transition table.

use nwipc_error::{ErrorCategory, ErrorCode, ErrorReport, Recoverability};

/// Lifecycle state of one session generation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SessionState {
    /// Identity exists but preparation has not started.
    Created,
    /// Resources are being prepared.
    Preparing,
    /// Resources are ready and the renderer has not attached.
    WaitingForRenderer,
    /// The renderer is ready and the native peer has not attached.
    WaitingForPeer,
    /// Both endpoints are negotiating the wire protocol.
    Handshaking,
    /// The data plane is ready.
    Open,
    /// Graceful shutdown is draining accepted work.
    Draining,
    /// An endpoint disconnected and this generation may be replaced.
    Disconnected,
    /// The generation failed and cannot be reused.
    Failed,
    /// Shutdown completed.
    Closed,
}

impl SessionState {
    /// Returns whether no further event is accepted for this generation.
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Failed | Self::Closed)
    }

    /// Returns whether runtime policy may replace this generation.
    pub const fn can_replace(self) -> bool {
        matches!(self, Self::Disconnected | Self::Failed | Self::Closed)
    }

    /// Applies an event through the canonical transition table.
    ///
    /// # Errors
    ///
    /// Returns a stable lifecycle error when the event is invalid in this state.
    pub fn transition(self, event: SessionEvent) -> Result<Self, ErrorReport> {
        use SessionEvent::{
            BeginDrain, Close, Disconnect, Fail, HandshakeComplete, PeerReady, Prepare,
            RendererReady, ResourcesReady,
        };
        let next = match (self, event) {
            (Self::Created, Prepare) => Self::Preparing,
            (Self::Preparing, ResourcesReady) => Self::WaitingForRenderer,
            (Self::WaitingForRenderer, RendererReady) => Self::WaitingForPeer,
            (Self::WaitingForPeer, PeerReady) => Self::Handshaking,
            (Self::Handshaking, HandshakeComplete) => Self::Open,
            (Self::Open, BeginDrain) => Self::Draining,
            (Self::Draining | Self::Disconnected, Close) => Self::Closed,
            (
                Self::WaitingForRenderer
                | Self::WaitingForPeer
                | Self::Handshaking
                | Self::Open
                | Self::Draining,
                Disconnect,
            ) => Self::Disconnected,
            (state, Fail) if !state.is_terminal() => Self::Failed,
            _ => return Err(invalid_transition()),
        };
        Ok(next)
    }
}

/// Events accepted by the session transition table.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SessionEvent {
    /// Resource preparation started.
    Prepare,
    /// All generation-bound resources are ready.
    ResourcesReady,
    /// The renderer attached to this generation.
    RendererReady,
    /// The native peer attached to this generation.
    PeerReady,
    /// HELLO/ACK negotiation completed.
    HandshakeComplete,
    /// Graceful shutdown started.
    BeginDrain,
    /// An attached endpoint disappeared.
    Disconnect,
    /// A failure invalidated the generation.
    Fail,
    /// Draining or disconnected cleanup completed.
    Close,
}

fn invalid_transition() -> ErrorReport {
    ErrorReport::new(
        ErrorCategory::Lifecycle,
        ErrorCode::InvalidStateTransition,
        Recoverability::ReplaceEndpoint,
        "invalid session transition",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const STATES: [SessionState; 10] = [
        SessionState::Created,
        SessionState::Preparing,
        SessionState::WaitingForRenderer,
        SessionState::WaitingForPeer,
        SessionState::Handshaking,
        SessionState::Open,
        SessionState::Draining,
        SessionState::Disconnected,
        SessionState::Failed,
        SessionState::Closed,
    ];
    const EVENTS: [SessionEvent; 9] = [
        SessionEvent::Prepare,
        SessionEvent::ResourcesReady,
        SessionEvent::RendererReady,
        SessionEvent::PeerReady,
        SessionEvent::HandshakeComplete,
        SessionEvent::BeginDrain,
        SessionEvent::Disconnect,
        SessionEvent::Fail,
        SessionEvent::Close,
    ];

    #[test]
    fn happy_path_reaches_closed() {
        let events = [
            SessionEvent::Prepare,
            SessionEvent::ResourcesReady,
            SessionEvent::RendererReady,
            SessionEvent::PeerReady,
            SessionEvent::HandshakeComplete,
            SessionEvent::BeginDrain,
            SessionEvent::Close,
        ];
        let state = events
            .into_iter()
            .try_fold(SessionState::Created, SessionState::transition);
        assert_eq!(state.unwrap(), SessionState::Closed);
    }

    #[test]
    fn transition_matrix_matches_the_contract() {
        for state in STATES {
            for event in EVENTS {
                let allowed = matches!(
                    (state, event),
                    (SessionState::Created, SessionEvent::Prepare)
                        | (SessionState::Preparing, SessionEvent::ResourcesReady)
                        | (
                            SessionState::WaitingForRenderer,
                            SessionEvent::RendererReady
                        )
                        | (SessionState::WaitingForPeer, SessionEvent::PeerReady)
                        | (SessionState::Handshaking, SessionEvent::HandshakeComplete)
                        | (SessionState::Open, SessionEvent::BeginDrain)
                        | (
                            SessionState::Draining | SessionState::Disconnected,
                            SessionEvent::Close
                        )
                        | (
                            SessionState::WaitingForRenderer
                                | SessionState::WaitingForPeer
                                | SessionState::Handshaking
                                | SessionState::Open
                                | SessionState::Draining,
                            SessionEvent::Disconnect
                        )
                ) || (event == SessionEvent::Fail && !state.is_terminal());
                assert_eq!(
                    state.transition(event).is_ok(),
                    allowed,
                    "state={state:?}, event={event:?}"
                );
            }
        }
    }

    #[test]
    fn terminal_states_reject_every_event() {
        for state in [SessionState::Failed, SessionState::Closed] {
            for event in EVENTS {
                let error = state.transition(event).unwrap_err();
                assert_eq!(error.code(), ErrorCode::InvalidStateTransition);
            }
        }
    }
}
