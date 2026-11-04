use std::collections::{BTreeMap, BTreeSet};

use packprune_pack::json;
use serde::Serialize;

use super::Exclusions;
use super::read::{Failure, Json, Key, Res};
use super::stack::Stack;

/// Bytes of a client glyph page: 256 x 256 RGBA.
pub const PAGE_BYTES: u64 = 4 * 256 * 256;
const PAGE: i64 = 256;

/// The cost of one font id of the pack: its bitmap sheets decoded, the staging copy of each sheet a glyph was drawn
/// from, and the glyph pages the client stitches.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct FontCost {
    pub font: String,
    pub providers: usize,
    pub glyphs: usize,
    pub decoded: u64,
    pub staging: u64,
    /// `decoded + staging`.
    pub once_drawn: u64,
    pub pages: usize,
    /// `once_drawn + pages * PAGE_BYTES`.
    pub total: u64,
    /// Glyphs whose cell is over 256 texels, which no glyph page can hold.
    pub oversized_glyphs: usize,
    pub provider_types: BTreeMap<String, usize>,
}

/// A sheet loaded by several bitmap providers, which the client decodes once per provider.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Duplicate {
    pub texture: String,
    pub providers: usize,
    pub decoded: u64,
    pub redundant_decoded: u64,
    pub uses: Vec<String>,
}

pub(super) struct FontAudit {
    pub fonts: Vec<FontCost>,
    pub duplicates: Vec<Duplicate>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Bitmap,
    Space,
    Reference,
    Other,
}

struct Provider {
    font: Key,
    kind: Kind,
    type_name: String,
    enabled: bool,
    /// `file > path` of the declaration, for a report.
    origin: String,
    texture: String,
    sheet: (i64, i64),
    cell: (i64, i64),
    chars: Vec<char>,
    advances: Vec<char>,
    reference: Option<Key>,
}

impl Provider {
    fn codepoints(&self) -> Vec<char> {
        if self.kind == Kind::Space {
            return self.advances.clone();
        }
        let mut seen = BTreeSet::new();
        self.chars
            .iter()
            .copied()
            .filter(|&c| c != '\0' && seen.insert(c))
            .collect()
    }

    fn decoded(&self) -> u64 {
        4 * self.sheet.0 as u64 * self.sheet.1 as u64
    }
}

/// The font sets of the pack, loaded as the client's `FontManager` does with the defaults (Force Unicode and
/// Japanese variants off): the pack's own providers, `reference` providers spliced in place.
struct Loader<'a, 'e> {
    stack: &'a Stack<'a>,
    exclusions: &'e mut Exclusions,
    arena: Vec<Provider>,
    declared: BTreeMap<Key, Vec<usize>>,
    sheets: BTreeMap<String, Option<(i64, i64)>>,
}

impl Loader<'_, '_> {
    /// Providers declared by the pack's file of `font`, disabled ones included.
    fn declared(&mut self, font: &Key) -> Res<Vec<usize>> {
        if let Some(found) = self.declared.get(font) {
            return Ok(found.clone());
        }
        let path = font_path(font);
        let mut indices = Vec::new();
        if let Some((file, entry)) = self.stack.pack_file(&path) {
            // The client reads fonts with a lenient Gson.
            let root = json::parse_lenient(&entry.blob)
                .map_err(|e| Failure(format!("{file} > {path}: {e}")))?;
            let root = Json::new(file, &path, &root);
            for (index, input) in root.field("providers")?.array()?.iter().enumerate() {
                if let Some(provider) = self.provider(font, input, file, &path, index)? {
                    self.arena.push(provider);
                    indices.push(self.arena.len() - 1);
                }
            }
        }
        self.declared.insert(font.clone(), indices.clone());
        Ok(indices)
    }

    /// Active providers with references spliced in place; an external reference stays as it is.
    fn resolve(&mut self, font: &Key, chain: &mut Vec<Key>) -> Res<Vec<usize>> {
        chain.push(font.clone());
        let mut resolved = Vec::new();
        for index in self.declared(font)? {
            let provider = &self.arena[index];
            if !provider.enabled {
                continue;
            }
            let Some(reference) = provider.reference.clone() else {
                resolved.push(index);
                continue;
            };
            if chain.contains(&reference) {
                return Err(Failure(format!(
                    "{}.id: font reference cycle at {reference}",
                    provider.origin
                )));
            }
            if self.stack.pack_file(&font_path(&reference)).is_none() {
                resolved.push(index);
            } else {
                resolved.extend(self.resolve(&reference, chain)?);
            }
        }
        chain.pop();
        Ok(resolved)
    }

