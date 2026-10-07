use std::io::{self, BufRead};

use serde::Deserialize;

#[derive(Deserialize)]
struct Case {
    range: String,
    version: String,
}

fn main() {
    for line in io::stdin().lock().lines() {
        let line = line.expect("read input line");
        let case: Case = serde_json::from_str(&line).expect("parse JSON case");
        let range = jsm_core::Range::new(case.range);
        let valid = range.is_ok();
        let matches = range.ok().is_some_and(|range| {
            jsm_core::Version::parse(&case.version)
                .ok()
                .is_some_and(|version| range.matches(&version))
        });
        println!(
            "{}",
            serde_json::json!({"valid": valid, "matches": matches})
        );
    }
}
