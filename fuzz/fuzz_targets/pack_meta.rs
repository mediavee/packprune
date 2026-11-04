#![no_main]

use libfuzzer_sys::fuzz_target;
use packprune_pack::PackMeta;

// `pack.mcmeta` is user input: parsing and cutting views never panic, and the views keep their invariants.
fuzz_target!(|data: &[u8]| {
    let Ok(meta) = PackMeta::parse(data) else {
        return;
    };
    for jar in [None, Some((88, 0))] {
        let views = meta.views(jar);
        // An inverted range declares nothing, so it may give no view at all.
        assert!(!views.is_empty() || meta.bounds().iter().all(|&point| !meta.declares(point)));
        assert!(views.iter().filter(|view| view.verified).count() <= if jar.is_some() { 1 } else { views.len() });
        for view in &views {
            assert!(meta.declares(view.format));
            let active = meta.active(view.format);
            assert_eq!(view.overlays.iter().map(String::as_str).collect::<Vec<_>>(), active);
        }
    }
});
