use std::collections::{BTreeMap, BTreeSet};

use packprune_pack::{VanillaIndex, json};
use serde::Serialize;

use super::Exclusions;
use super::read::{Failure, Json, Key, Res};
use super::stack::{CLIENT_FILE, Found, Matcher, Stack};
use super::stitch::{Item, mip_chain_bytes, stitch};

/// The client's `maxTextureSize` for an atlas.
pub const MAX_TEXTURE_SIZE: i64 = 16384;
/// Mip levels of the blocks atlas at the client's default options; the other atlases have none.
const MIPMAP_LEVELS: u32 = 4;
const LARGEST: usize = 10;
const MIPMAP_STRATEGIES: [&str; 5] = ["auto", "mean", "cutout", "strict_cutout", "dark_cutout"];

/// A sprite of an atlas, for the largest-sprites lists.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct SpriteRow {
    pub id: String,
    pub width: i64,
    pub height: i64,
    pub image_width: i64,
    pub image_height: i64,
    /// Every mip level of the whole image.
    pub decoded: u64,
    pub file: String,
    /// `pack` or `client`.
    pub origin: &'static str,
}

/// One texture atlas as the client stitches it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct AtlasCost {
    pub id: String,
    pub mip_level: u32,
    /// Sprites stitched, the missing sprite included.
    pub sprites: usize,
    pub pack_sprites: usize,
    pub width: i64,
    pub height: i64,
    /// The texture's RGBA bytes over all its mip levels.
    pub gpu: u64,
    pub decoded: u64,
    /// By frame area, what the stitched texture holds.
    pub largest: Vec<SpriteRow>,
    /// By decoded bytes, whole animation strips included.
    pub largest_decoded: Vec<SpriteRow>,
}

struct Sprite {
    name: Key,
    width: i64,
    height: i64,
    image_width: i64,
    image_height: i64,
    file: String,
    pack: bool,
}

impl Sprite {
    fn decoded(&self, mip_level: u32) -> u64 {
        mip_chain_bytes(self.image_width, self.image_height, mip_level)
    }

    fn row(&self, mip_level: u32) -> SpriteRow {
        SpriteRow {
            id: self.name.to_string(),
            width: self.width,
            height: self.height,
            image_width: self.image_width,
            image_height: self.image_height,
            decoded: self.decoded(mip_level),
            file: self.file.clone(),
            origin: if self.pack { "pack" } else { "client" },
        }
    }
}

#[derive(Clone)]
struct Texture {
    width: i64,
    height: i64,
    from_client: bool,
    file: String,
}

/// How a pending sprite is produced, once every source has run.
enum Loader {
    Sprite {
        path: String,
    },
    Region {
        path: String,
        location: String,
        x: f64,
        y: f64,
        width: f64,
        height: f64,
        divisor: (f64, f64),
    },
    Permutation {
        base: String,
        keys: String,
        values: String,
        location: String,
    },
}

/// The texture atlases a client stitches from the pack stacked over its jar: the sprite sources of each atlas
/// definition, then the size the client's stitcher gives the texture. A pack PNG is decoded to certify it and only
/// its size is kept.
pub(super) struct Atlases<'a, 'e> {
    stack: &'a Stack<'a>,
    vanilla: &'a VanillaIndex,
    palette_directory: &'static str,
    paths: BTreeSet<&'a str>,
    gui_metadata: bool,
    textures: BTreeMap<String, Option<Texture>>,
    exclusions: &'e mut Exclusions,
}

impl<'a, 'e> Atlases<'a, 'e> {
    /// `palettes_in_directory`: whether the client reads palettes from `textures/palettes/` (26.3) rather than as
    /// plain textures.
    pub(super) fn new(
        stack: &'a Stack<'a>,
        vanilla: &'a VanillaIndex,
        palettes_in_directory: bool,
        exclusions: &'e mut Exclusions,
    ) -> Self {
        Self {
            stack,
            vanilla,
            palette_directory: if palettes_in_directory {
                "textures/palettes/"
            } else {
                "textures/"
            },
            paths: stack.png_paths(),
            gui_metadata: false,
            textures: BTreeMap::new(),
            exclusions,
        }
    }

