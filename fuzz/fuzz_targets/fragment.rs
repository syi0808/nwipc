#![no_main]

use libfuzzer_sys::fuzz_target;
use nwipc_fragment::Reassembler;
use nwipc_record::{ParsedRecordHeader, RecordFlags, RecordKind};
use nwipc_types::{MessageId, Sequence};

fuzz_target!(|input: &[u8]| {
    let mut reassembler = Reassembler::new(16, 64).expect("fixed limits are valid");
    for (sequence, chunk) in input.chunks(18).enumerate() {
        let Some((&control, payload)) = chunk.split_first() else {
            continue;
        };
        let mut flags = RecordFlags::NONE;
        if control & 1 != 0 {
            flags = flags.union(RecordFlags::FRAGMENTED);
        }
        if control & 2 != 0 {
            flags = flags.union(RecordFlags::END_OF_MESSAGE);
        }
        let message_id = MessageId::new(u32::from(control >> 2) + 1).expect("non-zero id");
        let header = ParsedRecordHeader::new(
            payload.len() as u32,
            message_id,
            Sequence::new(sequence as u32),
            RecordKind::Data,
            flags,
        )
        .expect("bounded payload header");
        let _ = reassembler.push(header, payload);
    }
});
