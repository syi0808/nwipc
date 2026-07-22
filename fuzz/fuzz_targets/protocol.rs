#![no_main]

use libfuzzer_sys::fuzz_target;
use nwipc_capabilities::{SupportedCapabilities, TransportCapabilities};
use nwipc_protocol::{
    AcceptorConfig, AcceptorHandshake, EndpointRole, HandshakeIdentity, ProtocolVersion,
    VersionRange,
};
use nwipc_types::{Generation, SessionId};

fuzz_target!(|input: &[u8]| {
    let mut acceptor = AcceptorHandshake::new(AcceptorConfig {
        identity: HandshakeIdentity {
            session_id: SessionId::from_u128(1).expect("fixed non-zero session"),
            generation: Generation::new(1).expect("fixed non-zero generation"),
            role: EndpointRole::Coordinator,
        },
        remote_role: EndpointRole::Peer,
        versions: VersionRange::exact(ProtocolVersion::new(1, 0)),
        supported: SupportedCapabilities::new(TransportCapabilities::KNOWN),
        maximum_message: 1024 * 1024,
        proof: b"fuzz-proof".to_vec(),
    })
    .expect("fixed acceptor policy");
    let _ = acceptor.accept(input);
});
