//! Bidirectional in-process channel composed from two opposite-owner SPSC rings.
//!
//! Notifications are deliberately absent from the correctness path. [`ChannelSend`] tells an
//! adapter when an empty-to-non-empty hint is useful, while [`ChannelEndpoint::receive`] always
//! inspects acquired cursors and drains records without requiring that hint.

use nwipc_atomic::in_process_ring;
use nwipc_error::{ErrorCategory, ErrorCode, ErrorReport, Recoverability};
use nwipc_flow_control::{FlowControl, FlowUpdate};
use nwipc_record::{RecordFlags, RecordKind};
use nwipc_ring_reader::{OwnedRecord, RingReader};
use nwipc_ring_writer::{RingWriter, SendOutcome};

/// Creates two endpoints connected by opposite-direction SPSC rings.
///
/// # Errors
///
/// Returns `InvalidRange` for invalid capacity or watermark configuration.
pub fn in_process_channel(
    capacity: u32,
    maximum_inline_message: u32,
    low_watermark: u32,
    high_watermark: u32,
) -> Result<(ChannelEndpoint, ChannelEndpoint), ErrorReport> {
    let (a_to_b_producer, a_to_b_consumer) = in_process_ring(capacity)?;
    let (b_to_a_producer, b_to_a_consumer) = in_process_ring(capacity)?;
    let endpoint_a = ChannelEndpoint {
        writer: RingWriter::new(a_to_b_producer, maximum_inline_message),
        reader: RingReader::new(b_to_a_consumer, maximum_inline_message),
        flow: FlowControl::new(capacity, low_watermark, high_watermark)?,
        local_closed: false,
        remote_closed: false,
    };
    let endpoint_b = ChannelEndpoint {
        writer: RingWriter::new(b_to_a_producer, maximum_inline_message),
        reader: RingReader::new(a_to_b_consumer, maximum_inline_message),
        flow: FlowControl::new(capacity, low_watermark, high_watermark)?,
        local_closed: false,
        remote_closed: false,
    };
    Ok((endpoint_a, endpoint_b))
}

/// One side of a bidirectional channel.
pub struct ChannelEndpoint {
    writer: RingWriter,
    reader: RingReader,
    flow: FlowControl,
    local_closed: bool,
    remote_closed: bool,
}

impl ChannelEndpoint {
    /// Sends one complete application message.
    ///
    /// # Errors
    ///
    /// Returns `Closed`, `MessageTooLarge`, `Backpressured`, or a shared-state error.
    pub fn send(&mut self, payload: &[u8]) -> Result<ChannelSend, ErrorReport> {
        if self.local_closed || self.remote_closed {
            return Err(channel_error(ErrorCode::Closed, Recoverability::Terminal));
        }
        let flow = self.refresh_flow()?;
        if flow.backpressured {
            return Err(channel_error(
                ErrorCode::Backpressured,
                Recoverability::Retryable,
            ));
        }
        let outcome = self
            .writer
            .send(RecordKind::Data, RecordFlags::END_OF_MESSAGE, payload)?;
        self.flow.update(outcome.buffered_amount)?;
        Ok(ChannelSend::from(outcome))
    }

    /// Sends a graceful close after previously accepted records.
    ///
    /// # Errors
    ///
    /// Returns a typed error if already closed or the close record cannot be queued.
    pub fn close(&mut self) -> Result<ChannelSend, ErrorReport> {
        if self.local_closed {
            return Err(channel_error(ErrorCode::Closed, Recoverability::Terminal));
        }
        let outcome = self
            .writer
            .send(RecordKind::Close, RecordFlags::END_OF_MESSAGE, &[])?;
        self.local_closed = true;
        Ok(ChannelSend::from(outcome))
    }

    /// Sends an immediate reset marker for this channel generation.
    ///
    /// # Errors
    ///
    /// Returns a typed error if already closed or the reset cannot be queued.
    pub fn reset(&mut self) -> Result<ChannelSend, ErrorReport> {
        if self.local_closed {
            return Err(channel_error(ErrorCode::Closed, Recoverability::Terminal));
        }
        let outcome = self
            .writer
            .send(RecordKind::Reset, RecordFlags::END_OF_MESSAGE, &[])?;
        self.local_closed = true;
        Ok(ChannelSend::from(outcome))
    }

