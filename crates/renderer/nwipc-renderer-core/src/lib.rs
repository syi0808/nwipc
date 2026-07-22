//! Platform-independent renderer port lifecycle, callback registry, and event queue.

use std::collections::{HashMap, VecDeque};

use nwipc_error::{ErrorCategory, ErrorCode, ErrorReport, Recoverability};
use nwipc_renderer_api::{
    CallbackId, DocumentPort, PortState, RendererContext, RendererRuntime, RendererTransport,
    SendDisposition, TransportEvent,
};
use nwipc_types::{DocumentGeneration, PortId};

/// Event copied out of a native transport for JavaScript-thread delivery.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct QueuedEvent {
    /// Generation and port that owned the event when it was queued.
    pub port: DocumentPort,
    /// Callbacks present at queue time, in registration order.
    pub callbacks: Vec<CallbackId>,
    /// Event payload or terminal state.
    pub event: TransportEvent,
}

struct Port {
    generation: DocumentGeneration,
    state: PortState,
    transport: Box<dyn RendererTransport>,
    callbacks: Vec<CallbackId>,
    backpressured: bool,
}

/// Renderer-side owner of generation-scoped native ports.
pub struct RendererCore {
    active_document: Option<DocumentGeneration>,
    ports: HashMap<PortId, Port>,
    events: VecDeque<QueuedEvent>,
    next_port_id: u32,
    next_callback_id: u64,
}

impl Default for RendererCore {
    fn default() -> Self {
        Self::new()
    }
}

impl RendererCore {
    /// Creates an empty runtime without an installed document.
    pub fn new() -> Self {
        Self {
            active_document: None,
            ports: HashMap::new(),
            events: VecDeque::new(),
            next_port_id: 1,
            next_callback_id: 1,
        }
    }

    /// Returns the active document identity.
    pub const fn active_document(&self) -> Option<DocumentGeneration> {
        self.active_document
    }

    /// Opens a transport in the active document.
    ///
    /// # Errors
    ///
    /// Returns `StaleGeneration` when the caller belongs to an old document.
    pub fn connect(
        &mut self,
        generation: DocumentGeneration,
        transport: Box<dyn RendererTransport>,
    ) -> Result<DocumentPort, ErrorReport> {
        self.require_active(generation)?;
        let port_id = self.allocate_port_id()?;
        self.ports.insert(
            port_id,
            Port {
                generation,
                state: PortState::Open,
                transport,
                callbacks: Vec::new(),
                backpressured: false,
            },
        );
        Ok(DocumentPort {
            generation,
            port_id,
        })
    }

    /// Registers a callback and preserves registration order.
    ///
    /// # Errors
    ///
    /// Returns a lifecycle error for stale or terminal ports.
    pub fn register_callback(&mut self, port: DocumentPort) -> Result<CallbackId, ErrorReport> {
        self.require_port(port, false)?;
        let callback = CallbackId::new(self.next_callback_id);
        self.next_callback_id = self
            .next_callback_id
            .checked_add(1)
            .ok_or_else(|| lifecycle_error(ErrorCode::Internal, "renderer callback identity"))?;
        self.ports
            .get_mut(&port.port_id)
            .ok_or_else(stale_document)?
            .callbacks
            .push(callback);
        Ok(callback)
    }

    /// Removes a callback. Already removed and stale callbacks are harmless.
    pub fn remove_callback(&mut self, callback: CallbackId) {
        for port in self.ports.values_mut() {
            port.callbacks.retain(|candidate| *candidate != callback);
        }
        for event in &mut self.events {
            event.callbacks.retain(|candidate| *candidate != callback);
        }
    }

    /// Sends an owned message through the native transport.
    ///
    /// # Errors
    ///
    /// Rejects stale, closing, and terminal ports before touching native state.
    pub fn send(
        &mut self,
        port: DocumentPort,
        payload: &[u8],
    ) -> Result<SendDisposition, ErrorReport> {
        self.require_port(port, true)?;
        let native_port = self
            .ports
            .get_mut(&port.port_id)
            .ok_or_else(stale_document)?;
        let disposition = native_port.transport.send(payload)?;
        native_port.backpressured = disposition == SendDisposition::Backpressured;
        Ok(disposition)
    }

    /// Returns committed outbound bytes for an open port.
    ///
    /// # Errors
    ///
    /// Rejects stale and terminal ports.
    pub fn buffered_amount(&self, port: DocumentPort) -> Result<u32, ErrorReport> {
        self.require_port(port, false)?;
        self.ports
            .get(&port.port_id)
            .ok_or_else(stale_document)?
            .transport
            .buffered_amount()
    }

