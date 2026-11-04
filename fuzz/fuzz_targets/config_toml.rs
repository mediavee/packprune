#![no_main]

use libfuzzer_sys::fuzz_target;
use packprune_config::{packsquash, parse};

fuzz_target!(|data: &[u8]| {
    if let Ok(text) = std::str::from_utf8(data) {
        let _ = parse(text, "packprune.toml".into());
        let _ = packsquash::parse(text, "settings.toml".into(), &[]);
        let _ = packsquash::parse(text, "settings.toml".into(), &["lean".into()]);
    }
});
