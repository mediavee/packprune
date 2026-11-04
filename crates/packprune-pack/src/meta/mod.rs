use std::collections::{BTreeSet, HashSet};
use std::fmt;

use crate::PackInput;
use crate::json::{self, Node};

#[cfg(test)]
mod tests;

/// A pack format as the 26.x client compares it: `(major, minor)`. Each is a Java `int`, so a bound is at most
/// `i32::MAX`, and a bare major as an upper bound covers every minor ([`ANY_MINOR`]).
pub type Format = (u32, u32);

pub const ANY_MINOR: u32 = i32::MAX as u32;
const MAX_MAJOR: u32 = i32::MAX as u32;

/// An inclusive interval of pack formats, majors and minors.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FormatRange {
    pub min: (u32, u32),
    pub max: (u32, u32),
}

/// The pack's own range, its overlay entries and its `filter.block` patterns, parsed from `pack.mcmeta`.
/// `formats` is `None` when the pack declares no range of its own.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PackMeta {
    pub formats: Option<FormatRange>,
    pub overlays: Vec<OverlayEntry>,
    pub filters: Vec<BlockFilter>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OverlayEntry {
    pub formats: FormatRange,
    pub directory: String,
}

/// One `filter.block` entry: Java regexes found anywhere in an id's namespace and path; `None` matches everything.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BlockFilter {
    pub namespace: Option<String>,
    pub path: Option<String>,
}

/// The overlays active at one format, in priority order (last wins), and the format that
/// selects them. `verified` when the client jar can check it, which stands for the jar's own format.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ViewSpec {
    pub id: String,
    pub format: Format,
    pub overlays: Vec<String>,
    pub verified: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum MetaError {
    #[error("{pack}: no pack.mcmeta at the root, not a resource pack")]
    Missing { pack: String },
    #[error("{pack}: unreadable pack.mcmeta: {why}")]
    Unreadable { pack: String, why: String },
}

impl FormatRange {
    pub fn contains(&self, format: Format) -> bool {
        self.min <= format && format <= self.max
    }
}

/// The next format up: the next minor, or the next major once every minor is covered.
pub fn next_format((major, minor): Format) -> Format {
    if minor != ANY_MINOR {
        (major, minor + 1)
    } else if major == MAX_MAJOR {
        (major, minor)
    } else {
        (major + 1, 0)
    }
}

pub fn previous_format((major, minor): Format) -> Format {
    if minor == ANY_MINOR || minor > 0 {
        (
            major,
            if minor == ANY_MINOR {
                ANY_MINOR - 1
            } else {
                minor - 1
            },
        )
    } else if major == 0 {
        (major, minor)
    } else {
        (major - 1, ANY_MINOR)
    }
}

/// `88.0`, or `88.*` for any minor.
pub fn show_format((major, minor): Format) -> impl fmt::Display {
    struct Shown(u32, u32);
    impl fmt::Display for Shown {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            if self.1 == ANY_MINOR {
                write!(f, "{}.*", self.0)
            } else {
                write!(f, "{}.{}", self.0, self.1)
            }
        }
    }
    Shown(major, minor)
}

/// `88.0` or `88`, as `version.json` gives it.
pub fn parse_format(value: &str) -> Option<Format> {
    let mut parts = value.split('.');
    let major = java_int(parts.next()?)?;
    let minor = parts.next().map_or(Some(0), java_int)?;
    Some((major, minor))
}

/// `Integer.parseInt` restricted to what a `Format` holds.
fn java_int(text: &str) -> Option<u32> {
    text.parse::<i32>()
        .ok()
        .and_then(|value| u32::try_from(value).ok())
}

impl PackMeta {
    /// `pack.mcmeta` at the root of `pack`.
    pub fn read(pack: &PackInput) -> Result<Self, MetaError> {
        let path = crate::PhysPath::new("pack.mcmeta").expect("a valid path");
        let entry = pack.files.get(&path).ok_or_else(|| MetaError::Missing {
            pack: pack.name.clone(),
        })?;
        Self::parse(&entry.blob).map_err(|why| MetaError::Unreadable {
            pack: pack.name.clone(),
            why,
        })
    }

