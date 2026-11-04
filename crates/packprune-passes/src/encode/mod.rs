//! The lossless file optimisers: each wraps a codec of `packprune-codec`, which checks its own output (pixels, PCM,
//! JSON value, GLSL tokens, properties entries, NBT payload). Named after the config's file kinds, whose options they
//! read.

#[cfg(test)]
mod corpus;
#[cfg(test)]
mod tests;

use packprune_codec::{CodecError, DEFAULT_EFFORT, audio, glsl, minify, nbt, png, properties};
use packprune_pack::{FileKind, PhysPath};
use packprune_pass::{FileCx, FileOptimizer, Loss, Optimised, Pass, PassError};
use schemars::json_schema;
use serde::Deserialize;
use serde::de::DeserializeOwned;

pub fn passes() -> Vec<Box<dyn Pass>> {
    Vec::new()
}

pub fn optimisers() -> Vec<Box<dyn FileOptimizer>> {
    vec![
        Box::new(Json),
        Box::new(Png),
        Box::new(Audio),
        Box::new(Shader),
        Box::new(Properties),
        Box::new(LegacyLang),
        Box::new(Mcfunction),
        Box::new(Nbt),
    ]
}

/// Token-level JSON minification: whitespace out, every token's bytes kept.
struct Json;

impl FileOptimizer for Json {
    fn name(&self) -> &'static str {
        "json"
    }

    fn loss(&self) -> Loss {
        Loss::Lossless
    }

    fn applies(&self, _: FileKind, path: &PhysPath) -> bool {
        has_extension(path, &[".json", ".mcmeta"])
    }

    fn options_schema(&self) -> schemars::Schema {
        flags(&["minify"])
    }

    fn validate_options(&self, options: &toml::Table) -> Result<(), String> {
        read::<Minify>(options).map(drop)
    }

    fn optimise(
        &self,
        input: &[u8],
        _: &FileCx,
        options: &toml::Table,
    ) -> Result<Option<Optimised>, PassError> {
        if !options_of::<Minify>(self, options)?.minify {
            return Ok(None);
        }
        Ok(smaller(input, minify::json(input)))
    }
}

/// oxipng, its output decoded as the client decodes and compared to the input's pixels.
struct Png;

impl FileOptimizer for Png {
    fn name(&self) -> &'static str {
        "png"
    }

    fn loss(&self) -> Loss {
        Loss::Lossless
    }

    fn applies(&self, _: FileKind, path: &PhysPath) -> bool {
        has_extension(path, &[".png"])
    }

    fn options_schema(&self) -> schemars::Schema {
        // `alpha = "rewrite"` and quantisation are lossy: the lossy PNG optimisers take them.
        json_schema!({
            "type": "object",
            "properties": {
                "effort": {"type": "integer", "minimum": 0, "maximum": 255},
                "alpha": {"enum": ["keep"]},
                "quantization": {"enum": ["none"]},
            },
            "additionalProperties": false,
        })
    }

    fn validate_options(&self, options: &toml::Table) -> Result<(), String> {
        read::<PngOptions>(options).map(drop)
    }

    fn optimise(
        &self,
        input: &[u8],
        cx: &FileCx,
        options: &toml::Table,
    ) -> Result<Option<Optimised>, PassError> {
        let effort = options_of::<PngOptions>(self, options)?.effort;
        let banner = cx.quirks.abgr_layers && png_layers(&cx.path).banner;
        let options = png::Options {
            effort,
            keep_color_type: banner,
            keep_bit_depth: false,
            keep_grayscale: cx.quirks.imageio_grey,
        };
        Ok(smaller(input, png::optimise(input, &options)))
    }
}

/// OptiVorbis remuxing, and silence replaced by the smallest stream of the same length; decoded PCM compared.
struct Audio;

