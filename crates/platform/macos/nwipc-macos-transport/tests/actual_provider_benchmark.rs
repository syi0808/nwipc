#![cfg(target_os = "macos")]

use std::time::{Duration, Instant};

use nwipc_bootstrap_schema::{BootstrapEnvelope, BootstrapSecret, EndpointRole, ProtocolRange};
use nwipc_peer_core::{NativePort, PeerExpectation, PortEvent};
use nwipc_renderer_api::{RendererTransport, TransportEvent};
use nwipc_types::{Generation, SessionId};

use nwipc_macos_transport::{
    ChannelConfiguration, MacosEndpointTransport, MacosRendererTransport, PreparedMacosTransport,
    RendererExpectation, production_capabilities,
};

#[test]
#[ignore = "release-gate benchmark using actual IOSurface and Darwin providers"]
fn actual_provider_round_trip_baseline() {
    let session_id = SessionId::from_u128(1).unwrap();
    let generation = Generation::new(1).unwrap();
    let protocol = 1;
    let configuration = ChannelConfiguration::default();
    let (prepared, peer_envelope, renderer_envelope) =
        prepare_envelopes(session_id, generation, protocol, configuration);
    let cases = [(64_usize, 2_000_usize), (1024, 1_000), (16 * 1024, 200)];
    let expected_messages = cases.iter().map(|(_, iterations)| iterations).sum();
    let peer = spawn_peer(
        peer_envelope,
        PeerExpectation {
            session_id,
            generation,
            protocol,
        },
        configuration.maximum_message as usize,
        expected_messages,
    );
    let mut renderer = MacosRendererTransport::attach(
        renderer_envelope,
        RendererExpectation {
            session_id,
            generation,
            protocol,
        },
    )
    .unwrap();

    println!(
        "provider=IOSurface+Darwin/hybrid os={} architecture={} capacity={}",
        std::env::consts::OS,
        std::env::consts::ARCH,
        configuration.capacity
    );
    for (payload_length, iterations) in cases {
        run_case(&mut renderer, payload_length, iterations);
    }
    println!("transport_diagnostics={:?}", renderer.diagnostics());
    renderer.close().unwrap();
    peer.join().unwrap();
    drop(prepared);
}

fn prepare_envelopes(
    session_id: SessionId,
    generation: Generation,
    protocol: u16,
    configuration: ChannelConfiguration,
) -> (PreparedMacosTransport, BootstrapEnvelope, BootstrapEnvelope) {
    let prepared = PreparedMacosTransport::prepare(session_id, generation, configuration).unwrap();
    let memory = prepared.memory_descriptor().unwrap();
    let signal = prepared.signal_descriptor().unwrap();
    let range = ProtocolRange::new(protocol, protocol).unwrap();
    let envelope = |role, memory, signal| {
        BootstrapEnvelope::new(
            session_id,
            generation,
            range,
            role,
            memory,
            signal,
            BootstrapSecret::new(vec![0xa5; 32]).unwrap(),
        )
        .unwrap()
    };
    let peer = envelope(EndpointRole::Peer, memory.clone(), signal.clone());
    let renderer = envelope(EndpointRole::Renderer, memory, signal);
    (prepared, peer, renderer)
}

fn spawn_peer(
    envelope: BootstrapEnvelope,
    expectation: PeerExpectation,
    maximum_message: usize,
    expected_messages: usize,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let raw = MacosEndpointTransport::attach(&envelope, EndpointRole::Peer).unwrap();
        let mut port = NativePort::accept(
            envelope,
            expectation,
            raw,
            maximum_message,
            production_capabilities(),
        )
        .unwrap();
        for _ in 0..expected_messages {
            loop {
                match port.try_receive().unwrap() {
                    Some(PortEvent::Message(payload)) => {
                        port.try_send(&payload).unwrap();
                        break;
                    }
                    Some(PortEvent::Closed) => panic!("renderer closed before benchmark completed"),
                    None => std::thread::yield_now(),
                }
            }
        }
        loop {
            match port.try_receive().unwrap() {
                Some(PortEvent::Closed) => break,
                Some(PortEvent::Message(_)) => panic!("unexpected message after benchmark"),
                None => std::thread::yield_now(),
            }
        }
    })
}

fn run_case(renderer: &mut MacosRendererTransport, payload_length: usize, iterations: usize) {
    let payload = vec![0x5a; payload_length];
    let started = Instant::now();
    for _ in 0..iterations {
        renderer.send(&payload).unwrap();
        loop {
            match renderer.poll().unwrap() {
                Some(TransportEvent::Message(response)) => {
                    assert_eq!(response.len(), payload_length);
                    break;
                }
                Some(TransportEvent::Writable) | None => {
                    std::thread::sleep(Duration::from_micros(50));
                }
                event => panic!("unexpected renderer event: {event:?}"),
            }
        }
    }
    let elapsed = started.elapsed();
    let total_bytes = u32::try_from(payload_length * iterations).unwrap();
    println!(
        "payload={payload_length} iterations={iterations} mean_round_trip_ns={} throughput_mib_s={:.2}",
        elapsed.as_nanos() / iterations as u128,
        f64::from(total_bytes) / elapsed.as_secs_f64() / (1024.0 * 1024.0),
    );
}