    /// Receives and consumes one record based only on committed cursors.
    ///
    /// Call repeatedly until `None` after any signal, poll tick, or application opportunity.
    ///
    /// # Errors
    ///
    /// Returns a validation error for malformed or out-of-sequence shared records.
    pub fn receive(&mut self) -> Result<Option<ChannelEvent>, ErrorReport> {
        let Some(record) = self.reader.receive()? else {
            return Ok(None);
        };
        let event = match record.kind() {
            RecordKind::Data => ChannelEvent::Message(record.payload),
            RecordKind::Close => {
                self.remote_closed = true;
                ChannelEvent::Closed
            }
            RecordKind::Reset => {
                self.remote_closed = true;
                ChannelEvent::Reset
            }
            kind => ChannelEvent::Control(ControlRecord { kind, record }),
        };
        Ok(Some(event))
    }

    /// Re-observes byte occupancy and reports the backpressured-to-writable edge once.
    ///
    /// # Errors
    ///
    /// Returns `InvalidCursor` if shared cursor state is corrupt.
    pub fn refresh_flow(&mut self) -> Result<FlowUpdate, ErrorReport> {
        self.flow.update(self.writer.buffered_amount()?)
    }

    /// Returns whether either side has closed this endpoint.
    pub const fn is_closed(&self) -> bool {
        self.local_closed || self.remote_closed
    }
}

/// Sender result consumed by a signal adapter.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ChannelSend {
    /// Current committed byte count in this direction.
    pub buffered_amount: u32,
    /// Whether an empty-to-non-empty notification hint should be posted.
    pub notify: bool,
}

impl From<SendOutcome> for ChannelSend {
    fn from(outcome: SendOutcome) -> Self {
        Self {
            buffered_amount: outcome.buffered_amount,
            notify: outcome.signal_non_empty,
        }
    }
}

/// Received channel event.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ChannelEvent {
    /// One complete FIFO application message.
    Message(Vec<u8>),
    /// Graceful remote close.
    Closed,
    /// Immediate remote reset.
    Reset,
    /// A known or future non-application record.
    Control(ControlRecord),
}

/// Preserved control record not interpreted by channel core.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ControlRecord {
    /// Wire record kind.
    pub kind: RecordKind,
    /// Complete owned record.
    pub record: OwnedRecord,
}

fn channel_error(code: ErrorCode, recoverability: Recoverability) -> ErrorReport {
    ErrorReport::new(
        if code == ErrorCode::Closed {
            ErrorCategory::Closed
        } else {
            ErrorCategory::Resource
        },
        code,
        recoverability,
        "channel send",
    )
}

#[cfg(test)]
mod tests {
    use nwipc_testkit::{FakeSignal, FakeSignalMode};

    use super::*;

    fn channel() -> (ChannelEndpoint, ChannelEndpoint) {
        in_process_channel(128, 64, 32, 96).unwrap()
    }

    #[test]
    fn exchanges_bidirectional_fifo_messages() {
        let (mut endpoint_a, mut endpoint_b) = channel();
        endpoint_a.send(b"one").unwrap();
        endpoint_a.send(b"two").unwrap();
        endpoint_b.send(b"reply").unwrap();
        assert_eq!(
            endpoint_b.receive().unwrap(),
            Some(ChannelEvent::Message(b"one".to_vec()))
        );
        assert_eq!(
            endpoint_b.receive().unwrap(),
            Some(ChannelEvent::Message(b"two".to_vec()))
        );
        assert_eq!(
            endpoint_a.receive().unwrap(),
            Some(ChannelEvent::Message(b"reply".to_vec()))
        );
    }