    /// Starts an idempotent graceful close and queues exactly one close event.
    ///
    /// # Errors
    ///
    /// Rejects a port from a stale document.
    pub fn close(&mut self, port: DocumentPort) -> Result<(), ErrorReport> {
        self.require_port(port, false)?;
        let native_port = self
            .ports
            .get_mut(&port.port_id)
            .ok_or_else(stale_document)?;
        if native_port.state == PortState::Open {
            native_port.state = PortState::Closing;
            if let Err(error) = native_port.transport.close() {
                self.finish(port.port_id, TransportEvent::Error(error));
            }
        }
        Ok(())
    }

    /// Returns the current state, rejecting stale document handles.
    ///
    /// # Errors
    ///
    /// Returns `StaleGeneration` for invalidated handles.
    pub fn state(&self, port: DocumentPort) -> Result<PortState, ErrorReport> {
        self.require_port(port, false)?;
        Ok(self
            .ports
            .get(&port.port_id)
            .ok_or_else(stale_document)?
            .state)
    }

    /// Removes and returns the next event ready for JavaScript dispatch.
    pub fn pop_event(&mut self) -> Option<QueuedEvent> {
        while let Some(event) = self.events.pop_front() {
            if self.active_document == Some(event.port.generation) {
                return Some(event);
            }
        }
        None
    }

    fn poll_port(&mut self, port_id: PortId) -> Result<(), ErrorReport> {
        loop {
            let event = {
                let port = self.ports.get_mut(&port_id).expect("port id came from map");
                if port.state.is_terminal() {
                    return Ok(());
                }
                match port.transport.poll() {
                    Ok(event) => event,
                    Err(error) => {
                        self.finish(port_id, TransportEvent::Error(error));
                        return Ok(());
                    }
                }
            };
            let Some(event) = event else {
                return Ok(());
            };
            match event {
                TransportEvent::Closed | TransportEvent::Error(_) => self.finish(port_id, event),
                TransportEvent::Writable => {
                    let port = self.ports.get_mut(&port_id).expect("port id came from map");
                    if port.backpressured {
                        port.backpressured = false;
                        self.queue(port_id, TransportEvent::Writable);
                    }
                }
                TransportEvent::Message(_) => self.queue(port_id, event),
            }
        }
    }

    fn queue(&mut self, port_id: PortId, event: TransportEvent) {
        let port = self.ports.get(&port_id).expect("port id came from map");
        self.events.push_back(QueuedEvent {
            port: DocumentPort {
                generation: port.generation,
                port_id,
            },
            callbacks: port.callbacks.clone(),
            event,
        });
    }

    fn finish(&mut self, port_id: PortId, event: TransportEvent) {
        let failed = matches!(event, TransportEvent::Error(_));
        self.queue(port_id, event);
        let port = self.ports.get_mut(&port_id).expect("port id came from map");
        port.state = if failed {
            PortState::Failed
        } else {
            PortState::Closed
        };
        port.callbacks.clear();
    }

    fn require_active(&self, generation: DocumentGeneration) -> Result<(), ErrorReport> {
        if self.active_document == Some(generation) {
            Ok(())
        } else {
            Err(stale_document())
        }
    }

    fn require_port(&self, handle: DocumentPort, require_open: bool) -> Result<(), ErrorReport> {
        self.require_active(handle.generation)?;
        let port = self.ports.get(&handle.port_id).ok_or_else(stale_document)?;
        if port.generation != handle.generation {
            return Err(stale_document());
        }
        if require_open && port.state != PortState::Open {
            return Err(lifecycle_error(ErrorCode::Closed, "renderer port state"));
        }
        Ok(())
    }

    fn allocate_port_id(&mut self) -> Result<PortId, ErrorReport> {
        let value = self.next_port_id;
        self.next_port_id = self
            .next_port_id
            .checked_add(1)
            .ok_or_else(|| lifecycle_error(ErrorCode::Internal, "renderer port identity"))?;
        PortId::new(value)
            .ok_or_else(|| lifecycle_error(ErrorCode::Internal, "renderer port identity"))
    }
}

impl RendererRuntime for RendererCore {
    fn install_binding(&mut self, context: RendererContext) -> Result<(), ErrorReport> {
        if let Some(active) = self.active_document {
            if context.generation <= active {
                return Err(stale_document());
            }
            self.invalidate_document(active);
        }
        self.active_document = Some(context.generation);
        Ok(())
    }

    fn invalidate_document(&mut self, generation: DocumentGeneration) {
        self.ports.retain(|_, port| port.generation != generation);
        self.events
            .retain(|event| event.port.generation != generation);
        if self.active_document == Some(generation) {
            self.active_document = None;
        }
    }

    fn dispatch_readable(&mut self) -> Result<(), ErrorReport> {
        let port_ids: Vec<_> = self.ports.keys().copied().collect();
        for port_id in port_ids {
            self.poll_port(port_id)?;
        }
        Ok(())
    }
}

