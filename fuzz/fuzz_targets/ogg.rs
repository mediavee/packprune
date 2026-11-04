#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(pcm) = packprune_codec::audio::decode(data) {
        if let Ok(out) = packprune_codec::audio::optimise(data) {
            assert_eq!(pcm, packprune_codec::audio::decode(&out).unwrap());
        }
    }
});