    /// `None` for a bitmap provider whose sheet the pack lacks or cannot decode: it is listed, not modelled.
    fn provider(
        &mut self,
        font: &Key,
        input: &Json<'_>,
        file: &str,
        path: &str,
        index: usize,
    ) -> Res<Option<Provider>> {
        let type_name = input.field("type")?.string()?;
        let mut enabled = true;
        let filter = input.field("filter")?;
        if filter.present() {
            for (name, option) in filter.fields()? {
                if name != "uniform" && name != "jp" {
                    return Err(option.error(&format!("unknown font option {name}")));
                }
                if option.bool()? {
                    enabled = false;
                }
            }
        }
        let mut provider = Provider {
            font: font.clone(),
            kind: Kind::Other,
            type_name: type_name.to_owned(),
            enabled,
            origin: format!("{file} > {path}.providers[{index}]"),
            texture: String::new(),
            sheet: (0, 0),
            cell: (0, 0),
            chars: Vec::new(),
            advances: Vec::new(),
            reference: None,
        };
        match type_name {
            "bitmap" => {
                let texture = input.field("file")?.key()?;
                provider.texture =
                    format!("assets/{}/textures/{}", texture.namespace, texture.value);
                let height = input.field("height")?;
                let height = if height.present() {
                    height.integer()?
                } else {
                    8
                };
                let ascent = input.field("ascent")?;
                if ascent.integer()? > height {
                    return Err(ascent.error(&format!("ascent exceeds height {height}")));
                }
                let rows = input.field("chars")?;
                let rows = rows.array()?;
                if rows.is_empty() {
                    return Err(input.field("chars")?.error("empty chars"));
                }
                let mut columns = 0;
                for row in &rows {
                    let codepoints: Vec<char> = row.string()?.chars().collect();
                    if columns == 0 {
                        columns = codepoints.len();
                        if columns == 0 {
                            return Err(row.error("empty chars row"));
                        }
                    }
                    if codepoints.len() != columns {
                        return Err(row.error("unequal chars rows"));
                    }
                    provider.chars.extend(codepoints);
                }
                let Some(sheet) = self.sheet(&provider.texture, &provider.origin) else {
                    return Ok(None);
                };
                provider.sheet = sheet;
                provider.cell = (sheet.0 / columns as i64, sheet.1 / rows.len() as i64);
                if provider.cell.0 == 0 || provider.cell.1 == 0 {
                    return Err(input.field("chars")?.error("empty glyph cell"));
                }
                provider.kind = Kind::Bitmap;
            }
            "space" => {
                for (name, advance) in input.field("advances")?.fields()? {
                    let mut codepoints = name.chars();
                    let (Some(codepoint), None) = (codepoints.next(), codepoints.next()) else {
                        return Err(advance.error("expected one codepoint as the advance key"));
                    };
                    advance.decimal()?;
                    provider.advances.push(codepoint);
                }
                provider.kind = Kind::Space;
            }
            "reference" => {
                provider.reference = Some(input.field("id")?.key()?);
                provider.kind = Kind::Reference;
            }
            "unihex" => {
                input.field("hex_file")?.key()?;
                let overrides = input.field("size_overrides")?;
                if overrides.present() {
                    for range in overrides.array()? {
                        let from = codepoint(&range.field("from")?)?;
                        let to = codepoint(&range.field("to")?)?;
                        if from >= to {
                            return Err(range.error("from must be below to"));
                        }
                        range.field("left")?.integer()?;
                        range.field("right")?.integer()?;
                    }
                }
            }
            "ttf" => {
                input.field("file")?.key()?;
                for name in ["size", "oversample"] {
                    let number = input.field(name)?;
                    if number.present() {
                        number.decimal()?;
                    }
                }
                let shift = input.field("shift")?;
                if shift.present() {
                    let vector = shift.array()?;
                    if vector.len() != 2 {
                        return Err(shift.error("expected two shift components"));
                    }
                    for component in vector {
                        if !(-512.0..=512.0).contains(&component.decimal()?) {
                            return Err(component.error("shift must be between -512 and 512"));
                        }
                    }
                }
                let skip = input.field("skip")?;
                if skip.present() {
                    if skip.is_array() {
                        for entry in skip.array()? {
                            entry.string()?;
                        }
                    } else {
                        skip.string()?;
                    }
                }
            }
            _ => {
                return Err(input
                    .field("type")?
                    .error(&format!("unknown font provider type {type_name}")));
            }
        }
        Ok(Some(provider))
    }