    pub(super) fn read(&mut self) -> Res<Vec<AtlasCost>> {
        // The client stitches the atlases its own jar defines; a pack's extra definition is never read.
        let definitions: BTreeMap<&str, &str> = self
            .vanilla
            .atlases
            .keys()
            .filter_map(|path| {
                let name = path
                    .strip_prefix("assets/minecraft/atlases/")?
                    .strip_suffix(".json")?;
                (!name.contains('/')).then_some((name, path.as_str()))
            })
            .collect();
        let mut atlases = Vec::new();
        for (name, path) in definitions {
            let id = format!("minecraft:{name}");
            self.gui_metadata = name == "gui";
            let sprites = self.sprites(path)?;
            let mipmaps = if name == "blocks" { MIPMAP_LEVELS } else { 0 };
            let items: Vec<_> = sprites
                .iter()
                .map(|sprite| Item {
                    width: sprite.width,
                    height: sprite.height,
                    name: &sprite.name,
                })
                .collect();
            let Some(stitched) = stitch(&items, mipmaps, MAX_TEXTURE_SIZE) else {
                self.exclusions.add(format!(
                    "{id}: the sprites fit no {MAX_TEXTURE_SIZE} px texture, so the client fails to stitch it"
                ));
                continue;
            };
            let mip = stitched.mip_level;
            atlases.push(AtlasCost {
                sprites: sprites.len(),
                pack_sprites: sprites.iter().filter(|sprite| sprite.pack).count(),
                width: stitched.width,
                height: stitched.height,
                gpu: stitched.gpu_bytes(),
                decoded: sprites.iter().map(|sprite| sprite.decoded(mip)).sum(),
                largest: largest(&sprites, |s| (s.width * s.height) as u64, mip),
                largest_decoded: largest(&sprites, |s| s.decoded(mip), mip),
                mip_level: mip,
                id,
            });
        }
        Ok(atlases)
    }

    /// The client's `SpriteSourceList`: every layer's sources from the jar up, then the missing sprite and what
    /// survives.
    fn sprites(&mut self, definition: &str) -> Res<Vec<Sprite>> {
        let mut pending: BTreeMap<Key, Loader> = BTreeMap::new();
        let mut layers = self.stack.definitions(definition);
        layers.reverse();
        for (file, bytes) in layers {
            let root =
                json::parse(bytes).map_err(|e| Failure(format!("{file} > {definition}: {e}")))?;
            let root = Json::new(file, definition, &root);
            for source in root.object()?.field("sources")?.array()? {
                self.run(&source, &mut pending)?;
            }
        }
        let mut sprites = vec![Sprite {
            name: Key::new("minecraft", "missingno"),
            width: 16,
            height: 16,
            image_width: 16,
            image_height: 16,
            file: "<generated>".into(),
            pack: false,
        }];
        for (name, loader) in pending {
            sprites.extend(self.load(loader, &name));
        }
        Ok(sprites)
    }

    fn run(&mut self, source: &Json<'_>, pending: &mut BTreeMap<Key, Loader>) -> Res<()> {
        let kind = source.field("type")?;
        let kind_key = kind.key()?;
        if kind_key.namespace != "minecraft" {
            let why = kind.error(&format!("unsupported sprite source {kind_key}"));
            self.exclusions.add(why.0);
            return Ok(());
        }
        match kind_key.value.as_str() {
            "directory" => self.directory(source, pending),
            "single" => self.single(source, pending),
            "filter" => self.filter(source, pending),
            "unstitch" => self.unstitch(source, pending),
            "paletted_permutations" => self.permutations(source, pending),
            _ => {
                let why = kind.error(&format!("unsupported sprite source {kind_key}"));
                self.exclusions.add(why.0);
                Ok(())
            }
        }
    }

    fn directory(&mut self, source: &Json<'_>, pending: &mut BTreeMap<Key, Loader>) -> Res<()> {
        let root = format!("textures/{}/", source.field("source")?.string()?);
        let prefix = source.field("prefix")?.string()?;
        for &path in &self.paths {
            let mut parts = path.splitn(3, '/');
            let (Some("assets"), Some(namespace), Some(rest)) =
                (parts.next(), parts.next(), parts.next())
            else {
                continue;
            };
            let Some(file) = rest
                .strip_prefix(&root)
                .and_then(|f| f.strip_suffix(".png"))
            else {
                continue;
            };
            let value = format!("{prefix}{file}");
            // The client skips a file whose name is not a valid resource id.
            if !namespace.is_empty() && Key::namespace_ok(namespace) && Key::value_ok(&value) {
                pending.insert(
                    Key::new(namespace, &value),
                    Loader::Sprite {
                        path: path.to_owned(),
                    },
                );
            }
        }
        Ok(())
    }

