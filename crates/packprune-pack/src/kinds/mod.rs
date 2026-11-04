use packprune_zip::EntryClass;

use crate::PhysPath;

#[cfg(test)]
mod tests;

/// What the client reads a file as.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum FileKind {
    PackMeta,
    ItemDefinition,
    Model,
    Texture,
    TextureMeta,
    Blockstate,
    Atlas,
    Equipment,
    Font,
    FontFile,
    SoundsJson,
    Sound,
    Lang,
    Shader,
    ShaderInclude,
    PostEffect,
    Particle,
    WaypointStyle,
    Text,
    /// A path the vanilla client has but no rule here reads, such as `gpu_warnlist.json`.
    Other,
    /// Not read by the vanilla client: OptiFine trees, editor files, undeclared folders. Kept and listed.
    Unknown,
}

/// What a texture's metadata adds to its path: the client reads `x.png.mcmeta` with `x.png`.
pub const META_SUFFIX: &str = ".mcmeta";

/// A logical path `assets/<namespace>/<folder>/<rest>`; a file directly in the namespace has an empty folder.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ResourcePath<'a> {
    pub namespace: &'a str,
    pub folder: &'a str,
    pub rest: &'a str,
}

impl<'a> ResourcePath<'a> {
    /// `None` outside `assets/<namespace>/`.
    pub fn of(logical: &'a str) -> Option<Self> {
        let after = logical.strip_prefix("assets/")?;
        let (namespace, inside) = after.split_once('/')?;
        Some(match inside.split_once('/') {
            Some((folder, rest)) => Self {
                namespace,
                folder,
                rest,
            },
            None => Self {
                namespace,
                folder: "",
                rest: inside,
            },
        })
    }
}

impl FileKind {
    /// The variant in snake case (`item_definition`).
    pub fn id(self) -> &'static str {
        match self {
            Self::PackMeta => "pack_meta",
            Self::ItemDefinition => "item_definition",
            Self::Model => "model",
            Self::Texture => "texture",
            Self::TextureMeta => "texture_meta",
            Self::Blockstate => "blockstate",
            Self::Atlas => "atlas",
            Self::Equipment => "equipment",
            Self::Font => "font",
            Self::FontFile => "font_file",
            Self::SoundsJson => "sounds_json",
            Self::Sound => "sound",
            Self::Lang => "lang",
            Self::Shader => "shader",
            Self::ShaderInclude => "shader_include",
            Self::PostEffect => "post_effect",
            Self::Particle => "particle",
            Self::WaypointStyle => "waypoint_style",
            Self::Text => "text",
            Self::Other => "other",
            Self::Unknown => "unknown",
        }
    }

    /// Classifies a logical path `assets/<namespace>/...` by its folder and suffix alone. A pack file also needs its
    /// layer and the vanilla names: [`crate::Layout`] does that.
    pub fn of(logical: &str) -> Self {
        let Some(path) = ResourcePath::of(logical) else {
            return Self::Unknown;
        };
        let rest = path.rest;
        if path.folder.is_empty() {
            return if rest == "sounds.json" {
                Self::SoundsJson
            } else {
                Self::Unknown
            };
        }
        let json = rest.ends_with(".json");
        let when = |test: bool, kind| if test { kind } else { Self::Unknown };
        match path.folder {
            "items" => when(json, Self::ItemDefinition),
            "models" => when(json, Self::Model),
            "textures" if rest.ends_with(".png") => Self::Texture,
            "textures" => when(rest.ends_with(".png.mcmeta"), Self::TextureMeta),
            "blockstates" => when(json, Self::Blockstate),
            "atlases" => when(json, Self::Atlas),
            "equipment" => when(json, Self::Equipment),
            "font" if json => Self::Font,
            "font" => Self::FontFile,
            "sounds" => when(rest.ends_with(".ogg"), Self::Sound),
            "lang" => when(json, Self::Lang),
            "shaders" if rest.ends_with(".vsh") || rest.ends_with(".fsh") => Self::Shader,
            "shaders" => when(rest.ends_with(".glsl"), Self::ShaderInclude),
            "post_effect" => when(json, Self::PostEffect),
            "particles" => when(json, Self::Particle),
            "waypoint_style" => when(json, Self::WaypointStyle),
            "texts" => Self::Text,
            _ => Self::Unknown,
        }
    }

    /// How the zip writer compresses a file of this kind. Kinds the client reads as JSON or GLSL are text and
    /// textures and sounds are already compressed; kinds that hold any file type (`pack.png`, font files, `texts/`,
    /// unknown files) go by the file's extension.
    pub fn entry_class(self, path: &PhysPath) -> EntryClass {
        match self {
            Self::Texture | Self::Sound => EntryClass::Compressed,
            Self::ItemDefinition
            | Self::Model
            | Self::TextureMeta
            | Self::Blockstate
            | Self::Atlas
            | Self::Equipment
            | Self::Font
            | Self::SoundsJson
            | Self::Lang
            | Self::Shader
            | Self::ShaderInclude
            | Self::PostEffect
            | Self::Particle
            | Self::WaypointStyle => EntryClass::Text,
            Self::PackMeta | Self::FontFile | Self::Text | Self::Other | Self::Unknown => {
                class_of_extension(path.as_str())
            }
        }
    }
}

fn class_of_extension(path: &str) -> EntryClass {
    const TEXT: &[&str] = &[
        "json",
        "mcmeta",
        "properties",
        "fsh",
        "vsh",
        "glsl",
        "txt",
        "lang",
        "mcfunction",
    ];
    const COMPRESSED: &[&str] = &["png", "ogg", "jpg", "jpeg", "nbt", "zip", "jar"];
    let file = path.rsplit('/').next().unwrap_or(path);
    let Some((_, extension)) = file.rsplit_once('.') else {
        return EntryClass::Other;
    };
    let is = |list: &[&str]| {
        list.iter()
            .any(|known| extension.eq_ignore_ascii_case(known))
    };
    if is(TEXT) {
        EntryClass::Text
    } else if is(COMPRESSED) {
        EntryClass::Compressed
    } else {
        EntryClass::Other
    }
}
