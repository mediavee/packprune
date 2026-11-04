use std::collections::BTreeSet;

use packprune_codec::{audio, properties};
use packprune_graph::Names;
use packprune_pack::json::{self, TokenKind};
use packprune_pack::{Entry, FileKind, Layer, LogicalPath, PhysPath};
use packprune_pass::{Edits, Loss, Pass, PassCx, PassError, Stage};

use super::{kind, no_options};

/// The first format whose clients refuse a resource path outside `[a-z0-9/._-]` (1.13, `pack_format` 4); 1.11 and
/// 1.12 only lowercase it.
const VALID_PATHS: (u32, u32) = (4, 0);

/// The kinds no reader of a pack's files reads; anything else under `assets/` stays. The 26.2 client lists resources
/// by fixed suffixes (`.json`, `.png`, `.png.mcmeta`, `.ogg`, `.vsh`, `.fsh`, `.glsl`) or reads the path a JSON file
/// names, and OptiFine (HD U J9, `net/optifine`) by `.properties`, `.png`, `.jem`, `.jpm`, `.json` and `.mcmeta` or
/// the path a properties file names: none of the 26.2 classes nor OptiFine's has any suffix below. MTR3 reads a
/// `.bbmodel` from any path of its namespace (`CustomResources`), so `assets/mtr/` is left whole.
const UNREAD: [(&str, Reason); 6] = [
    // Blockbench projects: the client and OptiFine read models as `.json`, `.jem` and `.jpm` only.
    (".bbmodel", Reason::Project),
    // Backups and leftovers.
    (".bak", Reason::Backup),
    (".old", Reason::Backup),
    (".orig", Reason::Backup),
    ("~", Reason::Backup),
    // JSON with Comments, an editor format: the client's readers take `.json` only.
    (".jsonc", Reason::Source),
];

/// Why a file no client reads is dropped, in report order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Reason {
    OutsideAssets,
    Unnameable,
    Project,
    Backup,
    Source,
    Archive,
}

impl Reason {
    const ALL: [Self; 6] = [
        Self::OutsideAssets,
        Self::Unnameable,
        Self::Project,
        Self::Backup,
        Self::Source,
        Self::Archive,
    ];

    fn text(self) -> &'static str {
        match self {
            Self::OutsideAssets => "outside assets/ of the root and the declared overlays",
            Self::Unnameable => "system or hidden, a path no client of the views can name",
            Self::Project => "an editor project no client or mod reads",
            Self::Backup => "a backup or leftover no reader asks for",
            Self::Source => "a commented JSON source no reader opens",
            // The only 26.2 class that opens an archive from a pack is `UnihexProvider$Definition`, for `hex_file`.
            Self::Archive => "an archive no font's hex_file names",
        }
    }
}

/// Drops the files no client of a declared view can read: outside `assets/` of the root and of the declared overlays
/// (`pack.mcmeta` and `pack.png` of the root excepted), system or hidden files whose path no client can name, and
/// under `assets/` a file 26.2 does not classify, of a kind in [`UNREAD`] or an archive, that no reference and no font
/// names. Every other file stays, whatever reads it: an older client, OptiFine, a mod. Pinned paths (sources,
/// `force_include`) stay.
pub(crate) struct DropUnread;

