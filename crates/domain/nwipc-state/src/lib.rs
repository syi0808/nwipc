//! Session state and the single allowed transition table.

use nwipc_error::{ErrorCategory, ErrorCode, ErrorReport, Recoverability};

/// Lifecycle state of one session generation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SessionState {
    /// Identity exists but resources do not.
    Created,
    /// Resources have been prepared.
    Prepared,
    /// An endpoint is attaching to prepared resources.
    Attaching,
    /// Endpoints are negotiating protocol state.
    Handshaking,
    /// The data plane is ready.
    Open,
    /// Graceful shutdown is in progress.
    Closing,
    /// Shutdown completed.
    Closed,
    /// The generation failed and cannot be reused.
    Failed,
}

impl SessionState {
    /// Returns whether no further event is accepted for this generation.
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Closed | Self::Failed)
    }

    /// Returns whether a new generation may replace this one.
    pub const fn can_replace(self) -> bool {
        matches!(self, Self::Closed | Self::Failed)
    }

    /// Applies an event through the canonical transition table.
    ///
    /// # Errors
    ///
    /// Returns a stable transition error when the event is not valid in this state.
    pub fn transition(self, event: SessionEvent) -> Result<Self, ErrorReport> {
        use SessionEvent::{Attach, Close, Fail, HandshakeComplete, Prepare};
        let next = match (self, event) {
            (Self::Created, Prepare) => Self::Prepared,
            (Self::Prepared, Attach) => Self::Attaching,
            (Self::Attaching, HandshakeComplete) => Self::Handshaking,
            (Self::Handshaking, HandshakeComplete) => Self::Open,
            (Self::Open, Close) => Self::Closing,
            (Self::Closing, Close) => Self::Closed,
            (state, Fail) if !state.is_terminal() => Self::Failed,
            _ => {
                return Err(ErrorReport::new(
                    ErrorCategory::Protocol,
                    ErrorCode::InvalidStateTransition,
                    Recoverability::ReplaceEndpoint,
                    "invalid session transition",
                ));
            }
        };
        Ok(next)
    }
}

/// Events accepted by the session transition table.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SessionEvent {
    /// Resources are ready.
    Prepare,
    /// Endpoint attachment started.
    Attach,
    /// One handshake stage completed.
    HandshakeComplete,
    /// Graceful shutdown advanced.
    Close,
    /// A failure invalidated this generation.
    Fail,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn happy_path_reaches_closed() {
        let events = [
            SessionEvent::Prepare,
            SessionEvent::Attach,
            SessionEvent::HandshakeComplete,
            SessionEvent::HandshakeComplete,
            SessionEvent::Close,
            SessionEvent::Close,
        ];
        let state = events
            .into_iter()
            .try_fold(SessionState::Created, SessionState::transition);
        assert_eq!(state.unwrap(), SessionState::Closed);
    }

    #[test]
    fn terminal_state_rejects_more_events() {
        let error = SessionState::Closed
            .transition(SessionEvent::Close)
            .unwrap_err();
        assert_eq!(error.code(), ErrorCode::InvalidStateTransition);
    }
}