impl FileOptimizer for Audio {
    fn name(&self) -> &'static str {
        "audio"
    }

    fn loss(&self) -> Loss {
        Loss::Lossless
    }

    fn applies(&self, _: FileKind, path: &PhysPath) -> bool {
        has_extension(path, &[".ogg"])
    }

    fn options_schema(&self) -> schemars::Schema {
        // `transcode = true` is lossy: the Ogg transcode pass takes it.
        json_schema!({
            "type": "object",
            "properties": {
                "optimize_ogg": {"type": "boolean"},
                "empty_audio": {"type": "boolean"},
                "transcode": {"enum": [false]},
            },
            "additionalProperties": false,
        })
    }

    fn validate_options(&self, options: &toml::Table) -> Result<(), String> {
        read::<AudioOptions>(options).map(drop)
    }

    fn optimise(
        &self,
        input: &[u8],
        _: &FileCx,
        options: &toml::Table,
    ) -> Result<Option<Optimised>, PassError> {
        let options = options_of::<AudioOptions>(self, options)?;
        if options.empty_audio
            && let Some(silent) = smaller(input, audio::optimise_empty(input))
        {
            return Ok(Some(silent));
        }
        if !options.optimize_ogg {
            return Ok(None);
        }
        Ok(smaller(input, audio::optimise(input)))
    }
}

/// Comments and whitespace out of GLSL, preprocessor lines kept byte for byte, the token sequence checked.
struct Shader;

impl FileOptimizer for Shader {
    fn name(&self) -> &'static str {
        "shader"
    }

    fn loss(&self) -> Loss {
        Loss::Lossless
    }

    fn applies(&self, kind: FileKind, _: &PhysPath) -> bool {
        matches!(kind, FileKind::Shader | FileKind::ShaderInclude)
    }

    fn options_schema(&self) -> schemars::Schema {
        flags(&["minify"])
    }

    fn validate_options(&self, options: &toml::Table) -> Result<(), String> {
        read::<Minify>(options).map(drop)
    }

    fn optimise(
        &self,
        input: &[u8],
        _: &FileCx,
        options: &toml::Table,
    ) -> Result<Option<Optimised>, PassError> {
        if !options_of::<Minify>(self, options)?.minify {
            return Ok(None);
        }
        Ok(smaller(input, glsl::minify(input)))
    }
}

/// `key=value` lines only, every key and value byte kept, the entries checked.
struct Properties;

impl FileOptimizer for Properties {
    fn name(&self) -> &'static str {
        "properties"
    }

    fn loss(&self) -> Loss {
        Loss::Lossless
    }

    fn applies(&self, _: FileKind, path: &PhysPath) -> bool {
        has_extension(path, &[".properties"])
    }

    fn options_schema(&self) -> schemars::Schema {
        flags(&["minify"])
    }

    fn validate_options(&self, options: &toml::Table) -> Result<(), String> {
        read::<Minify>(options).map(drop)
    }

    fn optimise(
        &self,
        input: &[u8],
        _: &FileCx,
        options: &toml::Table,
    ) -> Result<Option<Optimised>, PassError> {
        if !options_of::<Minify>(self, options)?.minify {
            return Ok(None);
        }
        Ok(smaller(input, properties::minify(input)))
    }
}

/// Comment and blank lines out of a legacy `.lang`, and its UTF-8 BOM.
struct LegacyLang;

impl FileOptimizer for LegacyLang {
    fn name(&self) -> &'static str {
        "legacy_lang"
    }

    fn loss(&self) -> Loss {
        Loss::Lossless
    }

    fn applies(&self, _: FileKind, path: &PhysPath) -> bool {
        has_extension(path, &[".lang"])
    }

    fn options_schema(&self) -> schemars::Schema {
        flags(&["minify", "strip_bom"])
    }

    fn validate_options(&self, options: &toml::Table) -> Result<(), String> {
        read::<LangOptions>(options).map(drop)
    }

    fn optimise(
        &self,
        input: &[u8],
        _: &FileCx,
        options: &toml::Table,
    ) -> Result<Option<Optimised>, PassError> {
        let LangOptions { minify, strip_bom } = options_of(self, options)?;
        if !minify {
            let stripped = input.strip_prefix(b"\xef\xbb\xbf").filter(|_| strip_bom);
            return Ok(stripped.map(|bytes| Optimised {
                bytes: bytes.to_vec(),
                loss: None,
            }));
        }
        Ok(smaller(input, minify::legacy_lang(input, strip_bom)))
    }
}