    fn single(&mut self, source: &Json<'_>, pending: &mut BTreeMap<Key, Loader>) -> Res<()> {
        let resource = source.field("resource")?.key()?;
        let sprite = source.field("sprite")?;
        let name = if sprite.present() {
            sprite.key()?
        } else {
            resource.clone()
        };
        let path = texture_path(&resource);
        if self.stack.has_png(&path) {
            pending.insert(name, Loader::Sprite { path });
        }
        Ok(())
    }

    fn filter(&mut self, source: &Json<'_>, pending: &mut BTreeMap<Key, Loader>) -> Res<()> {
        let pattern = source.field("pattern")?;
        let pattern = pattern.object()?;
        let text = |name: &str| -> Res<Option<String>> {
            let field = pattern.field(name)?;
            field
                .present()
                .then(|| field.string().map(str::to_owned))
                .transpose()
        };
        let matcher = Matcher::new(
            text("namespace")?.as_deref(),
            text("path")?.as_deref(),
            &pattern.location(),
            self.exclusions,
        );
        pending.retain(|name, _| !matcher.matches(&name.namespace, &name.value));
        Ok(())
    }

    fn unstitch(&mut self, source: &Json<'_>, pending: &mut BTreeMap<Key, Loader>) -> Res<()> {
        let resource = source.field("resource")?.key()?;
        let divisor = |name: &str| -> Res<f64> {
            let field = source.field(name)?;
            if field.present() {
                field.decimal()
            } else {
                Ok(1.0)
            }
        };
        let divisor = (divisor("divisor_x")?, divisor("divisor_y")?);
        let regions = source.field("regions")?;
        let regions = regions.array()?;
        if regions.is_empty() {
            return Err(source
                .field("regions")?
                .error("expected at least one region"));
        }
        let path = texture_path(&resource);
        for region in regions {
            let name = region.field("sprite")?.key()?;
            let x = region.field("x")?.decimal()?;
            let y = region.field("y")?.decimal()?;
            let width = region.field("width")?.decimal()?;
            let height = region.field("height")?.decimal()?;
            if self.stack.has_png(&path) {
                pending.insert(
                    name,
                    Loader::Region {
                        path: path.clone(),
                        location: region.location(),
                        x,
                        y,
                        width,
                        height,
                        divisor,
                    },
                );
            }
        }
        Ok(())
    }

    fn permutations(&mut self, source: &Json<'_>, pending: &mut BTreeMap<Key, Loader>) -> Res<()> {
        let bases = source
            .field("textures")?
            .array()?
            .iter()
            .map(Json::key)
            .collect::<Res<Vec<_>>>()?;
        let keys = self.palette_path(&source.field("palette_key")?.key()?);
        let mut palettes = Vec::new();
        for (suffix, palette) in source.field("permutations")?.fields()? {
            palettes.push((suffix, self.palette_path(&palette.key()?)));
        }
        let separator = source.field("separator")?;
        let separator = if separator.present() {
            separator.string()?
        } else {
            "_"
        };
        for base in bases {
            let base_path = texture_path(&base);
            if !self.stack.has_png(&base_path) {
                continue;
            }
            for (suffix, values) in &palettes {
                let value = format!("{}{separator}{suffix}", base.value);
                if !Key::value_ok(&value) {
                    let why = source.error(&format!(
                        "permutation id {}:{value} is not a valid resource id",
                        base.namespace
                    ));
                    self.exclusions.add(why.0);
                    continue;
                }
                pending.insert(
                    Key::new(&base.namespace, &value),
                    Loader::Permutation {
                        base: base_path.clone(),
                        keys: keys.clone(),
                        values: values.clone(),
                        location: source.location(),
                    },
                );
            }
        }
        Ok(())
    }