    #[test]
    fn dropped_signal_does_not_prevent_progress() {
        let (mut endpoint_a, mut endpoint_b) = channel();
        let mut signal = FakeSignal::new(FakeSignalMode::Drop);
        let sent = endpoint_a.send(b"without hint").unwrap();
        if sent.notify {
            signal.notify();
        }
        assert!(!signal.try_wait());
        assert_eq!(
            endpoint_b.receive().unwrap(),
            Some(ChannelEvent::Message(b"without hint".to_vec()))
        );
    }

    #[test]
    fn coalesced_and_duplicate_hints_do_not_change_delivery() {
        let (mut endpoint_a, mut endpoint_b) = channel();
        let mut signal = FakeSignal::new(FakeSignalMode::Coalesce);
        for payload in [b"first".as_slice(), b"second"] {
            if endpoint_a.send(payload).unwrap().notify {
                signal.notify();
            }
        }
        signal.notify();
        assert_eq!(signal.pending(), 1);
        assert!(signal.try_wait());
        assert!(matches!(
            endpoint_b.receive().unwrap(),
            Some(ChannelEvent::Message(_))
        ));
        assert!(matches!(
            endpoint_b.receive().unwrap(),
            Some(ChannelEvent::Message(_))
        ));
        assert_eq!(endpoint_b.receive().unwrap(), None);

        let mut duplicate = FakeSignal::new(FakeSignalMode::Deliver);
        duplicate.notify();
        duplicate.notify();
        assert!(duplicate.try_wait());
        assert_eq!(endpoint_b.receive().unwrap(), None);
        assert!(duplicate.try_wait());
        assert_eq!(endpoint_b.receive().unwrap(), None);
    }

    #[test]
    fn backpressure_recovers_with_one_writable_edge() {
        let (mut endpoint_a, mut endpoint_b) = channel();
        for payload in [b"one".as_slice(), b"two", b"three"] {
            endpoint_a.send(payload).unwrap();
        }
        assert_eq!(
            endpoint_a.send(b"blocked").unwrap_err().code(),
            ErrorCode::Backpressured
        );
        endpoint_b.receive().unwrap();
        assert!(!endpoint_a.refresh_flow().unwrap().became_writable);
        endpoint_b.receive().unwrap();
        assert!(endpoint_a.refresh_flow().unwrap().became_writable);
        assert!(!endpoint_a.refresh_flow().unwrap().became_writable);
        endpoint_a.send(b"again").unwrap();
    }

    #[test]
    fn close_is_fifo_and_terminal() {
        let (mut endpoint_a, mut endpoint_b) = channel();
        endpoint_a.send(b"last").unwrap();
        endpoint_a.close().unwrap();
        assert!(matches!(
            endpoint_b.receive().unwrap(),
            Some(ChannelEvent::Message(_))
        ));
        assert_eq!(endpoint_b.receive().unwrap(), Some(ChannelEvent::Closed));
        assert_eq!(
            endpoint_b.send(b"late").unwrap_err().code(),
            ErrorCode::Closed
        );
    }

    #[test]
    fn stress_preserves_bidirectional_fifo_without_signal_progress() {
        const MESSAGES: u32 = 20_000;
        let (mut endpoint_a, mut endpoint_b) = in_process_channel(256, 64, 64, 192).unwrap();
        let mut dropped = FakeSignal::new(FakeSignalMode::Drop);
        for sequence in 0..MESSAGES {
            let mut payload = sequence.to_le_bytes().to_vec();
            payload.resize((sequence as usize % 57) + 4, sequence.to_le_bytes()[0]);
            let sent = if sequence % 2 == 0 {
                endpoint_a.send(&payload).unwrap()
            } else {
                endpoint_b.send(&payload).unwrap()
            };
            if sent.notify {
                dropped.notify();
            }
            let event = if sequence % 2 == 0 {
                endpoint_b.receive().unwrap()
            } else {
                endpoint_a.receive().unwrap()
            };
            assert_eq!(event, Some(ChannelEvent::Message(payload)));
        }
        assert!(!dropped.try_wait());
        assert_eq!(endpoint_a.receive().unwrap(), None);
        assert_eq!(endpoint_b.receive().unwrap(), None);
    }
}