/// Comment and blank lines out of a function, continued commands kept whole.
struct Mcfunction;

impl FileOptimizer for Mcfunction {
    fn name(&self) -> &'static str {
        "mcfunction"
    }

    fn loss(&self) -> Loss {
        Loss::Lossless
    }

    fn applies(&self, _: FileKind, path: &PhysPath) -> bool {
        has_extension(path, &[".mcfunction"])
    }

    fn options_schema(&self) -> schemars::Schema {
        flags(&["minify"])
    }

    fn validate_options(&self, options: &toml::Table) -> Result<(), String> {
        read::<Minify>(options).map(drop)
    }

    fn optimise(
        &self,
        input: &[u8],
        _: &FileCx,
        options: &toml::Table,
    ) -> Result<Option<Optimised>, PassError> {
        if !options_of::<Minify>(self, options)?.minify {
            return Ok(None);
        }
        Ok(smaller(input, minify::mcfunction(input)))
    }
}

/// A gzip NBT member recompressed with Zopfli, its payload compared.
struct Nbt;

impl FileOptimizer for Nbt {
    fn name(&self) -> &'static str {
        "nbt"
    }

    fn loss(&self) -> Loss {
        Loss::Lossless
    }

    fn applies(&self, _: FileKind, path: &PhysPath) -> bool {
        has_extension(path, &[".nbt"])
    }

    fn options_schema(&self) -> schemars::Schema {
        json_schema!({
            "type": "object",
            "properties": {"effort": {"type": "integer", "minimum": 0, "maximum": 255}},
            "additionalProperties": false,
        })
    }

    fn validate_options(&self, options: &toml::Table) -> Result<(), String> {
        read::<NbtOptions>(options).map(drop)
    }

    fn optimise(
        &self,
        input: &[u8],
        _: &FileCx,
        options: &toml::Table,
    ) -> Result<Option<Optimised>, PassError> {
        let effort = options_of::<NbtOptions>(self, options)?.effort;
        Ok(smaller(input, nbt::recompress(input, effort)))
    }
}

/// The codec's output when it is smaller. A codec error keeps the input: either the file is not one the codec reads
/// whole, or the codec's own check refused its output.
fn smaller(input: &[u8], output: Result<Vec<u8>, CodecError>) -> Option<Optimised> {
    output
        .ok()
        .filter(|bytes| bytes.len() < input.len())
        .map(|bytes| Optimised { bytes, loss: None })
}

fn has_extension(path: &PhysPath, extensions: &[&str]) -> bool {
    extensions.iter().any(|ext| path.as_str().ends_with(ext))
}

/// The vanilla textures that some client reads other than as a plain image.
pub(crate) struct PngLayers {
    /// 1.12.2's `BannerTextures` composes a `LayeredColorMaskTexture` from a base and the patterns of a folder.
    pub banner: bool,
    /// What `RenderType.eyes` draws in 24w38a: `EnderDragonRenderer`, `EnderEyesLayer`, `PhantomEyesLayer` and
    /// `SpiderEyesLayer`.
    pub eyes: bool,
    /// `PostChain` binds `textures/effect/` to shader samplers: the texels are data.
    pub effect: bool,
}

