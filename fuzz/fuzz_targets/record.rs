#![no_main]

use libfuzzer_sys::fuzz_target;
use nwipc_layout::MAX_INLINE_MESSAGE_SIZE;
use nwipc_record::ParsedRecordHeader;

fuzz_target!(|input: &[u8]| {
    let maximum = input
        .get(..4)
        .and_then(|bytes| bytes.try_into().ok())
        .map(u32::from_le_bytes)
        .unwrap_or(0)
        .min(MAX_INLINE_MESSAGE_SIZE);
    let committed = input.get(4..).unwrap_or_default();
    let _ = ParsedRecordHeader::decode_committed(committed, maximum);
});
