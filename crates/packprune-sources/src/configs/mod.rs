mod profile;
mod tags;

use camino::{Utf8Path, Utf8PathBuf};
use packprune_codec::properties;
use packprune_graph::{NoCache, SourceError, SubCache, Usage, UsageSource};
use packprune_pack::{Digest, RunCx};

use crate::{
    digest_files,
    document::{self, Node, Value},
    files, hash_part, memo, refused, relative,
    rules::{self, Rule},
};
use profile::Profile;

const STEP: &str = "configs-file";

pub fn open(path: &Utf8Path, cx: &RunCx) -> Result<Box<dyn UsageSource>, SourceError> {
    Ok(Box::new(Configs {
        inputs: vec![path.to_owned()],
        cx: cx.clone(),
        excluded: Vec::new(),
    }))
}

struct Configs {
    inputs: Vec<Utf8PathBuf>,
    cx: RunCx,
    excluded: Vec<Utf8PathBuf>,
}

impl UsageSource for Configs {
    fn inputs(&self) -> &[Utf8PathBuf] {
        &self.inputs
    }

    fn input_digest(&self, input: &Utf8Path) -> Result<Digest, SourceError> {
        if !self.inputs.iter().any(|p| p == input) {
            return Err(refused(input, "not an input of this source"));
        }
        digest(input, &self.cx, &self.excluded)
    }

    fn exclude(&mut self, dirs: &[Utf8PathBuf]) {
        self.excluded.extend_from_slice(dirs);
    }

    fn collect(&self) -> Result<Vec<Usage>, SourceError> {
        collect(&self.inputs[0], &self.cx, &self.excluded, &NoCache)
    }

    fn collect_memoised(&self, memo: &dyn SubCache) -> Result<Vec<Usage>, SourceError> {
        collect(&self.inputs[0], &self.cx, &self.excluded, memo)
    }
}

struct Text {
    keys: Vec<String>,
    path: String,
    value: String,
    line: usize,
}

fn texts(node: &Node, out: &mut Vec<Text>) {
    let mut pending = vec![(node, Vec::<String>::new(), String::new())];
    while let Some((node, keys, path)) = pending.pop() {
        match &*node.value {
            Value::Map(entries) => {
                for (key, child) in entries.iter().rev() {
                    let mut inner = keys.clone();
                    inner.push(key.clone());
                    let path = if path.is_empty() {
                        key.clone()
                    } else {
                        format!("{path}.{key}")
                    };
                    pending.push((child, inner, path));
                }
            }
            Value::List(list) => {
                for (i, child) in list.iter().enumerate().rev() {
                    pending.push((child, keys.clone(), format!("{path}[{i}]")));
                }
            }
            Value::Scalar(text, true) => out.push(Text {
                keys,
                path,
                value: text.clone(),
                line: node.line,
            }),
            _ => {}
        }
    }
}

/// The source's root, its profiles with the digest of the user ones, and the config files they select.
type Selection = (Utf8PathBuf, Vec<Profile>, Digest, Vec<Utf8PathBuf>);

fn selection(
    path: &Utf8Path,
    cx: &RunCx,
    excluded: &[Utf8PathBuf],
) -> Result<Selection, SourceError> {
    let base = Utf8PathBuf::from_path_buf(std::path::absolute(path)?)
        .map_err(|p| refused(p.display(), "not a UTF-8 path"))?;
    let root = if base.is_dir() {
        base.as_path()
    } else {
        base.parent()
            .expect("an absolute path that is no directory has a parent")
    };
    let (profiles, profiles_digest) = Profile::load(root, cx)?;
    // Resolved when the walk starts: the cache folder may appear during the run, before the sources.
    let excluded: Vec<Utf8PathBuf> = excluded
        .iter()
        .filter_map(|dir| dir.canonicalize_utf8().ok())
        .collect();
    let own = |dir: &Utf8Path| {
        !excluded.is_empty()
            && dir
                .canonicalize_utf8()
                .is_ok_and(|dir| excluded.contains(&dir))
    };
    let files = if base.is_file() {
        vec![base.clone()]
    } else {
        files(&base, &|p| {
            p.is_dir()
                && (p == root.join("packprune-profiles")
                    || p.join("pack.mcmeta").exists()
                    || own(p))
        })?
        .into_iter()
        .filter(|p| {
            is_config(p)
                && profiles.iter().all(|profile| {
                    profile
                        .exclude
                        .iter()
                        .all(|g| !g.matches(relative(p, root)))
                })
        })
        .collect()
    };
    let selected = files
        .into_iter()
        .filter(|file| {
            let relative = relative(file, root);
            profiles
                .iter()
                .any(|p| p.files.iter().any(|g| g.matches(relative)))
        })
        .collect();
    Ok((root.to_owned(), profiles, profiles_digest, selected))
}

fn digest(path: &Utf8Path, cx: &RunCx, excluded: &[Utf8PathBuf]) -> Result<Digest, SourceError> {
    let (root, _, _, mut selected) = selection(path, cx, excluded)?;
    selected.extend(Profile::user_files(&root)?);
    digest_files(&root, selected)
}

