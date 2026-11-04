#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(out) = packprune_codec::glsl::minify(data) {
        assert_eq!(
            packprune_codec::glsl::tokenize(data).unwrap(),
            packprune_codec::glsl::tokenize(&out).unwrap()
        );
    }
});