    fn load(&mut self, loader: Loader, name: &Key) -> Option<Sprite> {
        match loader {
            Loader::Sprite { path } => self.sprite(name, &path),
            Loader::Region {
                path,
                location,
                x,
                y,
                width,
                height,
                divisor,
            } => {
                let image = self.texture(&path)?;
                let x_scale = image.width as f64 / divisor.0;
                let y_scale = image.height as f64 / divisor.1;
                let left = (x * x_scale).floor() as i64;
                let top = (y * y_scale).floor() as i64;
                let (w, h) = (
                    (width * x_scale).floor() as i64,
                    (height * y_scale).floor() as i64,
                );
                if w <= 0
                    || h <= 0
                    || left < 0
                    || top < 0
                    || left + w > image.width
                    || top + h > image.height
                {
                    self.exclusions.add(format!(
                        "{location}: region {w}x{h} at {left},{top} is outside the {}x{} image",
                        image.width, image.height
                    ));
                    return None;
                }
                Some(Sprite {
                    name: name.clone(),
                    width: w,
                    height: h,
                    image_width: w,
                    image_height: h,
                    file: image.file,
                    pack: !image.from_client,
                })
            }
            Loader::Permutation {
                base,
                keys,
                values,
                location,
            } => {
                let image = self.texture(&base);
                let keys = self.texture(&keys);
                let values = self.texture(&values);
                let sized = |t: &Texture| t.width * t.height;
                let (Some(image), Some(keys), Some(values)) = (image, keys, values) else {
                    self.exclusions.add(palette_missing(&location, name));
                    return None;
                };
                if sized(&keys) != sized(&values) {
                    self.exclusions.add(palette_missing(&location, name));
                    return None;
                }
                Some(Sprite {
                    name: name.clone(),
                    width: image.width,
                    height: image.height,
                    image_width: image.width,
                    image_height: image.height,
                    file: image.file,
                    pack: !image.from_client,
                })
            }
        }
    }

    /// The client's `SpriteResourceLoader`: an animated texture occupies one frame of its strip.
    fn sprite(&mut self, name: &Key, path: &str) -> Option<Sprite> {
        let texture = self.texture(path)?;
        let (mut width, mut height) = (texture.width, texture.height);
        // Metadata comes from the topmost pack at or above the one providing the texture, never from a pack below it.
        let meta_path = format!("{path}.mcmeta");
        let meta = match self.stack.find(&meta_path) {
            Some(Found::Pack { file, entry }) => Some((file, &*entry.blob)),
            Some(Found::Client) if texture.from_client => self
                .stack
                .client_mcmeta(&meta_path)
                .map(|bytes| (CLIENT_FILE, bytes)),
            _ => None,
        };
        if let Some((file, bytes)) = meta {
            match self.frame_of(file, &meta_path, bytes, &texture) {
                Ok(Some(frame)) => (width, height) = frame,
                Ok(None) => {}
                Err(why) => {
                    self.exclusions.add(why.0);
                    return None;
                }
            }
        }
        Some(Sprite {
            name: name.clone(),
            width,
            height,
            image_width: texture.width,
            image_height: texture.height,
            file: texture.file,
            pack: !texture.from_client,
        })
    }

    /// The validated `.mcmeta` sections of a sprite, and its frame size when it is animated.
    fn frame_of(
        &self,
        file: &str,
        path: &str,
        bytes: &[u8],
        texture: &Texture,
    ) -> Res<Option<(i64, i64)>> {
        let value = json::parse(bytes).map_err(|e| Failure(format!("{file} > {path}: {e}")))?;
        let sections = Json::new(file, path, &value);
        let sections = sections.object()?;
        texture_section(&sections.field("texture")?)?;
        if self.gui_metadata && sections.field("gui")?.present() {
            gui_section(&sections.field("gui")?)?;
        }
        let animation = sections.field("animation")?;
        if !animation.present() {
            return Ok(None);
        }
        let (frame_width, frame_height) = frame(&animation, texture.width, texture.height)?;
        if texture.width % frame_width != 0 || texture.height % frame_height != 0 {
            return Err(animation.error(&format!(
                "image {}x{} is not a multiple of its {frame_width}x{frame_height} frame",
                texture.width, texture.height
            )));
        }
        Ok(Some((frame_width, frame_height)))
    }

    /// A texture of the stack and its size; an unreadable PNG is listed once and not stitched.
    fn texture(&mut self, path: &str) -> Option<Texture> {
        if let Some(found) = self.textures.get(path) {
            return found.clone();
        }
        let texture = match self.stack.find(path) {
            Some(Found::Pack { file, entry }) => match packprune_codec::png::decode(&entry.blob) {
                Ok(image) => Some(Texture {
                    width: i64::from(image.width),
                    height: i64::from(image.height),
                    from_client: false,
                    file: file.to_owned(),
                }),
                Err(e) => {
                    self.exclusions
                        .add(format!("{file} > {path}: unreadable PNG: {e}"));
                    None
                }
            },
            Some(Found::Client) => self.stack.client_texture(path).map(|size| Texture {
                width: i64::from(size.width),
                height: i64::from(size.height),
                from_client: true,
                file: path.to_owned(),
            }),
            None => None,
        };
        self.textures.insert(path.to_owned(), texture.clone());
        texture
    }

    fn palette_path(&self, key: &Key) -> String {
        format!(
            "assets/{}/{}{}.png",
            key.namespace, self.palette_directory, key.value
        )
    }
}