    /// The size of a sheet of the pack, decoded to certify it; `None` and a listed exclusion when it cannot be.
    fn sheet(&mut self, texture: &str, origin: &str) -> Option<(i64, i64)> {
        if let Some(found) = self.sheets.get(texture) {
            return *found;
        }
        let decoded = match self.stack.pack_file(texture) {
            None => Err("missing font texture".to_owned()),
            Some((_, entry)) => packprune_codec::png::decode(&entry.blob)
                .map(|image| (i64::from(image.width), i64::from(image.height)))
                .map_err(|e| format!("unreadable PNG: {e}")),
        };
        if let Err(why) = &decoded {
            self.exclusions.add(format!("{origin}: {why} {texture}"));
        }
        let size = decoded.ok();
        self.sheets.insert(texture.to_owned(), size);
        size
    }
}

fn codepoint(input: &Json<'_>) -> Res<char> {
    let mut chars = input.string()?.chars();
    match (chars.next(), chars.next()) {
        (Some(c), None) => Ok(c),
        _ => Err(input.error("expected one codepoint")),
    }
}

fn font_path(font: &Key) -> String {
    format!("assets/{}/font/{}.json", font.namespace, font.value)
}

/// What one font accumulates: the sheets decoded, the providers a glyph was uploaded from, the glyph pages.
struct Account {
    atlas: GlyphAtlas,
    drawn: BTreeSet<usize>,
}

/// Every font of the pack, each bitmap glyph reachable drawn once in provider declaration then `chars` row order:
/// a simulation of the client's allocation, not a measurement. `staging_copy` is whether the client keeps a staging
/// copy of an uploaded sheet (26.2 and later).
pub(super) fn audit(
    stack: &Stack<'_>,
    staging_copy: bool,
    exclusions: &mut Exclusions,
) -> Res<FontAudit> {
    let mut loader = Loader {
        stack,
        exclusions,
        arena: Vec::new(),
        declared: BTreeMap::new(),
        sheets: BTreeMap::new(),
    };
    let mut fonts: BTreeMap<String, Key> = BTreeMap::new();
    for path in stack.pack_paths() {
        let mut parts = path.splitn(4, '/');
        if let (Some("assets"), Some(namespace), Some("font"), Some(rest)) =
            (parts.next(), parts.next(), parts.next(), parts.next())
            && let Some(name) = rest.strip_suffix(".json")
            && !name.is_empty()
        {
            let key = Key::new(namespace, name);
            if !Key::namespace_ok(namespace) || !Key::value_ok(name) {
                return Err(Failure(format!("{path}: invalid font id {key}")));
            }
            fonts.insert(key.to_string(), key);
        }
    }
    for key in fonts.values() {
        for index in loader.declared(key)? {
            let provider = &loader.arena[index];
            if matches!(provider.type_name.as_str(), "ttf" | "unihex") {
                loader.exclusions.add(format!(
                    "{}: unsupported provider {}",
                    provider.origin, provider.type_name
                ));
            }
        }
    }
    let mut accounts: BTreeMap<String, Account> = fonts
        .keys()
        .map(|name| {
            let account = Account {
                atlas: GlyphAtlas::new(),
                drawn: BTreeSet::new(),
            };
            (name.clone(), account)
        })
        .collect();
    let mut glyph_counts = BTreeMap::new();
    for (name, key) in &fonts {
        let mut seen = BTreeSet::new();
        let (mut glyphs, mut oversized) = (0, 0);
        for index in loader.resolve(key, &mut Vec::new())? {
            let provider = &loader.arena[index];
            if let Some(reference) = &provider.reference {
                loader.exclusions.add(format!(
                    "{}.id: external font reference {reference} (not in this pack)",
                    provider.origin
                ));
            }
            let mut uploaded = false;
            for codepoint in provider.codepoints() {
                if !seen.insert(codepoint) || provider.kind != Kind::Bitmap {
                    continue;
                }
                glyphs += 1;
                let (width, height) = provider.cell;
                uploaded |= accounts
                    .get_mut(name)
                    .is_some_and(|account| account.atlas.add(width, height));
                oversized += usize::from(width > PAGE || height > PAGE);
            }
            if uploaded && let Some(account) = accounts.get_mut(&provider.font.to_string()) {
                account.drawn.insert(index);
            }
        }
        glyph_counts.insert(name.clone(), (glyphs, oversized));
    }
    let mut rows = Vec::new();
    let mut loads: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    for (name, key) in &fonts {
        let declared = loader.declared(key)?;
        let account = &accounts[name];
        let mut provider_types = BTreeMap::new();
        let mut decoded = 0;
        for &index in &declared {
            let provider = &loader.arena[index];
            *provider_types
                .entry(provider.type_name.clone())
                .or_insert(0) += 1;
            if provider.kind == Kind::Bitmap {
                decoded += provider.decoded();
                loads
                    .entry(provider.texture.clone())
                    .or_default()
                    .push(index);
            }
        }
        let staging: u64 = if staging_copy {
            account
                .drawn
                .iter()
                .map(|&i| loader.arena[i].decoded())
                .sum()
        } else {
            0
        };
        let (glyphs, oversized_glyphs) = glyph_counts[name];
        let pages = account.atlas.pages.len();
        rows.push(FontCost {
            font: name.clone(),
            providers: provider_types.get("bitmap").copied().unwrap_or(0),
            glyphs,
            decoded,
            staging,
            once_drawn: decoded + staging,
            pages,
            total: decoded + staging + pages as u64 * PAGE_BYTES,
            oversized_glyphs,
            provider_types,
        });
    }
    let duplicates = loads
        .into_iter()
        .filter(|(_, providers)| providers.len() > 1)
        .map(|(texture, providers)| {
            let decoded = loader.arena[providers[0]].decoded();
            Duplicate {
                texture,
                providers: providers.len(),
                decoded,
                redundant_decoded: (providers.len() as u64 - 1) * decoded,
                uses: providers
                    .iter()
                    .map(|&i| loader.arena[i].origin.clone())
                    .collect(),
            }
        })
        .collect();
    Ok(FontAudit {
        fonts: rows,
        duplicates,
    })
}

