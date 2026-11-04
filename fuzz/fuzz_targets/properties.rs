#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = packprune_codec::properties::parse(data);
    if let Ok(out) = packprune_codec::properties::minify(data) {
        let pairs = |bytes: &[u8]| {
            packprune_codec::properties::parse_checked(bytes)
                .unwrap()
                .into_iter()
                .map(|p| (p.key, p.value))
                .collect::<Vec<_>>()
        };
        assert_eq!(pairs(data), pairs(&out));
    }
});
