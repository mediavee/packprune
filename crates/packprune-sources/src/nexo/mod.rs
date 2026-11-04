mod items;
mod keys;

use camino::{Utf8Path, Utf8PathBuf};
use packprune_graph::{SourceError, Usage, UsageKind, UsageSource};
use packprune_pack::{Digest, ResourceId, RunCx};

use crate::{
    Source, digest_files,
    document::{self, Node},
    files, refused, relative,
    rules::usage,
};
use keys::{
    DEFAULT_FONT, DEFAULT_FONT_SETTING, EVENT, Form, GLYPH_ANIMATED, GLYPH_FONT, GLYPH_TEXTURE,
    GLYPHS, ITEMS, PREFER_ITEM_MODELS, RECIPES, SETTINGS, SOUND_EXTENSION, SOUND_FILES, SOUND_ID,
    SOUND_REFERENCE, SOUND_TYPE, SOUNDS, SOUNDS_ENTRIES, SOUNDS_GENERATE, YAML,
};

pub fn open(path: &Utf8Path, cx: &RunCx) -> Result<Box<dyn UsageSource>, SourceError> {
    if !path.is_dir() {
        return Err(refused(
            path,
            "not a directory, expected a Nexo plugin folder",
        ));
    }
    if !path.join(ITEMS).is_dir() && !path.join(GLYPHS).is_dir() {
        return Err(refused(
            path,
            "no items or glyphs folder, not a Nexo plugin folder",
        ));
    }
    Ok(Box::new(Source {
        inputs: [ITEMS, GLYPHS, SOUNDS, SETTINGS, RECIPES]
            .into_iter()
            .map(|n| path.join(n))
            .filter(|p| p.exists())
            .collect(),
        root: path.into(),
        cx: cx.clone(),
        collect,
        digest,
    }))
}

fn load(root: &Utf8Path, path: &Utf8Path) -> Result<Option<Node>, SourceError> {
    if !path.is_file() {
        return Ok(None);
    }
    let name = relative(path, root);
    let mut docs = document::parse(name, &std::fs::read(path)?, true)?;
    if docs.len() > 1 {
        return Err(refused(name, "expected a single YAML document"));
    }
    Ok(docs
        .pop()
        .filter(|n| n.mapping() || n.text().is_some() || !n.list().is_empty()))
}

fn selected_yaml(path: &Utf8Path) -> Result<Vec<Utf8PathBuf>, SourceError> {
    if !path.is_dir() {
        return Ok(Vec::new());
    }
    Ok(files(path, &|_| false)?
        .into_iter()
        .filter(|p| p.extension() == Some(YAML))
        .collect())
}

fn recipe_files(path: &Utf8Path) -> Result<Vec<Utf8PathBuf>, SourceError> {
    if !path.is_dir() {
        return Ok(Vec::new());
    }
    let mut folders = Vec::new();
    for entry in std::fs::read_dir(path)? {
        folders.push(
            Utf8PathBuf::from_path_buf(entry?.path())
                .map_err(|p| refused(p.display(), "not a UTF-8 path"))?,
        );
    }
    folders.sort_by(|a, b| a.as_str().cmp(b.as_str()));
    let mut out = Vec::new();
    for folder in folders {
        out.extend(selected_yaml(&folder)?);
    }
    Ok(out)
}

fn digest(path: &Utf8Path, _cx: &RunCx) -> Result<Digest, SourceError> {
    let selected = match path.file_name() {
        Some(ITEMS | GLYPHS) => selected_yaml(path)?,
        Some(RECIPES) => recipe_files(path)?,
        _ => {
            if path.is_file() {
                vec![path.to_owned()]
            } else {
                Vec::new()
            }
        }
    };
    let root = path
        .parent()
        .expect("a source input is a folder or file under its root");
    digest_files(root, selected)
}

