use std::io;

use camino::{Utf8Path, Utf8PathBuf};
use packprune_graph::SourceError;
use packprune_pack::rules::Glob;
use packprune_pack::{Digest, RunCx, pattern::JavaPattern};
use serde_json::Value;

use crate::{hash_part, refused, relative, rules::Rule};

pub(super) struct Profile {
    name: String,
    pub files: Vec<Glob>,
    pub exclude: Vec<Glob>,
    pub placeholders: Vec<JavaPattern>,
    pub rules: Vec<KeyRule>,
}

pub(super) struct KeyRule {
    key: Glob,
    under: Option<String>,
    pub usage: Rule,
}

impl KeyRule {
    pub fn matches(&self, keys: &[String]) -> bool {
        keys.last().is_some_and(|k| self.key.matches(&normalise(k)))
            && self
                .under
                .as_ref()
                .is_none_or(|under| keys.len() > 1 && *under == normalise(&keys[keys.len() - 2]))
    }
}

fn normalise(key: &str) -> String {
    let mut out = String::new();
    let mut previous = '\0';
    for c in key.chars() {
        if c.is_ascii_uppercase() && (previous.is_ascii_lowercase() || previous.is_ascii_digit()) {
            out.push('_');
        }
        out.extend(if c == '-' { '_' } else { c }.to_lowercase());
        previous = c;
    }
    out
}

impl Profile {
    pub fn generic(cx: &RunCx) -> Result<Self, SourceError> {
        Self::parse(
            include_str!("../../data/generic.json"),
            "built-in profile generic",
            cx,
        )
    }

    /// The profiles and the digest of the user ones, from one read of each file: a profile saved meanwhile never
    /// gives rules of one version under the digest of another.
    pub fn load(root: &Utf8Path, cx: &RunCx) -> Result<(Vec<Self>, Digest), SourceError> {
        let mut profiles = vec![Self::generic(cx)?];
        let mut digest = blake3::Hasher::new();
        for file in Self::user_files(root)? {
            let bytes = std::fs::read(&file)?;
            hash_part(&mut digest, relative(&file, root).as_bytes());
            hash_part(&mut digest, blake3::hash(&bytes).as_bytes());
            let text = String::from_utf8(bytes).map_err(|_| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "stream did not contain valid UTF-8",
                )
            })?;
            profiles.push(Self::parse(&text, file.as_str(), cx)?);
        }
        for (i, profile) in profiles.iter().enumerate() {
            if profiles[..i].iter().any(|p| p.name == profile.name) {
                return Err(refused(
                    "profiles",
                    format!("two profiles are called {}", profile.name),
                ));
            }
        }
        Ok((profiles, Digest(*digest.finalize().as_bytes())))
    }

    pub fn user_files(root: &Utf8Path) -> Result<Vec<Utf8PathBuf>, SourceError> {
        let folder = root.join("packprune-profiles");
        if !folder.is_dir() {
            return Ok(Vec::new());
        }
        let mut files = Vec::new();
        for entry in std::fs::read_dir(folder)? {
            let path = Utf8PathBuf::from_path_buf(entry?.path())
                .map_err(|p| refused(p.display(), "not a UTF-8 path"))?;
            if path.extension() == Some("json") {
                files.push(path);
            }
        }
        files.sort_by(|a, b| a.as_str().cmp(b.as_str()));
        Ok(files)
    }

    fn parse(json: &str, where_: &str, cx: &RunCx) -> Result<Self, SourceError> {
        let node: Value = serde_json::from_str(json)
            .map_err(|e| refused(where_, format!("not valid JSON: {e}")))?;
        only(
            &node,
            &[
                "version",
                "name",
                "why",
                "files",
                "exclude",
                "placeholders",
                "rules",
            ],
            where_,
        )?;
        if node["version"].as_i64() != Some(1) {
            return Err(refused(where_, "expected version: 1"));
        }
        let name = node["name"]
            .as_str()
            .filter(|s| !s.trim().is_empty())
            .ok_or_else(|| refused(where_, "a profile needs a name"))?
            .to_owned();
        let mut files = globs(&node, "files");
        if files.is_empty() {
            files.push(Glob::path("**"));
        }
        let mut rules = Vec::new();
        for r in array(&node, "rules") {
            only(
                r,
                &[
                    "key",
                    "under",
                    "kind",
                    "atlas",
                    "id",
                    "namespaced",
                    "quiet",
                    "why",
                ],
                where_,
            )?;
            let key = r["key"]
                .as_str()
                .filter(|s| !s.trim().is_empty())
                .ok_or_else(|| refused(where_, "every rule needs a key"))?;
            rules.push(KeyRule {
                key: Glob::id(&normalise(key)),
                under: r.get("under").map(|v| normalise(v.as_str().unwrap_or(""))),
                usage: Rule::parse(r, where_, cx)?,
            });
        }
        let placeholders = array(&node, "placeholders")
            .iter()
            .map(|v| {
                JavaPattern::new(v.as_str().unwrap_or(""))
                    .map_err(|e| refused(where_, format!("invalid placeholder regex: {e}")))
            })
            .collect::<Result<_, _>>()?;
        Ok(Self {
            name,
            files,
            exclude: globs(&node, "exclude"),
            placeholders,
            rules,
        })
    }
}

fn only(node: &Value, fields: &[&str], where_: &str) -> Result<(), SourceError> {
    if let Some(object) = node.as_object() {
        for field in object.keys() {
            if !fields.contains(&field.as_str()) {
                return Err(refused(where_, format!("unknown field '{field}'")));
            }
        }
    }
    Ok(())
}
fn array<'a>(node: &'a Value, field: &str) -> &'a [Value] {
    node[field].as_array().map(Vec::as_slice).unwrap_or(&[])
}
fn globs(node: &Value, field: &str) -> Vec<Glob> {
    array(node, field)
        .iter()
        .map(|v| Glob::path(v.as_str().unwrap_or("")))
        .collect()
}
