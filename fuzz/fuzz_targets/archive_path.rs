#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let candidate = String::from_utf8_lossy(data);
    let _ = jsm_testkit::validate_relative_archive_path(std::path::Path::new(candidate.as_ref()));
});