fn yaml_files(
    root: &Utf8Path,
    selected: Vec<Utf8PathBuf>,
) -> Result<Vec<(String, Node)>, SourceError> {
    let mut out = Vec::new();
    for file in selected {
        if let Some(node) = load(root, &file)? {
            if !node.mapping() {
                return Err(refused(&file, "expected a mapping of ids at the top level"));
            }
            out.push((relative(&file, root).into(), node));
        }
    }
    Ok(out)
}

fn collect(root: &Utf8Path, cx: &RunCx) -> Result<Vec<Usage>, SourceError> {
    let settings = load(root, &root.join(SETTINGS))?;
    usages(
        settings.as_ref(),
        &yaml_files(root, selected_yaml(&root.join(ITEMS))?)?,
        &yaml_files(root, recipe_files(&root.join(RECIPES))?)?,
        &yaml_files(root, selected_yaml(&root.join(GLYPHS))?)?,
        load(root, &root.join(SOUNDS))?.as_ref(),
        cx,
    )
}

fn usages(
    settings: Option<&Node>,
    items: &[(String, Node)],
    recipes: &[(String, Node)],
    glyphs: &[(String, Node)],
    sounds: Option<&Node>,
    cx: &RunCx,
) -> Result<Vec<Usage>, SourceError> {
    let prefer = settings
        .map(|s| s.checked_get(SETTINGS, PREFER_ITEM_MODELS))
        .transpose()?
        .flatten()
        .and_then(Node::text)
        .is_none_or(|s| s.eq_ignore_ascii_case("true"));
    let font = settings
        .map(|s| s.checked_get(SETTINGS, DEFAULT_FONT_SETTING))
        .transpose()?
        .flatten()
        .and_then(Node::text)
        .unwrap_or(DEFAULT_FONT);
    let mut claims = Claims {
        cx,
        out: Vec::new(),
        declaration: None,
    };
    items::read(items, recipes, prefer, &mut claims)?;
    for (file, node) in glyphs {
        for (id, glyph) in node
            .checked_entries(file)?
            .iter()
            .filter(|(_, n)| n.mapping())
        {
            if let Some(value) = glyph.checked_get(file, GLYPH_FONT)? {
                claims.value(
                    UsageKind::Font,
                    value,
                    Form::AsWritten,
                    file,
                    format!("glyph {id} {GLYPH_FONT}"),
                );
            } else {
                claims.id(
                    UsageKind::Font,
                    font,
                    Form::AsWritten,
                    format!("{file}:{}", glyph.key_line),
                    format!("glyph {id} default font"),
                );
            }
            if let Some(gif) = glyph.checked_get(file, GLYPH_ANIMATED)? {
                claims.unresolvable(
                    UsageKind::Sprite,
                    gif.text().unwrap_or(GLYPH_ANIMATED),
                    format!("{file}:{}", gif.line),
                    format!("glyph {id} is animated: Nexo converts the gif into textures at build"),
                );
            } else if let Some(texture) = glyph.checked_get(file, GLYPH_TEXTURE)? {
                claims.value(
                    UsageKind::Sprite,
                    texture,
                    Form::WithoutPng,
                    file,
                    format!("glyph {id} {GLYPH_TEXTURE}"),
                );
            }
        }
    }
    if let Some(sounds) = sounds {
        sound_entries(sounds, &mut claims)?;
    }
    Ok(claims.out)
}

