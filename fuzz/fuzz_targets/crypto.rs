#![no_main]

use libfuzzer_sys::fuzz_target;
use nwipc_crypto::{EndpointProtection, EndpointRole, MINIMUM_SECRET_LENGTH};
use nwipc_types::{Generation, SessionId};

fuzz_target!(|input: &[u8]| {
    let mut peer = EndpointProtection::derive(
        &[0xa5; MINIMUM_SECRET_LENGTH],
        SessionId::from_u128(1).expect("fixed non-zero session"),
        Generation::new(1).expect("fixed non-zero generation"),
        EndpointRole::Peer,
    )
    .expect("fixed protection parameters");
    let _ = peer.open(input);
});