impl Pass for DropUnread {
    fn name(&self) -> &'static str {
        "drop-unread"
    }

    fn stage(&self) -> Stage {
        Stage::Json
    }

    fn loss(&self) -> Loss {
        Loss::Lossless
    }

    fn options_schema(&self) -> schemars::Schema {
        no_options()
    }

    fn run(&self, cx: &PassCx<'_>) -> Result<Edits, PassError> {
        let oldest = cx.layout.views.iter().map(|view| view.formats.min).min();
        let checked = oldest.is_some_and(|format| format >= VALID_PATHS);
        let named = named(cx);
        let fonts = font_files(cx);
        let mut edits = Edits::default();
        let mut dropped: Vec<(Reason, u64)> = Vec::new();
        let mut pinned = Vec::new();
        let mut audio = Vec::new();
        let mut found = Vec::new();
        for (path, entry) in &cx.pack.files {
            let reason = match cx.layout.split(path) {
                (Layer::Root, None) if matches!(path.as_str(), "pack.mcmeta" | "pack.png") => None,
                (_, None) => Some(Reason::OutsideAssets),
                (_, Some(logical)) => {
                    let text = logical.as_str();
                    if text.contains("/sounds/")
                        && audio::DECODABLE_EXTENSIONS
                            .iter()
                            .any(|ext| text.ends_with(&format!(".{ext}")))
                    {
                        audio.push(path.as_str());
                    }
                    if checked && system_or_hidden(path) && !nameable(text) {
                        Some(Reason::Unnameable)
                    } else if kind(cx, path) == FileKind::Unknown
                        && !text.starts_with("assets/mtr/")
                        && !named.contains(&logical)
                        && !fonts.names(&logical)
                    {
                        unread(text)
                    } else {
                        None
                    }
                }
            };
            if let Some(reason) = reason {
                found.push((path, entry, reason));
            }
        }
        // A file the pass cannot read as its reader does may name any candidate: then none goes for its kind.
        let by_kind =
            |reason: &Reason| !matches!(reason, Reason::OutsideAssets | Reason::Unnameable);
        let (mentioned, unreadable) = if found.iter().any(|(_, _, reason)| by_kind(reason)) {
            match mentioned(cx, &found) {
                Ok(mentioned) => (mentioned, None),
                Err(path) => (BTreeSet::new(), Some(path)),
            }
        } else {
            (BTreeSet::new(), None)
        };
        let mut named_by_text = Vec::new();
        for (path, entry, reason) in found {
            if cx.pinned.contains(path) {
                pinned.push(path.as_str());
            } else if unreadable.is_some() && by_kind(&reason) {
                continue;
            } else if mentioned.contains(path) {
                named_by_text.push(path.as_str());
            } else {
                edits.removed.insert(path.clone());
                dropped.push((reason, entry.blob.len() as u64));
            }
        }
        edits.changes.push(format!(
            "{} files no client reads dropped",
            edits.removed.len()
        ));
        if let Some(path) = &unreadable {
            edits
                .changes
                .push(format!("drop by kind skipped: {path} unreadable"));
        }
        for reason in Reason::ALL {
            let of: Vec<u64> = dropped
                .iter()
                .filter(|(r, _)| *r == reason)
                .map(|(_, bytes)| *bytes)
                .collect();
            if !of.is_empty() {
                edits.changes.push(format!(
                    "{} files, {} bytes: {}",
                    of.len(),
                    of.iter().sum::<u64>(),
                    reason.text()
                ));
            }
        }
        if !pinned.is_empty() {
            edits.changes.push(listed(
                "files no client reads kept, a source names them",
                &pinned,
            ));
        }
        if !named_by_text.is_empty() {
            edits.changes.push(listed(
                "files of a kind no reader asks for kept, a text file of the pack names them",
                &named_by_text,
            ));
        }
        if !audio.is_empty() {
            edits.changes.push(listed(
                "non-Ogg audio files kept, the client reads only .ogg",
                &audio,
            ));
        }
        Ok(edits)
    }
}

