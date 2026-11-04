//! The self-check of the written pack: read back and analysed again, it shows nothing the input did not.

use std::collections::BTreeSet;

use packprune_graph::Reached;
use packprune_oracle::Remap;
use packprune_pack::{LogicalPath, PhysPath, ResourceId, RunCx};

/// What `after` shows that `before` did not: files still removable, references or sprites newly broken, files newly
/// unreadable. A reference the input could not resolve stays the same finding when it changes kind (a sprite no atlas
/// stitched becomes a missing one once its file is dropped: the client draws the same missing texture). The input's
/// findings are named as `remap` renamed their files and sprites.
pub fn differences(cx: &RunCx, before: &Reached, after: &Reached, remap: &Remap) -> Vec<String> {
    let mut differences = Vec::new();
    for (path, file) in &after.files {
        if file.status.removable() {
            differences.push(format!("still {}: {path}", file.status.id()));
        }
    }
    let sprite = |id: &str| -> String {
        ResourceId::parse(&cx.interner, id)
            .filter(|from| remap.sprite(from) != *from)
            .map_or_else(
                || id.to_owned(),
                |from| remap.sprite(&from).display(&cx.interner).to_string(),
            )
    };
    let target = |target: &str| -> String {
        if target.starts_with("assets/") {
            LogicalPath::new(target).map_or_else(
                |_| target.to_owned(),
                |logical| remap.file(&logical).to_string(),
            )
        } else {
            sprite(target)
        }
    };
    let broken: BTreeSet<(PhysPath, String)> = before
        .missing
        .iter()
        .map(|missing| (remap.physical(&missing.source), target(&missing.target)))
        .chain(
            before
                .not_stitched
                .iter()
                .map(|found| (remap.physical(&found.source), sprite(&found.sprite))),
        )
        .collect();
    for missing in &after.missing {
        if !broken.contains(&(missing.source.clone(), missing.target.clone())) {
            differences.push(format!(
                "new missing reference: {} ({}) from {}",
                missing.target, missing.reason, missing.source
            ));
        }
    }
    for found in &after.not_stitched {
        if !broken.contains(&(found.source.clone(), found.sprite.clone())) {
            differences.push(format!(
                "new sprite not stitched: {} (file {}) from {}",
                found.sprite, found.file, found.source
            ));
        }
    }
    let unreadable: BTreeSet<PhysPath> = before
        .invalid
        .keys()
        .map(|path| remap.physical(path))
        .collect();
    for path in after.invalid.keys() {
        if !unreadable.contains(path) {
            differences.push(format!("unreadable file: {path}"));
        }
    }
    differences
}