/// The client's glyph atlas for one font id (`GlyphStitcher`, `FontTexture`): 256 x 256 RGBA pages starting with the
/// two built-in glyphs; a glyph takes its cell plus a 1 px gutter, inserted first-fit by a guillotine split, and a new
/// page opens when no page has room.
struct GlyphAtlas {
    pages: Vec<Node>,
}

impl GlyphAtlas {
    fn new() -> Self {
        let mut atlas = Self { pages: Vec::new() };
        // `FontSet.resetTextures` bakes the missing and the white glyph before any other.
        atlas.add(5, 8);
        atlas.add(5, 8);
        atlas
    }

    /// Stitches a cell in first-draw order; a failed insertion still retains its new page.
    fn add(&mut self, width: i64, height: i64) -> bool {
        if self.pages.iter_mut().any(|page| page.insert(width, height)) {
            return true;
        }
        let mut page = Node::new(PAGE, PAGE);
        let inserted = page.insert(width, height);
        self.pages.push(page);
        inserted
    }
}

struct Node {
    width: i64,
    height: i64,
    children: Option<Box<(Node, Node)>>,
    occupied: bool,
}

impl Node {
    fn new(width: i64, height: i64) -> Self {
        Self {
            width,
            height,
            children: None,
            occupied: false,
        }
    }

    fn insert(&mut self, w: i64, h: i64) -> bool {
        if let Some(children) = &mut self.children {
            return children.0.insert(w, h) || children.1.insert(w, h);
        }
        if self.occupied || w > self.width || h > self.height {
            return false;
        }
        if w == self.width && h == self.height {
            self.occupied = true;
            return true;
        }
        let (left, right) = if self.width - w > self.height - h {
            (
                Node::new(w, self.height),
                Node::new(self.width - w - 1, self.height),
            )
        } else {
            (
                Node::new(self.width, h),
                Node::new(self.width, self.height - h - 1),
            )
        };
        self.children.insert(Box::new((left, right))).0.insert(w, h)
    }
}
