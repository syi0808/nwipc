#![no_main]

use libfuzzer_sys::fuzz_target;
use nwipc_bootstrap_codec::{decode, encode};

fuzz_target!(|input: &[u8]| {
    if let Ok(envelope) = decode(input) {
        let canonical = encode(&envelope).expect("a decoded envelope must remain encodable");
        assert_eq!(decode(&canonical), Ok(envelope));
    }
});
