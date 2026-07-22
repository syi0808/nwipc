//! Bidirectional in-process channel composed from two opposite-owner SPSC rings.
//!
//! Notifications are deliberately absent from the correctness path. [`ChannelSend`] tells an
//! adapter when an empty-to-non-empty hint is useful, while [`ChannelEndpoint::receive`] always
//! inspects acquired cursors and drains records without requiring that hint.

use nwipc_atomic::{ConsumerMemory, ProducerMemory, in_process_ring};
use nwipc_error::{ErrorCategory, ErrorCode, ErrorReport, Recoverability};
use nwipc_flow_control::{FlowControl, FlowUpdate};
use nwipc_fragment::{Fragmenter, Reassembler, Reassembly};
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
    in_process_channel_with_config(ChannelConfig {
        capacity,
        maximum_inline_message,
        maximum_message: maximum_inline_message,
        low_watermark,
        high_watermark,
    })
}

/// Creates two endpoints with explicit inline and logical-message limits.
///
/// # Errors
///
/// Returns `InvalidRange` when limits, capacity, or watermarks are inconsistent.
pub fn in_process_channel_with_config(
    config: ChannelConfig,
) -> Result<(ChannelEndpoint, ChannelEndpoint), ErrorReport> {
    let (a_to_b_producer, a_to_b_consumer) = in_process_ring(config.capacity)?;
    let (b_to_a_producer, b_to_a_consumer) = in_process_ring(config.capacity)?;
    let endpoint_a = ChannelEndpoint::from_memories(a_to_b_producer, b_to_a_consumer, config)?;
    let endpoint_b = ChannelEndpoint::from_memories(b_to_a_producer, a_to_b_consumer, config)?;
    Ok((endpoint_a, endpoint_b))
}

/// Bounded in-process channel configuration.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ChannelConfig {
    /// Directional ring capacity in bytes.
    pub capacity: u32,
    /// Maximum payload bytes stored in one record.
    pub maximum_inline_message: u32,
    /// Maximum payload bytes in one reassembled logical message.
    pub maximum_message: u32,
    /// Occupancy at or below which a writable edge is emitted.
    pub low_watermark: u32,
    /// Occupancy at or above which new sends are backpressured.
    pub high_watermark: u32,
}

impl ChannelConfig {
    fn fragmenter(self) -> Result<Fragmenter, ErrorReport> {
        Fragmenter::new(self.maximum_inline_message, self.maximum_message)
    }

    fn validate_capacity(self, fragmenter: Fragmenter) -> Result<(), ErrorReport> {
        let wire_bytes = fragmenter.maximum_wire_bytes()?;
        let worst_case_bytes = if self.maximum_message > self.maximum_inline_message {
            fragmenter.maximum_batch_bytes()?
        } else {
            wire_bytes
        };
        if worst_case_bytes > self.capacity {
            return Err(invalid_channel_configuration());
        }
        Ok(())
    }
}

/// One side of a bidirectional channel.
pub struct ChannelEndpoint {
    writer: RingWriter,
    reader: RingReader,
    flow: FlowControl,
    fragmenter: Fragmenter,
    reassembler: Reassembler,
    fragmentation_enabled: bool,
    local_closed: bool,
    remote_closed: bool,
}

