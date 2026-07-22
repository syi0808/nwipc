#![no_main]

use libfuzzer_sys::fuzz_target;
use nwipc_layout::OwnerRole;
use nwipc_types::{Generation, Sequence, SessionId};
use nwipc_validation::{RegionExpectation, Validator};

fuzz_target!(|input: &[u8]| {
    let validator = Validator::new();
    let expectation = RegionExpectation {
        session_id: SessionId::from_u128(1).expect("fixed non-zero session"),
        generation: Generation::new(1).expect("fixed non-zero generation"),
        owner: OwnerRole::Peer,
    };
    let _ = validator.region(input, expectation);
    let producer = read_u32(input, 0);
    let consumer = read_u32(input, 4);
    let capacity = read_u32(input, 8);
    let maximum = read_u32(input, 12);
    let committed = read_u32(input, 16);
    let bytes = input.get(20..).unwrap_or_default();
    let _ = validator.cursors(producer, consumer, capacity);
    let _ = validator.record(bytes, committed, maximum, Some(Sequence::new(0)));
    let _ = validator.payload(bytes, producer, consumer, capacity);
});

fn read_u32(input: &[u8], offset: usize) -> u32 {
    input
        .get(offset..offset + 4)
        .and_then(|bytes| bytes.try_into().ok())
        .map(u32::from_le_bytes)
        .unwrap_or(0)
}
