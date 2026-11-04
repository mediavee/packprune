#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(image) = packprune_codec::png::decode(data) {
        let encoded = packprune_codec::png::encode(&image).unwrap();
        assert_eq!(image, packprune_codec::png::decode(&encoded).unwrap());
        if image.rgba.len() < 65536 {
            if let Ok(out) = packprune_codec::png::optimise(data, &Default::default()) {
                assert_eq!(image, packprune_codec::png::decode(&out).unwrap());
            }
        }
    }
});