fn sound_entries(sounds: &Node, claims: &mut Claims<'_>) -> Result<(), SourceError> {
    if !sounds
        .checked_get(SOUNDS, SOUNDS_GENERATE)?
        .and_then(Node::text)
        .is_some_and(|s| s.eq_ignore_ascii_case("true"))
    {
        return Ok(());
    }
    for entry in sounds
        .checked_get(SOUNDS, SOUNDS_ENTRIES)?
        .map(Node::list)
        .unwrap_or(&[])
        .iter()
        .filter(|n| n.mapping())
    {
        let Some(id) = entry.checked_get(SOUNDS, SOUND_ID)? else {
            continue;
        };
        let name = id.text().unwrap_or("");
        claims.value(
            UsageKind::SoundEvent,
            id,
            Form::AsWritten,
            SOUNDS,
            format!("sound event {name}"),
        );
        if entry
            .checked_get(SOUNDS, SOUND_TYPE)?
            .and_then(Node::text)
            .is_some_and(|s| s.eq_ignore_ascii_case(EVENT))
        {
            if let Some(reference) = entry.checked_get(SOUNDS, SOUND_REFERENCE)? {
                claims.value(
                    UsageKind::SoundEvent,
                    reference,
                    Form::AsWritten,
                    SOUNDS,
                    format!("sound event {name} {SOUND_REFERENCE}"),
                );
            }
        } else {
            for key in SOUND_FILES {
                let files = entry
                    .checked_get(SOUNDS, key)?
                    .map(Node::list)
                    .unwrap_or(&[]);
                if files.is_empty() {
                    continue;
                }
                for file in files {
                    if let Some(text) = file.text().map(str::trim) {
                        let location = format!("{SOUNDS}:{}", file.line);
                        let reason = format!("sound event {name} {key}");
                        let written = text.strip_suffix(SOUND_EXTENSION).unwrap_or(text);
                        if let Some(id) = ResourceId::parse(&claims.cx.interner, written) {
                            claims.add(usage(
                                UsageKind::Path,
                                &format!(
                                    "assets/{}/sounds/{}{}",
                                    claims.cx.interner.resolve(id.ns),
                                    claims.cx.interner.resolve(id.path),
                                    SOUND_EXTENSION
                                ),
                                location,
                                Some(reason),
                            ));
                        } else {
                            claims.unresolvable(
                                UsageKind::Path,
                                written,
                                location,
                                format!(
                                    "{reason} is not a resource id: a placeholder or an invalid key"
                                ),
                            );
                        }
                    }
                }
                break;
            }
        }
    }
    Ok(())
}

struct Claims<'a> {
    cx: &'a RunCx,
    out: Vec<Usage>,
    declaration: Option<String>,
}
impl Claims<'_> {
    fn add(&mut self, mut u: Usage) {
        u.declares = self.declaration.clone();
        self.out.push(u);
    }
    fn unresolvable(&mut self, kind: UsageKind, written: &str, location: String, reason: String) {
        self.add(usage(
            kind,
            written,
            location,
            Some(format!("unresolvable: {reason}")),
        ));
    }
    fn value(&mut self, kind: UsageKind, node: &Node, form: Form, file: &str, reason: String) {
        if let Some(text) = node.text().map(str::trim).filter(|s| !s.is_empty()) {
            self.id(kind, text, form, format!("{file}:{}", node.line), reason);
        }
    }
    fn id(&mut self, kind: UsageKind, written: &str, form: Form, location: String, reason: String) {
        let text = match form {
            Form::AsWritten => written,
            Form::DropExtension => written.rsplit_once('.').map(|(s, _)| s).unwrap_or(written),
            Form::WithoutPng => written.strip_suffix(".png").unwrap_or(written),
        };
        if let Some(id) = ResourceId::parse(&self.cx.interner, text) {
            self.add(usage(
                kind,
                &id.display(&self.cx.interner).to_string(),
                location,
                Some(reason),
            ));
        } else {
            self.unresolvable(
                kind,
                written,
                location,
                format!("{reason} is not a resource id: a placeholder or an invalid key"),
            );
        }
    }
}

/// Collects Nexo usages from one in-memory document for parser fuzzing.
pub fn yaml_usages(bytes: &[u8], cx: &RunCx) -> Result<Vec<Usage>, SourceError> {
    let docs = document::parse("input.yml", bytes, true)?;
    if docs.len() > 1 {
        return Err(refused("input.yml", "expected a single YAML document"));
    }
    let Some(node) = docs.into_iter().next() else {
        return Ok(Vec::new());
    };
    let files = [("input.yml".into(), node.clone())];
    usages(Some(&node), &files, &files, &files, Some(&node), cx)
}
