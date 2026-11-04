//! `report.md`: the analysis as text.

use std::collections::BTreeSet;
use std::fmt::Write;

use super::model::{Report, SourceInfo, UnusedSelection, UsageRef};

/// Shown with every unused-item selection: the files that exist only in live data are not seen.
const LIVE_DATA: &str = "No world, player or database data is read: an item that exists only there looks unused, \
    and once dropped it shows the missing model until the next build, its data untouched. Keep it with a keep-list \
    entry or a source that names it.";

pub fn render(report: &Report) -> String {
    let mut md = String::new();
    let s = &report.summary;
    md.push_str("# packprune report\n\n");
    md.push_str("| Input | Name | SHA-256 |\n|---|---|---|\n");
    for (label, input) in [
        ("pack", &report.inputs.pack),
        ("client jar", &report.inputs.client_jar),
        ("asset index", &report.inputs.asset_index),
    ] {
        let _ = writeln!(md, "| {label} | `{}` | `{}` |", input.name, input.sha256);
    }
    for source in &report.sources {
        for input in &source.inputs {
            let _ = writeln!(
                md,
                "| source `{}` | `{}` | `blake3:{}` |",
                source.id, input.name, input.blake3
            );
        }
    }
    md.push('\n');
    let _ = write!(
        md,
        "Client {}, rules of {}. packprune {}, report schema {}. Views: ",
        report.inputs.client_version,
        report.inputs.client_rules,
        report.tool.version,
        report.schema
    );
    let views: Vec<String> = report
        .views
        .iter()
        .map(|view| format!("`{}` (format {})", view.id, view.format))
        .collect();
    md.push_str(&views.join(", "));
    md.push_str(".\n\n");
    if !report.warnings.is_empty() {
        md.push_str("## Warnings\n\n");
        for warning in &report.warnings {
            let _ = writeln!(md, "- {warning}");
        }
        md.push('\n');
    }
    md.push_str("## Summary\n\n| | Files | Bytes |\n|---|---:|---:|\n");
    let rows: [(&str, String, String); 11] = [
        ("All files", s.files.to_string(), s.bytes.to_string()),
        ("Reached", s.reached.to_string(), String::new()),
        ("Orphans", s.orphans.to_string(), s.orphan_bytes.to_string()),
        (
            "Atlas-loaded orphans",
            s.atlas_loaded_orphans.to_string(),
            s.atlas_loaded_orphan_bytes.to_string(),
        ),
        (
            "Unused items' files",
            s.unused.to_string(),
            s.unused_bytes.to_string(),
        ),
        (
            "Dead blockstates",
            s.dead_blockstates.to_string(),
            String::new(),
        ),
        ("Unknown files (kept)", s.unknown.to_string(), String::new()),
        ("Missing references", s.missing.to_string(), String::new()),
        (
            "Sprites not stitched by any atlas",
            s.not_stitched.to_string(),
            String::new(),
        ),
        (
            "Other formats, unverified: missing references, sprites not stitched",
            format!(
                "{}, {}",
                s.other_formats_missing, s.other_formats_not_stitched
            ),
            String::new(),
        ),
        ("Unreadable files", s.invalid.to_string(), String::new()),
    ];
    for (label, files, bytes) in rows {
        let bytes = if bytes.is_empty() {
            bytes
        } else {
            format!("{bytes} ")
        };
        let _ = writeln!(md, "| {label} | {files} | {bytes}|");
    }
    let _ = writeln!(md, "| Usages matching nothing | {} | |", s.unmatched_usages);
    md.push_str(
        "\n## Views\n\n| Format | Verified | Overlays | Missing references | Sprites not stitched |\n|---|---|---|---:|---:|\n",
    );
    for view in &report.views {
        let overlays = if view.overlays.is_empty() {
            "none".to_owned()
        } else {
            view.overlays.join(", ")
        };
        let missing = report
            .missing
            .iter()
            .chain(&report.other_formats.missing)
            .filter(|finding| finding.views.contains(&view.format))
            .count();
        let not_stitched = report
            .not_stitched
            .iter()
            .chain(&report.other_formats.not_stitched)
            .filter(|finding| finding.views.contains(&view.format))
            .count();
        let _ = writeln!(
            md,
            "| {} | {} | {overlays} | {missing} | {not_stitched} |",
            view.format,
            if view.verified { "yes" } else { "no" }
        );
    }
    md.push_str(
        "\n## By kind\n\n| Kind | Files | Bytes | Reached | Unreached | Unreached bytes |\n|---|---:|---:|---:|---:|---:|\n",
    );
    for kind in &report.by_kind {
        let _ = writeln!(
            md,
            "| {} | {} | {} | {} | {} | {} |",
            kind.kind, kind.files, kind.bytes, kind.reached, kind.unreached, kind.unreached_bytes
        );
    }
    let view_count = report.views.len();
    section(
        &mut md,
        "Orphans",
        "Files nothing reaches.",
        report
            .orphans
            .iter()
            .map(|file| format!("`{}` ({}, {} B)", file.path, file.kind, file.bytes))
            .collect(),
    );
    section(
        &mut md,
        "Atlas-loaded orphans",
        "Nothing in the pack names them, but an atlas source stitches them: they cost client memory as well as \
         download. apply keeps them, since the server can name a stitched sprite at any time (tooltip styles, text \
         sprites, data-pack asset ids).",
        report
            .atlas_loaded_orphans
            .iter()
            .map(|file| {
                let atlases: Vec<&str> = file.atlases.iter().map(String::as_str).collect();
                format!("`{}` ({} B, {})", file.path, file.bytes, atlases.join(", "))
            })
            .collect(),
    );
    for selection in &report.unused_items {
        unused_section(&mut md, selection);
    }
    section(
        &mut md,
        "Dead blockstates",
        "Blockstates for blocks the client does not have.",
        report
            .dead_blockstates
            .iter()
            .map(|file| format!("`{}`", file.path))
            .collect(),
    );
    section(
        &mut md,
        "Missing references",
        "Targets found neither in the pack nor in vanilla.",
        report
            .missing
            .iter()
            .map(|finding| {
                format!(
                    "`{}` ({}) from `{}`{}",
                    finding.target,
                    finding.reason,
                    finding.source,
                    views_of(&finding.views, view_count)
                )
            })
            .collect(),
    );
    section(
        &mut md,
        "Sprites not stitched by any atlas",
        "A file of the sprite's name exists, but no atlas source stitches it, so the client draws the missing \
         texture. Pack generators often add atlas entries when they build the pack: run on the built pack, which \
         is the input that counts.",
        report
            .not_stitched
            .iter()
            .map(|finding| {
                format!(
                    "`{}` ({}, file `{}`) from `{}`{}",
                    finding.sprite,
                    finding.atlases,
                    finding.file,
                    finding.source,
                    views_of(&finding.views, view_count)
                )
            })
            .collect(),
    );
    let mut other = Vec::new();
    for finding in &report.other_formats.missing {
        other.push(format!(
            "missing `{}` ({}) from `{}`{}",
            finding.target,
            finding.reason,
            finding.source,
            views_of(&finding.views, view_count)
        ));
    }
    for finding in &report.other_formats.not_stitched {
        other.push(format!(
            "not stitched `{}` ({}, file `{}`) from `{}`{}",
            finding.sprite,
            finding.atlases,
            finding.file,
            finding.source,
            views_of(&finding.views, view_count)
        ));
    }
    section(
        &mut md,
        "Other formats, unverified",
        "Found only in views for other formats than the client jar's, checked against that jar's vanilla and rules: \
         listed, not counted for the exit code.",
        other,
    );
    section(
        &mut md,
        "Unknown files",
        "Not a vanilla file kind: kept and listed.",
        report
            .unknown
            .iter()
            .map(|file| format!("`{}` ({} B)", file.path, file.bytes))
            .collect(),
    );
    section(
        &mut md,
        "Unreadable files",
        "Invalid JSON: their references could not be followed.",
        report
            .invalid
            .iter()
            .map(|file| format!("`{}`: {}", file.path, file.error))
            .collect(),
    );
    let _ = write!(md, "\n## Sources ({})\n\n", report.sources.len());
    if report.sources.is_empty() {
        md.push_str("None.\n");
    } else {
        md.push_str(
            "| Source | Inputs | Usages | Matched | Vanilla only | Unmatched |\n|---|---|---:|---:|---:|---:|\n",
        );
        for source in &report.sources {
            let _ = writeln!(
                md,
                "| {} | {} | {} | {} | {} | {} |",
                source.id,
                inputs_of(source),
                source.usages,
                source.matched,
                source.vanilla,
                source.unmatched.len()
            );
        }
    }
    section(
        &mut md,
        "Usages matching nothing",
        "Found neither in the pack nor in vanilla: a stale entry hides nothing, fix or remove it.",
        report
            .sources
            .iter()
            .flat_map(|source| &source.unmatched)
            .map(usage)
            .collect(),
    );
    section(
        &mut md,
        "Roots from sources",
        "Files kept because a source names them.",
        report
            .source_roots
            .iter()
            .map(|root| {
                let usages: Vec<String> = root.usages.iter().map(usage).collect();
                format!("`{}` from {}", root.path, usages.join("; "))
            })
            .collect(),
    );
    md
}

