#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let candidate = String::from_utf8_lossy(data);
    let _ = jsm_core::PackageName::new(candidate.into_owned());
});