fn stale_document() -> ErrorReport {
    lifecycle_error(ErrorCode::StaleGeneration, "renderer document generation")
}

fn lifecycle_error(code: ErrorCode, operation: &'static str) -> ErrorReport {
    ErrorReport::new(
        ErrorCategory::Lifecycle,
        code,
        Recoverability::ReplaceEndpoint,
        operation,
    )
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;

    use super::*;

    #[derive(Default)]
    struct MockTransport {
        events: VecDeque<TransportEvent>,
        buffered: u32,
        closes: u32,
    }

    impl RendererTransport for MockTransport {
        fn send(&mut self, payload: &[u8]) -> Result<SendDisposition, ErrorReport> {
            self.buffered += u32::try_from(payload.len()).unwrap();
            Ok(if self.buffered > 4 {
                SendDisposition::Backpressured
            } else {
                SendDisposition::Sent
            })
        }

        fn buffered_amount(&self) -> Result<u32, ErrorReport> {
            Ok(self.buffered)
        }

        fn poll(&mut self) -> Result<Option<TransportEvent>, ErrorReport> {
            Ok(self.events.pop_front())
        }

        fn close(&mut self) -> Result<(), ErrorReport> {
            self.closes += 1;
            self.events.push_back(TransportEvent::Closed);
            Ok(())
        }
    }

    fn generation(value: u64) -> DocumentGeneration {
        DocumentGeneration::new(value).unwrap()
    }

    fn runtime() -> RendererCore {
        let mut runtime = RendererCore::new();
        runtime
            .install_binding(RendererContext {
                generation: generation(1),
            })
            .unwrap();
        runtime
    }

    #[test]
    fn queues_binary_events_in_transport_order() {
        let mut runtime = runtime();
        let mut transport = MockTransport::default();
        transport
            .events
            .push_back(TransportEvent::Message(vec![1, 2]));
        transport.events.push_back(TransportEvent::Message(vec![3]));
        let port = runtime.connect(generation(1), Box::new(transport)).unwrap();
        let callback = runtime.register_callback(port).unwrap();
        runtime.dispatch_readable().unwrap();
        let first = runtime.pop_event().unwrap();
        let second = runtime.pop_event().unwrap();
        assert_eq!(first.callbacks, vec![callback]);
        assert_eq!(first.event, TransportEvent::Message(vec![1, 2]));
        assert_eq!(second.event, TransportEvent::Message(vec![3]));
    }

    #[test]
    fn invalidation_drops_ports_callbacks_and_queued_events() {
        let mut runtime = runtime();
        let mut transport = MockTransport::default();
        transport.events.push_back(TransportEvent::Message(vec![1]));
        let stale = runtime.connect(generation(1), Box::new(transport)).unwrap();
        runtime.register_callback(stale).unwrap();
        runtime.dispatch_readable().unwrap();
        runtime.invalidate_document(generation(1));
        runtime
            .install_binding(RendererContext {
                generation: generation(2),
            })
            .unwrap();
        assert_eq!(
            runtime.send(stale, b"stale").unwrap_err().code(),
            ErrorCode::StaleGeneration
        );
        assert!(runtime.pop_event().is_none());
    }

    #[test]
    fn close_is_idempotent_and_terminal_event_is_once() {
        let mut runtime = runtime();
        let port = runtime
            .connect(generation(1), Box::<MockTransport>::default())
            .unwrap();
        runtime.register_callback(port).unwrap();
        runtime.close(port).unwrap();
        runtime.close(port).unwrap();
        runtime.dispatch_readable().unwrap();
        assert_eq!(runtime.pop_event().unwrap().event, TransportEvent::Closed);
        assert!(runtime.pop_event().is_none());
        assert_eq!(runtime.state(port).unwrap(), PortState::Closed);
    }

    #[test]
    fn duplicate_writable_events_collapse_to_one_edge() {
        let mut runtime = runtime();
        let mut transport = MockTransport::default();
        transport.events.push_back(TransportEvent::Writable);
        transport.events.push_back(TransportEvent::Writable);
        let port = runtime.connect(generation(1), Box::new(transport)).unwrap();
        assert_eq!(
            runtime.send(port, b"12345").unwrap(),
            SendDisposition::Backpressured
        );
        runtime.dispatch_readable().unwrap();
        assert_eq!(runtime.pop_event().unwrap().event, TransportEvent::Writable);
        assert!(runtime.pop_event().is_none());
    }

    #[test]
    fn implements_the_shared_renderer_contract_fixture() {
        let scenarios = include_str!("../../../../tests/renderer-contract/scenarios.txt")
            .lines()
            .collect::<Vec<_>>();
        assert_eq!(
            scenarios,
            [
                "binary-copy",
                "fifo-reentrancy",
                "backpressure-writable-edge",
                "terminal-close",
                "terminal-error",
                "stale-document",
            ]
        );
    }
}
