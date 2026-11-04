#![no_main]

use libfuzzer_sys::fuzz_target;
use packprune_pack::RunCx;

fuzz_target!(|data: &[u8]| {
    let cx = RunCx::default();
    for collect in [
        packprune_sources::configs::yaml_usages,
        packprune_sources::nexo::yaml_usages,
        packprune_sources::keep::yaml_usages,
    ] {
        if let Ok(usages) = collect(data, &cx) {
            assert_eq!(usages, collect(data, &cx).unwrap());
        }
    }
});
