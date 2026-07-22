#![no_main]

use libfuzzer_sys::fuzz_target;
use nwipc_layout::{REGION_HEADER_SIZE, RegionLayout};

fuzz_target!(|input: &[u8]| {
    if let Ok(layout) = RegionLayout::decode(input) {
        let mut canonical = vec![0; REGION_HEADER_SIZE];
        layout
            .encode(&mut canonical)
            .expect("a decoded layout must remain encodable");
        assert_eq!(RegionLayout::decode(&canonical), Ok(layout));
    }
});