/// A file's usages are a function of its bytes, its path under the root and the profiles alone, so each is
/// memoised on those: an edited, added or removed file is the only one read again.
fn collect(
    path: &Utf8Path,
    cx: &RunCx,
    excluded: &[Utf8PathBuf],
    memo: &dyn SubCache,
) -> Result<Vec<Usage>, SourceError> {
    let (root, profiles, profiles_digest, files) = selection(path, cx, excluded)?;
    let mut out = Vec::new();
    for file in files {
        let relative = relative(&file, &root);
        let applicable: Vec<_> = profiles
            .iter()
            .filter(|p| p.files.iter().any(|g| g.matches(relative)))
            .collect();
        if applicable.is_empty() {
            continue;
        }
        let bytes = std::fs::read(&file)?;
        let mut key = blake3::Hasher::new();
        hash_part(&mut key, &profiles_digest.0);
        hash_part(&mut key, relative.as_bytes());
        hash_part(&mut key, blake3::hash(&bytes).as_bytes());
        let key = key.finalize();
        if let Some(usages) = memo
            .get(STEP, key.as_bytes())
            .and_then(|value| memo::decode(&value, cx))
        {
            out.extend(usages);
            continue;
        }
        let mut strings = Vec::new();
        if file
            .extension()
            .is_some_and(|s| s.eq_ignore_ascii_case("properties"))
        {
            for entry in properties::parse_checked(&bytes)
                .map_err(|e| refused(&file, format!("not a readable config: {e}")))?
            {
                strings.push(Text {
                    keys: entry.key.split('.').map(str::to_owned).collect(),
                    path: entry.key,
                    value: entry.value,
                    line: entry.line as usize,
                });
            }
        } else {
            for doc in document::parse(file.as_str(), &bytes, false)
                .map_err(|e| refused(&file, format!("not a readable config: {e}")))?
            {
                texts(&doc, &mut strings);
            }
        }
        let mut usages = Vec::new();
        scan(strings, relative, &applicable, &profiles, cx, &mut usages);
        memo.put(STEP, key.as_bytes(), &memo::encode(&usages, cx));
        out.extend(usages);
    }
    Ok(rules::unique(out))
}

fn is_config(path: &Utf8Path) -> bool {
    path.extension().is_some_and(|ext| {
        ["yml", "yaml", "json", "properties"]
            .iter()
            .any(|e| ext.eq_ignore_ascii_case(e))
    })
}

fn emit(
    rule: &Rule,
    value: &str,
    location: String,
    reason: Option<String>,
    profiles: &[Profile],
    cx: &RunCx,
    out: &mut Vec<Usage>,
) {
    if profiles
        .iter()
        .flat_map(|p| &p.placeholders)
        .any(|p| p.find(value))
    {
        if !rule.quiet {
            let mut u = rules::usage(
                rule.kind,
                &value.trim().replace(['*', '?'], "_"),
                location,
                Some("unresolvable: placeholder".into()),
            );
            u.atlas = rule.atlas;
            out.push(u);
        }
    } else if let Some(u) = rule.apply(value, location, reason, cx) {
        out.push(u);
    }
}

/// Reads YAML config bytes with the built-in profile and the same extraction as the configs provider.
pub fn yaml_usages(bytes: &[u8], cx: &RunCx) -> Result<Vec<Usage>, SourceError> {
    let profiles = vec![Profile::generic(cx)?];
    let mut strings = Vec::new();
    for doc in document::parse("input.yml", bytes, false)? {
        texts(&doc, &mut strings);
    }
    let mut out = Vec::new();
    scan(
        strings,
        "input.yml",
        &[&profiles[0]],
        &profiles,
        cx,
        &mut out,
    );
    Ok(rules::unique(out))
}

fn scan(
    strings: Vec<Text>,
    relative: &str,
    applicable: &[&Profile],
    profiles: &[Profile],
    cx: &RunCx,
    out: &mut Vec<Usage>,
) {
    for text in strings {
        let where_ = format!("{relative}:{} {}", text.line, text.path);
        let matching: Vec<_> = applicable
            .iter()
            .flat_map(|p| &p.rules)
            .filter(|r| r.matches(&text.keys))
            .collect();
        let precise = matching.iter().any(|r| !r.usage.quiet);
        for rule in matching.into_iter().filter(|r| !precise || !r.usage.quiet) {
            emit(
                &rule.usage,
                &text.value,
                where_.clone(),
                None,
                profiles,
                cx,
                out,
            );
        }
        for tag in tags::references(&text.value, cx) {
            emit(
                &tag.rule,
                &tag.value,
                format!("{where_} {}", tag.at),
                Some("MiniMessage tag".into()),
                profiles,
                cx,
                out,
            );
        }
        let stripped = text.value.trim();
        if stripped.starts_with(['{', '['])
            && let Some(json) = rules::json(stripped)
        {
            let mut refs = Vec::new();
            rules::components(&json, "", cx, &mut refs);
            for r in refs {
                emit(
                    &r.rule,
                    &r.value,
                    format!("{where_} json{}", r.at),
                    Some("JSON text component".into()),
                    profiles,
                    cx,
                    out,
                );
            }
        }
    }
}