/// The candidates of a kind rule whose file name a kept file contains. A reader such as OptiFine opens the path a
/// properties file names whatever its suffix (`TextureAnimations` reads `from=./frames.png.bak` as written), and
/// every form it resolves (`./`, `~/`, `namespace:`) ends with the file name, so the name alone is matched, ignoring
/// ASCII case since 1.12 lowercases the path. A `.properties` file is also read as `java.util.Properties` loads it
/// (Latin-1, escapes, continuations), every other file as raw bytes; a candidate a namer keeps names in turn. A
/// false keep costs bytes, a false drop loses what a reader loads.
fn mentioned(
    cx: &PassCx<'_>,
    found: &[(&PhysPath, &Entry, Reason)],
) -> Result<BTreeSet<PhysPath>, PhysPath> {
    let mut candidates: Vec<(&PhysPath, Vec<u8>)> = found
        .iter()
        .filter(|(path, _, reason)| {
            !matches!(reason, Reason::OutsideAssets | Reason::Unnameable)
                && !cx.pinned.contains(*path)
        })
        .map(|(path, _, _)| {
            let name = path.as_str().rsplit('/').next().unwrap_or_default();
            (*path, name.to_ascii_lowercase().into_bytes())
        })
        .collect();
    let dropping: BTreeSet<&PhysPath> = found
        .iter()
        .filter(|(path, _, _)| !cx.pinned.contains(*path))
        .map(|(path, _, _)| *path)
        .collect();
    let mut namers: Vec<&PhysPath> = cx
        .pack
        .files
        .keys()
        .filter(|path| !dropping.contains(path))
        .collect();
    let mut mentioned = BTreeSet::new();
    while !namers.is_empty() && !candidates.is_empty() {
        let mut next = Vec::new();
        for path in namers {
            let texts = texts(path, &cx.pack.files[path].blob).ok_or_else(|| path.clone())?;
            candidates.retain(|(candidate, name)| {
                let named = texts.iter().any(|text| contains(text, name));
                if named {
                    mentioned.insert((*candidate).clone());
                    next.push(*candidate);
                }
                !named
            });
        }
        namers = next;
    }
    Ok(mentioned)
}

/// What a file says to its reader, lowercased: its bytes, and the text a reader decodes from them, for a properties
/// file its keys and values as `java.util.Properties` loads them, for a JSON file (Gson, also OptiFine's `.jem` and
/// `.jpm`) its names and strings. `None` when the pass cannot decode the file as that reader would. Other text files
/// are read as they are written.
fn texts(path: &PhysPath, bytes: &[u8]) -> Option<Vec<Vec<u8>>> {
    let mut texts = vec![bytes.to_ascii_lowercase()];
    let path = path.as_str();
    if path.ends_with(".properties") {
        for entry in properties::parse_checked(bytes).ok()? {
            texts.push(entry.key.to_ascii_lowercase().into_bytes());
            texts.push(entry.value.to_ascii_lowercase().into_bytes());
        }
    } else if [".json", ".mcmeta", ".jem", ".jpm"]
        .iter()
        .any(|ext| path.ends_with(ext))
    {
        json::visit_lenient(bytes, |token, _| {
            if matches!(token.kind, TokenKind::String | TokenKind::Name) {
                texts.push(token.text.to_ascii_lowercase().into_bytes());
            }
        })
        .ok()?;
    }
    Some(texts)
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty()
        && haystack
            .windows(needle.len())
            .any(|window| window == needle)
}

/// The reason a file of an unread kind goes, by its name.
fn unread(logical: &str) -> Option<Reason> {
    let file = logical.rsplit('/').next().unwrap_or(logical);
    if file.ends_with(".zip") {
        return Some(Reason::Archive);
    }
    UNREAD
        .iter()
        .find(|(suffix, _)| file.ends_with(suffix))
        .map(|(_, reason)| *reason)
}

/// What the pack's fonts name, read as the client reads them (`FontManager.GSON`, lenient).
struct FontFiles {
    files: BTreeSet<LogicalPath>,
    /// A `legacy_unicode` `template`: every page between its prefix and suffix.
    templates: Vec<(String, String)>,
}

impl FontFiles {
    fn names(&self, logical: &LogicalPath) -> bool {
        let text = logical.as_str();
        self.files.contains(logical)
            || self.templates.iter().any(|(prefix, suffix)| {
                text.len() >= prefix.len() + suffix.len()
                    && text.starts_with(prefix.as_str())
                    && text.ends_with(suffix.as_str())
            })
    }
}

