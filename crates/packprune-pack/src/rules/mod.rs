use std::cmp::Ordering;
use std::collections::BTreeMap;

use serde_json::Value;

#[cfg(test)]
mod tests;

const CLIENT_RULES: &str = include_str!("../../data/client-rules.json");
const CODE_LOADED: &str = include_str!("../../data/vanilla-code-loaded.txt");

/// What the client's code decides and its assets do not say, for one client version: which atlases a reference kind
/// looks in, the folders code prefixes to an id (`client-rules.json`), and the vanilla files code loads by name
/// (`vanilla-code-loaded.txt`). Ids are text (`minecraft:missingno`): a run interns them with its own interner.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ClientRules {
    /// The version whose rules these are: the client's, or the newest when it has none.
    pub version: String,
    /// Set when the client version has no rules of its own.
    pub warning: Option<String>,
    pub missing_sprite: String,
    pub builtin_model_prefix: String,
    pub model_atlases: Vec<String>,
    pub particle_atlases: Vec<String>,
    pub waypoint_sprites: SpriteRule,
    pub post_effect_textures: FileRule,
    pub shader_includes: FileRule,
    pub equipment_textures: FileRule,
    /// By special model type (`minecraft:chest`).
    pub special_textures: BTreeMap<String, TextureRule>,
    pub tooltip_sprites: Vec<SpriteRule>,
    pub code_loaded: Vec<Glob>,
}

/// How a texture field becomes a file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TextureRule {
    Sprite(SpriteRule),
    File(FileRule),
}

/// An atlas sprite: `prefix` and `suffix` wrap the id's path, then the atlases resolve it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SpriteRule {
    pub atlases: Vec<String>,
    pub prefix: String,
    pub suffix: String,
}

/// A file: `assets/<ns>/<folder>/<path><suffix>`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FileRule {
    pub folder: String,
    pub suffix: String,
}

/// A usage or selector pattern. Over paths, `*` stays within a folder and `**` crosses folders; over ids,
/// `*` matches anything, since an id such as `example:furniture/chair` is one name. `?` is one character but `/`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Glob {
    pub pattern: String,
    path: bool,
}

impl ClientRules {
    /// The rules of `client_version`, or the newest ones with a warning when that version has none.
    pub fn for_version(client_version: &str) -> Self {
        let all: serde_json::Map<String, Value> =
            serde_json::from_str(CLIENT_RULES).expect("client-rules.json is JSON");
        let newest = all
            .keys()
            .max_by(|a, b| version_order(a, b))
            .expect("client-rules.json has a version");
        let own = all
            .keys()
            .find(|version| version_order(version, client_version) == Ordering::Equal);
        let (version, warning) = match own {
            Some(_) => (client_version, None),
            None => (
                newest.as_str(),
                Some(format!(
                    "client version {client_version} has no rules; using the rules of {newest}"
                )),
            ),
        };
        let rules = &all[own.unwrap_or(newest)];
        let text = |value: &Value| value.as_str().unwrap_or_default().to_owned();
        let strings = |value: &Value| {
            value
                .as_array()
                .map(|items| items.iter().map(text).collect())
                .unwrap_or_default()
        };
        let sprite = |value: &Value| SpriteRule {
            atlases: strings(&value["atlases"]),
            prefix: text(&value["prefix"]),
            suffix: text(&value["suffix"]),
        };
        let file = |value: &Value| FileRule {
            folder: text(&value["folder"]),
            suffix: text(&value["suffix"]),
        };
        let rule = |value: &Value| {
            if value.get("atlases").is_some() {
                TextureRule::Sprite(sprite(value))
            } else {
                TextureRule::File(file(value))
            }
        };
        Self {
            version: version.to_owned(),
            warning,
            missing_sprite: text(&rules["missing_sprite"]),
            builtin_model_prefix: text(&rules["builtin_model_prefix"]),
            model_atlases: strings(&rules["model_atlases"]),
            particle_atlases: strings(&rules["particle_atlases"]),
            waypoint_sprites: sprite(&rules["waypoint_sprites"]),
            post_effect_textures: file(&rules["post_effect_textures"]),
            shader_includes: file(&rules["shader_includes"]),
            equipment_textures: file(&rules["equipment_textures"]),
            special_textures: rules["special_textures"]
                .as_object()
                .into_iter()
                .flatten()
                .map(|(kind, value)| (kind.clone(), rule(value)))
                .collect(),
            tooltip_sprites: rules["tooltip_sprites"]
                .as_array()
                .into_iter()
                .flatten()
                .map(sprite)
                .collect(),
            code_loaded: CODE_LOADED
                .lines()
                .map(str::trim)
                .filter(|line| !line.is_empty() && !line.starts_with('#'))
                .map(Glob::path)
                .collect(),
        }
    }
}

