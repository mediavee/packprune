use packprune_graph::UsageKind::{self, *};

#[derive(Clone, Copy)]
pub(super) enum Form {
    AsWritten,
    DropExtension,
    WithoutPng,
}
use Form::*;

pub(super) const ITEMS: &str = "items";
pub(super) const GLYPHS: &str = "glyphs";
pub(super) const SOUNDS: &str = "sounds.yml";
pub(super) const SETTINGS: &str = "settings.yml";
pub(super) const RECIPES: &str = "recipes";
pub(super) const MECHANICS: &str = "Mechanics";
pub(super) const NEXO_ITEM: &str = "nexo_item";
pub(super) const YAML: &str = "yml";
pub(super) const GENERATED_NAMESPACE: &str = "nexo";
pub(super) const PREFER_ITEM_MODELS: &str = "Pack.generation.prefer_item_models";
pub(super) const DEFAULT_FONT_SETTING: &str = "Glyphs.default_font";
pub(super) const DEFAULT_FONT: &str = "nexo:default";
pub(super) const MATERIAL: &str = "material";
pub(super) const DEFAULT_MATERIAL: &str = "paper";
pub(super) const TEMPLATE: &str = "template";
pub(super) const ITEM_MODEL_SECTION: &str = "ItemModel";
pub(super) const PACK: &str = "Pack";
pub(super) const PACK_MODEL: &str = "Pack.model";
pub(super) const PACK_BBMODEL: &str = "Pack.bbmodel";
pub(super) const PACK_TEXTURES: &[&str] = &["Pack.texture", "Pack.textures"];
pub(super) const CUSTOM_MODEL_DATA: &str = "Pack.custom_model_data";
pub(super) const ITEM_MODEL_COMPONENT: &str = "Components.item_model";
pub(super) const ARMOR: &str = "CustomArmor";
pub(super) const ARMOR_ID: &str = "id";
pub(super) const ARMOR_LAYERS: &[&str] = &[
    "layer1",
    "layer2",
    "humanoid_baby",
    "elytra",
    "wolf_armor",
    "llama_armor",
    "horse_armor",
    "nautilus_armor",
    "harness",
    "camel_saddle",
    "donkey_saddle",
    "horse_saddle",
    "mule_saddle",
    "pig_saddle",
    "skeleton_horse_saddle",
    "strider_saddle",
    "zombie_horse_saddle",
    "nautilus_saddle",
    "camel_husk_saddle",
];
pub(super) const GLYPH_TEXTURE: &str = "texture";
pub(super) const GLYPH_FONT: &str = "font";
pub(super) const GLYPH_ANIMATED: &str = "gif";
pub(super) const SOUNDS_GENERATE: &str = "settings.generate_sounds";
pub(super) const SOUNDS_ENTRIES: &str = "sounds";
pub(super) const SOUND_ID: &str = "id";
pub(super) const SOUND_TYPE: &str = "type";
pub(super) const EVENT: &str = "EVENT";
pub(super) const SOUND_REFERENCE: &str = "reference_id";
pub(super) const SOUND_FILES: &[&str] = &["sound", "sounds"];
pub(super) const SOUND_EXTENSION: &str = ".ogg";

pub(super) struct Rule {
    pub path: String,
    pub kinds: Vec<UsageKind>,
    pub form: Form,
    pub each: bool,
    pub note: &'static str,
}
impl Rule {
    fn id(path: impl Into<String>, kind: UsageKind, form: Form) -> Self {
        Self {
            path: path.into(),
            kinds: vec![kind],
            form,
            each: false,
            note: "",
        }
    }
    fn model_texture(path: impl Into<String>, form: Form) -> Self {
        Self {
            path: path.into(),
            kinds: vec![Model, Sprite],
            form,
            each: false,
            note: "a model, or a texture when no such model exists",
        }
    }
}

pub(super) fn item_rules() -> Vec<Rule> {
    let mut out = vec![
        Rule::id("Pack.parent_model", Model, AsWritten),
        Rule::id("Pack.parent", Model, AsWritten),
        Rule::id(PACK_MODEL, Model, DropExtension),
        Rule::id("Pack.texture", Sprite, DropExtension),
        Rule {
            path: "Pack.textures".into(),
            kinds: vec![Sprite],
            form: DropExtension,
            each: true,
            note: "",
        },
        Rule::model_texture("Pack.pulling_models", AsWritten),
        Rule::model_texture("Pack.pulling_textures", DropExtension),
        Rule::model_texture("Pack.damaged_models", AsWritten),
        Rule::model_texture("Pack.damaged_textures", DropExtension),
        Rule::id("Pack.composite_models", Model, AsWritten),
        Rule::id("Pack.composite_textures", Model, DropExtension),
    ];
    for state in [
        "broken", "blocking", "throwing", "cast", "charged", "firework", "dyeable",
    ] {
        out.push(Rule::model_texture(
            format!("Pack.{state}_model"),
            DropExtension,
        ));
        out.push(Rule::model_texture(
            format!("Pack.{state}_texture"),
            DropExtension,
        ));
    }
    for (path, kind) in [
        (ITEM_MODEL_COMPONENT, ItemDefinition),
        ("Components.tooltip_style", TooltipStyle),
        ("Components.equippable.asset_id", Equipment),
        ("Components.equippable.camera_overlay", Sprite),
        ("Components.equippable.equip_sound", SoundEvent),
        ("Components.equippable.shear_sound", SoundEvent),
        ("Components.break_sound", SoundEvent),
        ("Components.blocks_attacks.block_sound", SoundEvent),
        ("Components.blocks_attacks.disable_sound", SoundEvent),
        ("Components.piercing_weapon.sound", SoundEvent),
        ("Components.piercing_weapon.hit_sound", SoundEvent),
        ("Components.kinetic_weapon.sound", SoundEvent),
        ("Components.kinetic_weapon.hit_sound", SoundEvent),
        ("Mechanics.furniture.item_model", ItemDefinition),
        ("Mechanics.custom_block.model", Model),
        ("Mechanics.furniture.door.open_sound", SoundEvent),
        ("Mechanics.furniture.door.close_sound", SoundEvent),
        ("Mechanics.furniture.storage.open_sound", SoundEvent),
        ("Mechanics.furniture.storage.close_sound", SoundEvent),
    ] {
        out.push(Rule::id(path, kind, AsWritten));
    }
    for mechanic in ["furniture", "custom_block"] {
        for sound in ["place", "break", "step", "hit", "fall"] {
            out.push(Rule::id(
                format!("Mechanics.{mechanic}.block_sounds.{sound}_sound"),
                SoundEvent,
                AsWritten,
            ));
        }
    }
    out
}