/// Every file the providers of every font that parses leniently name.
fn font_files(cx: &PassCx<'_>) -> FontFiles {
    let mut fonts = FontFiles {
        files: BTreeSet::new(),
        templates: Vec::new(),
    };
    for (path, entry) in &cx.pack.files {
        if kind(cx, path) != FileKind::Font {
            continue;
        }
        // A font that does not parse is a JSON namer the pass cannot read, which stops every drop by kind.
        let Ok(root) = json::parse_lenient(&entry.blob) else {
            continue;
        };
        for provider in root.path("providers").items() {
            let kind = provider.path("type").as_str().unwrap_or_default();
            let fields: &[(&str, &str)] = match kind.strip_prefix("minecraft:").unwrap_or(kind) {
                "bitmap" => &[("file", "textures/")],
                "ttf" => &[("file", "font/")],
                "unihex" => &[("hex_file", "")],
                "legacy_unicode" => &[("sizes", "")],
                _ => &[],
            };
            for (field, folder) in fields {
                if let Some(logical) = provider
                    .path(field)
                    .as_str()
                    .and_then(|id| file(id, folder))
                    .and_then(|text| LogicalPath::new(&text).ok())
                {
                    fonts.files.insert(logical);
                }
            }
            if let Some(template) = provider.path("template").as_str()
                && let Some((prefix, suffix)) = template.split_once("%s")
                && let Some(prefix) = file(prefix, "textures/")
            {
                fonts.templates.push((prefix, suffix.to_owned()));
            }
        }
    }
    fonts
}

/// The logical path of an id, under `folder` of its namespace.
fn file(id: &str, folder: &str) -> Option<String> {
    let (namespace, path) = id.split_once(':').unwrap_or(("minecraft", id));
    (!path.is_empty()).then(|| format!("assets/{namespace}/{folder}{path}"))
}

/// Every logical path a reference names, in any view: a `hex_file` archive, a texture, a sound.
fn named(cx: &PassCx<'_>) -> BTreeSet<LogicalPath> {
    cx.index
        .all()
        .values()
        .flatten()
        .filter_map(|reference| match &reference.names {
            Names::File { logical, .. } | Names::Beside { logical } | Names::Text { logical } => {
                Some(logical.clone())
            }
            _ => None,
        })
        .collect()
}

fn listed(what: &str, paths: &[&str]) -> String {
    let shown: Vec<&str> = paths.iter().take(5).copied().collect();
    format!(
        "{} {what}: {}{}",
        paths.len(),
        shown.join(", "),
        if paths.len() > 5 { ", ..." } else { "" }
    )
}

/// A file an operating system writes on its own or hides: a path part with a leading dot (hidden on Unix and macOS:
/// `.DS_Store`, AppleDouble `._*`, `.git/`), what Windows Explorer writes in a folder (`Thumbs.db`, `desktop.ini`,
/// in any case), the `__MACOSX/` that macOS's Archive Utility adds to a zip, and Finder's `Icon\r`.
fn system_or_hidden(path: &PhysPath) -> bool {
    path.as_str().split('/').any(|part| {
        part.starts_with('.')
            || part.eq_ignore_ascii_case("thumbs.db")
            || part.eq_ignore_ascii_case("desktop.ini")
            || part == "__MACOSX"
            || part == "Icon\r"
    })
}

/// A logical path `assets/<namespace>/<path>` a client from 1.13 on can name: both parts in its character set.
fn nameable(logical: &str) -> bool {
    let Some((namespace, path)) = logical
        .strip_prefix("assets/")
        .and_then(|rest| rest.split_once('/'))
    else {
        return false;
    };
    let valid = |text: &str, slash: bool| {
        text.bytes().all(|byte| {
            matches!(byte, b'a'..=b'z' | b'0'..=b'9' | b'_' | b'.' | b'-') || slash && byte == b'/'
        })
    };
    valid(namespace, false) && valid(path, true)
}