    /// `pack.mcmeta`'s bytes parsed with Jackson's coercions: a `null` or missing number reads
    /// as 0, a string number as its value, a fractional one truncated.
    pub fn parse(bytes: &[u8]) -> Result<Self, String> {
        let root = json::parse(bytes).map_err(|error| error.to_string())?;
        let formats = range(root.path("pack"), "supported_formats", Some("pack_format"))?;
        let mut overlays = Vec::new();
        for entry in root.path("overlays").path("entries").children() {
            let Some(directory) = entry.get("directory").and_then(Node::as_str) else {
                return Err("pack.mcmeta: overlay entry without a directory".into());
            };
            let formats = overlay_range(entry)?;
            overlays.push(OverlayEntry {
                formats,
                directory: directory.to_owned(),
            });
        }
        let pattern =
            |block: &Node, field| block.get(field).and_then(Node::as_str).map(str::to_owned);
        let filters = root
            .path("filter")
            .path("block")
            .children()
            .map(|block| BlockFilter {
                namespace: pattern(block, "namespace"),
                path: pattern(block, "path"),
            })
            .collect();
        Ok(Self {
            formats,
            overlays,
            filters,
        })
    }

    /// One view per distinct set of active overlays over every format the pack declares: its own range and each
    /// overlay's, since overlays exist for the clients outside the pack's range. The set only changes at a range
    /// bound, so sampling each bound and the formats just outside it finds every set. The formats of `jar`'s major
    /// that share its set get the one verified view; any other format gets an unverified view, even with the same
    /// set, since its vanilla and its client rules may differ. Without `jar`, every view is verified.
    pub fn views(&self, jar: Option<Format>) -> Vec<ViewSpec> {
        let mut points = self.bounds();
        let jar_set = jar.and_then(|jar| self.overlays_at(jar));
        if let (Some(jar), Some(_)) = (jar, &jar_set) {
            points.insert(jar);
        }
        // Insertion order matters: the first format of each key selects it.
        let mut sets: Vec<((Vec<&str>, bool), Format)> = Vec::new();
        let mut seen = HashSet::new();
        for point in points {
            if !self.declares(point) {
                continue;
            }
            let active = self.active(point);
            let at_jar = match (&jar_set, jar) {
                (Some(set), Some(jar)) => *set == active && point.0 == jar.0,
                _ => false,
            };
            let key = (active, at_jar);
            if seen.insert(key.clone()) {
                sets.push((key, point));
            }
        }
        let verified_set = sets
            .iter()
            .find(|((_, at_jar), _)| *at_jar)
            .map(|((active, _), _)| active);
        sets.iter()
            .map(|((active, at_jar), format)| {
                let mut id = overlays_id(active);
                // The same overlays as the verified view, at a format of their own.
                if !at_jar && verified_set == Some(active) {
                    id = format!("{id}@{}", show_format(*format));
                }
                ViewSpec {
                    id,
                    format: *format,
                    overlays: active.iter().map(|&overlay| overlay.to_owned()).collect(),
                    verified: jar.is_none() || *at_jar,
                }
            })
            .collect()
    }

    /// Each bound of every range the pack declares, and the formats just outside it: the active overlays only change
    /// there, so a format of each interval between two of them is among them.
    pub fn bounds(&self) -> BTreeSet<Format> {
        let mut points = BTreeSet::new();
        for range in self.declared() {
            points.insert(previous_format(range.min));
            points.insert(range.min);
            points.insert(range.max);
            points.insert(next_format(range.max));
        }
        if points.is_empty() {
            points.insert((0, 0));
        }
        points
    }

    /// True when a client at `format` is one the pack declares: its range or an overlay's names it.
    pub fn declares(&self, format: Format) -> bool {
        let mut declared = self.declared().peekable();
        declared.peek().is_none() || declared.any(|range| range.contains(format))
    }