pub(crate) fn png_layers(path: &PhysPath) -> PngLayers {
    let vanilla = logical(path).and_then(|path| path.strip_prefix("assets/minecraft/textures/"));
    let entity = vanilla.and_then(|path| path.strip_prefix("entity/"));
    PngLayers {
        banner: entity.is_some_and(|path| {
            ["banner_base.png", "shield_base.png"].contains(&path)
                || path.starts_with("banner/")
                || path.starts_with("shield/")
        }),
        eyes: entity.is_some_and(|path| {
            [
                "enderdragon/dragon_eyes.png",
                "enderman/enderman_eyes.png",
                "phantom_eyes.png",
                "spider_eyes.png",
            ]
            .contains(&path)
        }),
        effect: vanilla.is_some_and(|path| path.starts_with("effect/")),
    }
}

/// The path from `assets/` on, in the root or an overlay directory.
pub(crate) fn logical(path: &PhysPath) -> Option<&str> {
    let text = path.as_str();
    if text.starts_with("assets/") {
        return Some(text);
    }
    let (_, rest) = text.split_once('/')?;
    rest.starts_with("assets/").then_some(rest)
}

fn flags(names: &[&str]) -> schemars::Schema {
    let properties: serde_json::Map<String, serde_json::Value> = names
        .iter()
        .map(|name| ((*name).to_owned(), serde_json::json!({"type": "boolean"})))
        .collect();
    json_schema!({"type": "object", "properties": properties, "additionalProperties": false})
}

/// `options` read as `T`: what the config is checked with when it is loaded, and what the optimiser runs with.
fn read<T: DeserializeOwned>(options: &toml::Table) -> Result<T, String> {
    options
        .clone()
        .try_into()
        .map_err(|error: toml::de::Error| error.to_string().trim_end().replace('\n', " "))
}

fn options_of<T: DeserializeOwned>(
    optimiser: &dyn FileOptimizer,
    options: &toml::Table,
) -> Result<T, PassError> {
    read(options).map_err(|why| PassError::Failed {
        pass: optimiser.name(),
        why,
    })
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, default)]
struct Minify {
    minify: bool,
}

impl Default for Minify {
    fn default() -> Self {
        Self { minify: true }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, default)]
struct LangOptions {
    minify: bool,
    strip_bom: bool,
}

impl Default for LangOptions {
    fn default() -> Self {
        Self {
            minify: true,
            strip_bom: true,
        }
    }
}

/// `alpha = "rewrite"` and quantisation are lossy: only their accepted lossless values are read here.
#[derive(Deserialize)]
#[serde(deny_unknown_fields, default)]
struct PngOptions {
    effort: u8,
    #[serde(rename = "alpha")]
    _alpha: Keep,
    #[serde(rename = "quantization")]
    _quantization: NoQuantization,
}

impl Default for PngOptions {
    fn default() -> Self {
        Self {
            effort: DEFAULT_EFFORT,
            _alpha: Keep::Keep,
            _quantization: NoQuantization::None,
        }
    }
}

#[derive(Deserialize)]
enum Keep {
    #[serde(rename = "keep")]
    Keep,
}

#[derive(Deserialize)]
enum NoQuantization {
    #[serde(rename = "none")]
    None,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, default)]
struct AudioOptions {
    optimize_ogg: bool,
    empty_audio: bool,
    #[serde(rename = "transcode")]
    _transcode: NoTranscode,
}

impl Default for AudioOptions {
    fn default() -> Self {
        Self {
            optimize_ogg: true,
            empty_audio: true,
            _transcode: NoTranscode,
        }
    }
}

/// `transcode = true` is lossy: the Ogg transcode pass takes it.
#[derive(Deserialize)]
#[serde(try_from = "bool")]
struct NoTranscode;

impl TryFrom<bool> for NoTranscode {
    type Error = &'static str;

    fn try_from(transcode: bool) -> Result<Self, Self::Error> {
        if transcode {
            Err("transcode = true is lossy: a lossy.<pass> table takes it")
        } else {
            Ok(Self)
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, default)]
struct NbtOptions {
    effort: u8,
}

impl Default for NbtOptions {
    fn default() -> Self {
        Self {
            effort: DEFAULT_EFFORT,
        }
    }
}