impl SpriteRule {
    /// The sprite path a texture id's path names.
    pub fn of(&self, path: &str) -> String {
        format!("{}{path}{}", self.prefix, self.suffix)
    }

    /// The path [`Self::of`] turns into `sprite`, if any.
    pub fn unwrap<'a>(&self, sprite: &'a str) -> Option<&'a str> {
        (sprite.len() >= self.prefix.len() + self.suffix.len())
            .then(|| {
                sprite
                    .strip_prefix(&*self.prefix)?
                    .strip_suffix(&*self.suffix)
            })
            .flatten()
    }
}

impl FileRule {
    /// `assets/<namespace>/<folder>/<path><suffix>`, no folder segment when it is empty.
    pub fn file(&self, namespace: &str, path: &str) -> String {
        let folder = if self.folder.is_empty() {
            String::new()
        } else {
            format!("{}/", self.folder)
        };
        format!("assets/{namespace}/{folder}{path}{}", self.suffix)
    }
}

impl Glob {
    pub fn path(pattern: &str) -> Self {
        Self {
            pattern: pattern.to_owned(),
            path: true,
        }
    }

    pub fn id(pattern: &str) -> Self {
        Self {
            pattern: pattern.to_owned(),
            path: false,
        }
    }

    pub fn matches(&self, value: &str) -> bool {
        enum Part {
            Any,
            Segment,
            One,
            Literal(char),
        }
        let mut parts = Vec::new();
        let mut chars = self.pattern.chars().peekable();
        while let Some(c) = chars.next() {
            parts.push(match c {
                '*' if chars.next_if_eq(&'*').is_some() => Part::Any,
                '*' if self.path => Part::Segment,
                '*' => Part::Any,
                '?' => Part::One,
                c => Part::Literal(c),
            });
        }
        let value: Vec<char> = value.chars().collect();
        // matched[i]: the parts so far match the first i characters of the value.
        let mut matched = vec![false; value.len() + 1];
        matched[0] = true;
        for part in &parts {
            let mut next = vec![false; value.len() + 1];
            for i in 0..=value.len() {
                next[i] = match part {
                    Part::Any => matched[i] || i > 0 && next[i - 1],
                    Part::Segment => matched[i] || i > 0 && next[i - 1] && value[i - 1] != '/',
                    Part::One => i > 0 && matched[i - 1] && value[i - 1] != '/',
                    Part::Literal(c) => i > 0 && matched[i - 1] && value[i - 1] == *c,
                };
            }
            matched = next;
        }
        matched[value.len()]
    }
}

/// Version order: parts split on `.` and `-`, compared as numbers when both are digits, so 26.10 sorts after
/// 26.2.
fn version_order(a: &str, b: &str) -> Ordering {
    let x = java_split(a);
    let y = java_split(b);
    for (x, y) in x.iter().zip(&y) {
        let digits =
            |part: &str| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit());
        let order = if digits(x) && digits(y) {
            let (x, y) = (x.trim_start_matches('0'), y.trim_start_matches('0'));
            x.len().cmp(&y.len()).then_with(|| x.cmp(y))
        } else {
            utf16_order(x, y)
        };
        if order != Ordering::Equal {
            return order;
        }
    }
    x.len().cmp(&y.len())
}

/// `String.split("[.-]")`: trailing empty parts dropped, and the whole string when nothing splits it.
fn java_split(text: &str) -> Vec<&str> {
    if !text.contains(['.', '-']) {
        return vec![text];
    }
    let mut parts: Vec<&str> = text.split(['.', '-']).collect();
    while parts.last() == Some(&"") {
        parts.pop();
    }
    parts
}

/// `String.compareTo`: by UTF-16 code unit.
fn utf16_order(a: &str, b: &str) -> Ordering {
    a.encode_utf16().cmp(b.encode_utf16())
}