    /// The overlays a client at `format` applies, in priority order.
    pub fn active(&self, format: Format) -> Vec<&str> {
        self.overlays
            .iter()
            .filter(|overlay| overlay.formats.contains(format))
            .map(|overlay| overlay.directory.as_str())
            .collect()
    }

    fn declared(&self) -> impl Iterator<Item = &FormatRange> {
        self.formats
            .iter()
            .chain(self.overlays.iter().map(|overlay| &overlay.formats))
    }

    /// The overlays active at `format`, `None` when the pack does not declare that format.
    fn overlays_at(&self, format: Format) -> Option<Vec<&str>> {
        let declared = self
            .formats
            .map_or(self.overlays.is_empty(), |range| range.contains(format))
            || self
                .overlays
                .iter()
                .any(|overlay| overlay.formats.contains(format));
        declared.then(|| self.active(format))
    }
}

/// An overlay entry's `min_format`/`max_format` range, falling back to `formats`.
pub fn overlay_range(entry: &Node) -> Result<FormatRange, String> {
    range(entry, "formats", None)?.ok_or_else(|| {
        format!(
            "pack.mcmeta: overlay {} without formats",
            entry.path("directory").as_string("")
        )
    })
}

/// A view's name before any `@<format>`: `base`, or its overlays joined by `+`.
pub(crate) fn overlays_id(active: &[&str]) -> String {
    if active.is_empty() {
        "base".to_owned()
    } else {
        active.join("+")
    }
}

/// `min_format`/`max_format` first, else the older integer field (`formats` or `supported_formats`, then
/// `pack_format`).
fn range(
    node: &Node,
    range_field: &str,
    single_field: Option<&str>,
) -> Result<Option<FormatRange>, String> {
    if node.has("min_format") || node.has("max_format") {
        let min = node
            .get("min_format")
            .map(|min| format(min, 0))
            .transpose()?;
        let max = node
            .get("max_format")
            .map(|max| format(max, ANY_MINOR))
            .transpose()?;
        return Ok(Some(FormatRange {
            min: min.unwrap_or((0, 0)),
            max: max.unwrap_or((MAX_MAJOR, ANY_MINOR)),
        }));
    }
    if let Some(range) = node.get(range_field) {
        if let Some(major) = range.int() {
            return majors(major, major);
        }
        if range.is_array() && range.size() == 2 {
            return majors(as_int(&range.items()[0])?, as_int(&range.items()[1])?);
        }
        if range.is_object() {
            return majors(
                as_int(range.path("min_inclusive"))?,
                as_int(range.path("max_inclusive"))?,
            );
        }
        return Err(format!("pack.mcmeta: unreadable {range_field}"));
    }
    if let Some(single) = single_field.and_then(|field| node.get(field)) {
        let major = as_int(single)?;
        return majors(major, major);
    }
    Ok(None)
}

fn majors(min: i32, max: i32) -> Result<Option<FormatRange>, String> {
    Ok(Some(FormatRange {
        min: (unsigned(min)?, 0),
        max: (unsigned(max)?, ANY_MINOR),
    }))
}

fn format(node: &Node, default_minor: u32) -> Result<Format, String> {
    if let Some(major) = node.int() {
        return Ok((unsigned(major)?, default_minor));
    }
    if node.is_array() && node.size() > 0 {
        let minor = if node.size() > 1 {
            unsigned(as_int(&node.items()[1])?)?
        } else {
            default_minor
        };
        return Ok((unsigned(as_int(&node.items()[0])?)?, minor));
    }
    Err("pack.mcmeta: unreadable format".into())
}

fn as_int(node: &Node) -> Result<i32, String> {
    node.try_as_int()
        .ok_or_else(|| "pack.mcmeta: a format is not an int".to_owned())
}

/// No client has a negative format, and a `Format` is unsigned.
fn unsigned(value: i32) -> Result<u32, String> {
    u32::try_from(value).map_err(|_| format!("pack.mcmeta: negative format {value}"))
}