fn texture_path(key: &Key) -> String {
    format!("assets/{}/textures/{}.png", key.namespace, key.value)
}

fn largest(sprites: &[Sprite], size: impl Fn(&Sprite) -> u64, mip_level: u32) -> Vec<SpriteRow> {
    let mut sorted: Vec<&Sprite> = sprites.iter().collect();
    sorted.sort_by(|a, b| {
        size(b)
            .cmp(&size(a))
            .then_with(|| a.name.to_string().cmp(&b.name.to_string()))
    });
    sorted
        .into_iter()
        .take(LARGEST)
        .map(|sprite| sprite.row(mip_level))
        .collect()
}

fn palette_missing(location: &str, name: &Key) -> String {
    format!(
        "{location}: palette of {name} is missing, unreadable or not the size of the palette key"
    )
}

fn integer(field: &Json<'_>, minimum: i32) -> Res<i32> {
    let value = field.integer()?;
    if value < minimum {
        let why = if minimum == 0 {
            "expected a non-negative integer"
        } else {
            "expected a positive integer"
        };
        return Err(field.error(why));
    }
    Ok(value)
}

/// `AnimationMetadataSection`: a validated section and its frame size.
fn frame(animation: &Json<'_>, image_width: i64, image_height: i64) -> Res<(i64, i64)> {
    let frames = animation.field("frames")?;
    if frames.present() {
        for entry in frames.array()? {
            if entry.is_object() {
                integer(&entry.field("index")?, 0)?;
                let time = entry.field("time")?;
                if time.present() {
                    integer(&time, 1)?;
                }
            } else {
                integer(&entry, 0)?;
            }
        }
    }
    let frame_time = animation.field("frametime")?;
    if frame_time.present() {
        integer(&frame_time, 1)?;
    }
    animation.field("interpolate")?.flag()?;
    let width = animation.field("width")?;
    let height = animation.field("height")?;
    if width.present() {
        let frame_height = if height.present() {
            i64::from(integer(&height, 1)?)
        } else {
            image_height
        };
        return Ok((i64::from(integer(&width, 1)?), frame_height));
    }
    if height.present() {
        return Ok((image_width, i64::from(integer(&height, 1)?)));
    }
    let side = image_width.min(image_height);
    Ok((side, side))
}

/// `TextureMetadataSection`.
fn texture_section(section: &Json<'_>) -> Res<()> {
    if !section.present() {
        return Ok(());
    }
    section.object()?;
    section.field("blur")?.flag()?;
    section.field("clamp")?.flag()?;
    let strategy = section.field("mipmap_strategy")?;
    if strategy.present() && !MIPMAP_STRATEGIES.contains(&strategy.string()?) {
        return Err(strategy.error(&format!("expected one of {}", MIPMAP_STRATEGIES.join(", "))));
    }
    let bias = section.field("alpha_cutoff_bias")?;
    if bias.present() {
        bias.decimal()?;
    }
    Ok(())
}

/// `GuiMetadataSection`, which only the gui atlas reads.
fn gui_section(section: &Json<'_>) -> Res<()> {
    let scaling = section.object()?.field("scaling")?;
    if !scaling.present() {
        return Ok(());
    }
    let kind = scaling.object()?.field("type")?;
    match kind.string()? {
        "stretch" => {}
        "tile" => {
            integer(&scaling.field("width")?, 1)?;
            integer(&scaling.field("height")?, 1)?;
        }
        "nine_slice" => {
            let width = integer(&scaling.field("width")?, 1)?;
            let height = integer(&scaling.field("height")?, 1)?;
            let border = scaling.field("border")?;
            // One positive size for the four sides, or each side as a non-negative size.
            let sides = if border.is_object() {
                let side = |name: &str| -> Res<i32> { integer(&border.field(name)?, 0) };
                [side("left")?, side("top")?, side("right")?, side("bottom")?]
            } else {
                [integer(&border, 1)?; 4]
            };
            scaling.field("stretch_inner")?.flag()?;
            if sides[0] + sides[2] >= width {
                return Err(border.error(&format!(
                    "nine-sliced texture has no horizontal center slice: {} + {} >= {width}",
                    sides[0], sides[2]
                )));
            }
            if sides[1] + sides[3] >= height {
                return Err(border.error(&format!(
                    "nine-sliced texture has no vertical center slice: {} + {} >= {height}",
                    sides[1], sides[3]
                )));
            }
        }
        _ => return Err(kind.error("expected stretch, tile or nine_slice")),
    }
    Ok(())
}