impl ChannelEndpoint {
    /// Builds one endpoint from opposite-direction mapped or in-process ring halves.
    ///
    /// # Errors
    ///
    /// Rejects mismatched ring capacity or an invalid channel configuration.
    pub fn from_memories(
        writer_memory: ProducerMemory,
        reader_memory: ConsumerMemory,
        config: ChannelConfig,
    ) -> Result<Self, ErrorReport> {
        let fragmenter = config.fragmenter()?;
        config.validate_capacity(fragmenter)?;
        if writer_memory.capacity() != config.capacity
            || reader_memory.capacity() != config.capacity
        {
            return Err(invalid_channel_configuration());
        }
        Ok(Self {
            writer: RingWriter::new(writer_memory, config.maximum_inline_message),
            reader: RingReader::new(reader_memory, config.maximum_inline_message),
            flow: FlowControl::new(config.capacity, config.low_watermark, config.high_watermark)?,
            fragmenter,
            reassembler: Reassembler::new(config.maximum_inline_message, config.maximum_message)?,
            fragmentation_enabled: config.maximum_message > config.maximum_inline_message,
            local_closed: false,
            remote_closed: false,
        })
    }

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
        let fragments = self.fragmenter.fragments(payload)?;
        let outcome = self.writer.send_fragments(RecordKind::Data, &fragments)?;
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
        loop {
            let Some(record) = self.reader.receive()? else {
                return Ok(None);
            };
            let fragmented = record.flags().bits() & RecordFlags::FRAGMENTED.bits() != 0;
            let event = match record.kind() {
                RecordKind::Data => {
                    if fragmented && !self.fragmentation_enabled {
                        return Err(channel_protocol_error("fragmentation not enabled"));
                    }
                    match self.reassembler.push(record.header, &record.payload)? {
                        Reassembly::Pending => continue,
                        Reassembly::Complete(payload) => ChannelEvent::Message(payload),
                    }
                }
                RecordKind::Close => {
                    if fragmented {
                        return Err(channel_protocol_error("fragmented close"));
                    }
                    self.reassembler.discard();
                    self.remote_closed = true;
                    ChannelEvent::Closed
                }
                RecordKind::Reset => {
                    if fragmented {
                        return Err(channel_protocol_error("fragmented reset"));
                    }
                    self.reassembler.discard();
                    self.remote_closed = true;
                    ChannelEvent::Reset
                }
                kind => {
                    if fragmented {
                        return Err(channel_protocol_error("fragmented control record"));
                    }
                    ChannelEvent::Control(ControlRecord { kind, record })
                }
            };
            return Ok(Some(event));
        }
    }

    /// Re-observes byte occupancy and reports the backpressured-to-writable edge once.
    ///
    /// # Errors
    ///
    /// Returns `InvalidCursor` if shared cursor state is corrupt.
    pub fn refresh_flow(&mut self) -> Result<FlowUpdate, ErrorReport> {
        self.flow.update(self.writer.buffered_amount()?)
    }

    /// Returns committed outbound bytes not yet consumed by the remote endpoint.
    ///
    /// # Errors
    ///
    /// Returns `InvalidCursor` if shared cursor state is corrupt.
    pub fn buffered_amount(&self) -> Result<u32, ErrorReport> {
        self.writer.buffered_amount()
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

fn channel_protocol_error(operation: &'static str) -> ErrorReport {
    ErrorReport::new(
        ErrorCategory::Protocol,
        ErrorCode::ProtocolViolation,
        Recoverability::ReplaceEndpoint,
        operation,
    )
}

fn invalid_channel_configuration() -> ErrorReport {
    ErrorReport::new(
        ErrorCategory::Configuration,
        ErrorCode::InvalidRange,
        Recoverability::Terminal,
        "channel fragmentation capacity",
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

    #[test]
    fn fragments_and_reassembles_one_logical_message() {
        let config = ChannelConfig {
            capacity: 512,
            maximum_inline_message: 32,
            maximum_message: 100,
            low_watermark: 128,
            high_watermark: 384,
        };
        let (mut sender, mut receiver) = in_process_channel_with_config(config).unwrap();
        let payload = (0_u8..100).collect::<Vec<_>>();
        let sent = sender.send(&payload).unwrap();
        assert!(sent.notify);
        assert_eq!(
            receiver.receive().unwrap(),
            Some(ChannelEvent::Message(payload))
        );
        assert_eq!(receiver.receive().unwrap(), None);
    }

    #[test]
    fn atomic_fragment_backpressure_exposes_no_partial_message() {
        let config = ChannelConfig {
            capacity: 256,
            maximum_inline_message: 32,
            maximum_message: 80,
            low_watermark: 64,
            high_watermark: 240,
        };
        let (mut sender, mut receiver) = in_process_channel_with_config(config).unwrap();
        let first = vec![1; 80];
        sender.send(&first).unwrap();
        assert_eq!(
            sender.send(&[2; 80]).unwrap_err().code(),
            ErrorCode::Backpressured
        );
        assert_eq!(
            receiver.receive().unwrap(),
            Some(ChannelEvent::Message(first))
        );
        assert_eq!(receiver.receive().unwrap(), None);
    }

    #[test]
    fn fragmented_batch_wraps_with_padding_and_preserves_fifo() {
        let config = ChannelConfig {
            capacity: 512,
            maximum_inline_message: 32,
            maximum_message: 100,
            low_watermark: 128,
            high_watermark: 384,
        };
        let (mut sender, mut receiver) = in_process_channel_with_config(config).unwrap();
        for value in 0_u8..8 {
            let payload = [value; 32];
            sender.send(&payload).unwrap();
            assert_eq!(
                receiver.receive().unwrap(),
                Some(ChannelEvent::Message(payload.to_vec()))
            );
        }
        let payload = (0_u8..100).rev().collect::<Vec<_>>();
        sender.send(&payload).unwrap();
        assert_eq!(
            receiver.receive().unwrap(),
            Some(ChannelEvent::Message(payload))
        );
        assert_eq!(receiver.receive().unwrap(), None);
    }

    #[test]
    fn close_discards_an_incomplete_fragment() {
        let config = ChannelConfig {
            capacity: 256,
            maximum_inline_message: 32,
            maximum_message: 80,
            low_watermark: 64,
            high_watermark: 240,
        };
        let (mut sender, mut receiver) = in_process_channel_with_config(config).unwrap();
        let fragmenter = Fragmenter::new(32, 80).unwrap();
        let payload = vec![7; 80];
        let fragments = fragmenter.fragments(&payload).unwrap();
        sender
            .writer
            .send_fragments(RecordKind::Data, &fragments[..1])
            .unwrap();
        sender.close().unwrap();
        assert_eq!(receiver.receive().unwrap(), Some(ChannelEvent::Closed));
        assert!(!receiver.reassembler.is_pending());
    }

    #[test]
    fn rejects_impossible_fragment_configuration() {
        let result = in_process_channel_with_config(ChannelConfig {
            capacity: 128,
            maximum_inline_message: 32,
            maximum_message: 80,
            low_watermark: 32,
            high_watermark: 96,
        });
        assert_eq!(result.err().unwrap().code(), ErrorCode::InvalidRange);
    }
}
