#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // Keep individual cases bounded so the target measures parser behavior rather
    // than allocating indefinitely on a single adversarial input.
    if data.len() > 16 * 1024 {
        return;
    }
    let candidate = String::from_utf8_lossy(data);
    let _ = jsm_core::Range::new(candidate.into_owned());
});