fn inputs_of(source: &SourceInfo) -> String {
    let names: Vec<String> = source
        .inputs
        .iter()
        .map(|input| format!("`{}`", input.name))
        .collect();
    names.join(", ")
}

fn unused_section(md: &mut String, selection: &UnusedSelection) {
    let own_bytes: u64 = selection.unused.iter().map(|item| item.bytes).sum();
    let own_files: usize = selection.unused.iter().map(|item| item.files.len()).sum();
    let shared_bytes: u64 = selection.shared.iter().map(|file| file.bytes).sum();
    let _ = write!(md, "\n## Unused items `{}`\n\n", selection.selector);
    let _ = write!(
        md,
        "Item declarations this selector picks that no source uses, searched in: {}. apply drops each item \
         definition and what only unused items reach. {LIVE_DATA}\n\n",
        selection.searched.join(", ")
    );
    md.push_str("| | Items | Files | Bytes |\n|---|---:|---:|---:|\n");
    let _ = writeln!(
        md,
        "| Unused, files only each reaches | {} | {own_files} | {own_bytes} |",
        selection.unused.len()
    );
    let _ = writeln!(
        md,
        "| Shared by several unused items | | {} | {shared_bytes} |",
        selection.shared.len()
    );
    let _ = write!(
        md,
        "| Kept, something uses them | {} | | |\n\n",
        selection.kept.len()
    );
    md.push_str("| Folder | Unused items | Files | Bytes |\n|---|---:|---:|---:|\n");
    for folder in &selection.folders {
        let _ = writeln!(
            md,
            "| `{}` | {} | {} | {} |",
            folder.folder, folder.items, folder.files, folder.bytes
        );
    }
    let quoted = |paths: &[String]| -> String {
        let quoted: Vec<String> = paths.iter().map(|path| format!("`{path}`")).collect();
        quoted.join(", ")
    };
    section(
        md,
        &format!("Unused: `{}`", selection.selector),
        "Declaration, item definitions, then the files and bytes only it reaches.",
        selection
            .unused
            .iter()
            .map(|item| {
                format!(
                    "`{}` {}: {} files, {} B",
                    item.declaration,
                    quoted(&item.definitions),
                    item.files.len(),
                    item.bytes
                )
            })
            .collect(),
    );
    section(
        md,
        &format!("Shared by unused items: `{}`", selection.selector),
        "",
        selection
            .shared
            .iter()
            .map(|file| {
                format!(
                    "`{}` ({} B) from {}",
                    file.path,
                    file.bytes,
                    file.declarations.join(", ")
                )
            })
            .collect(),
    );
    section(
        md,
        &format!("Kept: `{}`", selection.selector),
        "Picked, but a use or another declaration roots their item definition.",
        selection
            .kept
            .iter()
            .map(|item| {
                let used_by: Vec<String> = item.used_by.iter().map(usage).collect();
                format!("`{}` used by {}", item.declaration, used_by.join("; "))
            })
            .collect(),
    );
}

fn usage(usage: &UsageRef) -> String {
    let atlas = if usage.atlas.is_empty() {
        String::new()
    } else {
        format!(" in {}", usage.atlas)
    };
    let reason = if usage.reason.is_empty() {
        String::new()
    } else {
        format!(", {}", usage.reason)
    };
    format!(
        "{} {} (`{}: {}`{atlas}{reason})",
        usage.source, usage.location, usage.kind, usage.id
    )
}

/// Empty when a finding occurs in every view, else the formats of the views it occurs in.
fn views_of(views: &BTreeSet<String>, view_count: usize) -> String {
    if views.len() == view_count {
        String::new()
    } else {
        let formats: Vec<&str> = views.iter().map(String::as_str).collect();
        format!(" (views {})", formats.join(", "))
    }
}

fn section(md: &mut String, title: &str, intro: &str, lines: Vec<String>) {
    let _ = write!(md, "\n## {title} ({})\n\n", lines.len());
    if lines.is_empty() {
        md.push_str("None.\n");
        return;
    }
    if !intro.is_empty() {
        let _ = write!(md, "{intro}\n\n");
    }
    for line in lines {
        let _ = writeln!(md, "- {line}");
    }
}
