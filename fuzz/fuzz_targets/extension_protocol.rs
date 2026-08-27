#![no_main]

use e::core::api::parse_incoming;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let line = String::from_utf8_lossy(data);
    let _ = parse_incoming(&line);
});
