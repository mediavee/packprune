//! Attachment, the part that does not depend on a view: what each token of a file names at the file's place (its
//! kind and logical path) under the client rules. Computed once per file; the walk ([`crate::walk`]) only resolves
//! targets in a view.

use std::collections::{BTreeSet, HashMap};
use std::ops::Range;
use std::sync::Arc;

use packprune_pack::kinds::ResourcePath;
use packprune_pack::rules::{FileRule, SpriteRule, TextureRule};
use packprune_pack::{ClientRules, FileKind, Interner, LogicalPath, Pointer, ResourceId};

use crate::atlases::{AtlasFile, atlas_sources};
use crate::json::{id_parts, type_name, unescape};
use crate::tokens::OBJECT_MARKER;
use crate::{Names, RawRef, Role};

/// Where a reference is written and what it names, as the index records it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Note {
    pub pointer: Pointer,
    pub span: Range<u32>,
    pub names: Names,
    pub reason: Box<str>,
}

/// One step of the walk's visit of a file, in its order.
#[derive(Debug)]
pub(crate) enum Act {
    /// Only noted (atlas sources: the walk follows their sprites through the view's atlases).
    Note(Note),
    /// A finding without a target to look up: an id that does not parse.
    Missing {
        target: Box<str>,
        reason: Box<str>,
        unverified_only: bool,
    },
    /// A file reference: the target is linked, or missing unless vanilla has it. `quiet_verified`: a model override,
    /// which the jar's client ignores, so a target it lacks is no finding in a verified view.
    Ref {
        note: Option<Note>,
        target: Box<str>,
        reason: Box<str>,
        quiet_verified: bool,
    },
    /// A sprite, resolved through the atlases of `atlases` only.
    Sprite {
        note: Note,
        sprite: ResourceId,
        shown: Box<str>,
        atlases: Arc<[Box<str>]>,
        reason: Box<str>,
        missing_sprite: bool,
    },
    SoundFile {
        note: Option<Note>,
        file: Box<str>,
        minecraft: bool,
    },
    SoundEvent {
        note: Note,
        namespace: Box<str>,
        path: Box<str>,
        shown: Box<str>,
    },
}

/// A `#moj_import` as written.
#[derive(Clone, Debug)]
pub(crate) struct Import {
    pub quoted: bool,
    pub raw: Box<str>,
    pub span: Range<u32>,
}

/// What one file's tokens mean at its place.
#[derive(Debug, Default)]
pub(crate) struct Facts {
    pub acts: Vec<Act>,
    pub imports: Vec<Import>,
    pub atlas: Option<Arc<AtlasFile>>,
    /// For a `sounds.json` that parses: the events it defines.
    pub events: Option<BTreeSet<Box<str>>>,
    /// The tokenizer's message for a file it cannot read.
    pub invalid: Option<Box<str>>,
}

pub(crate) struct Attach<'a> {
    pub rules: &'a ClientRules,
    pub interner: &'a Interner,
    model_atlases: Arc<[Box<str>]>,
    particle_atlases: Arc<[Box<str>]>,
    waypoint_atlases: Arc<[Box<str>]>,
    missing_sprite: Option<(String, String)>,
}

impl<'a> Attach<'a> {
    pub(crate) fn new(rules: &'a ClientRules, interner: &'a Interner) -> Self {
        let list = |atlases: &[String]| atlases.iter().map(|atlas| atlas.as_str().into()).collect();
        Self {
            rules,
            interner,
            model_atlases: list(&rules.model_atlases),
            particle_atlases: list(&rules.particle_atlases),
            waypoint_atlases: list(&rules.waypoint_sprites.atlases),
            missing_sprite: id_parts(&rules.missing_sprite)
                .map(|(ns, path)| (ns.to_owned(), path.to_owned())),
        }
    }

    pub(crate) fn facts(
        &self,
        tokens: &Result<Vec<RawRef>, Box<str>>,
        kind: FileKind,
        logical: Option<&str>,
    ) -> Facts {
        let tokens = match tokens {
            Ok(tokens) => tokens,
            Err(why) => {
                return Facts {
                    invalid: Some(why.clone()),
                    ..Facts::default()
                };
            }
        };
        let (imports, json): (Vec<&RawRef>, Vec<&RawRef>) = tokens.iter().partition(|token| {
            matches!(
                token.role,
                Role::ShaderImportQuoted | Role::ShaderImportAngled
            )
        });
        let mut facts = Facts {
            imports: imports
                .into_iter()
                .map(|token| Import {
                    quoted: token.role == Role::ShaderImportQuoted,
                    raw: token.raw.clone(),
                    span: token.span.clone(),
                })
                .collect(),
            ..Facts::default()
        };
        let mut acts = Vec::new();
        match kind {
            FileKind::Model => self.model(&json, &mut acts),
            FileKind::ItemDefinition => self.item(&json, &mut acts),
            FileKind::Blockstate => {
                for token in &json {
                    acts.push(self.file_ref(token, "models", ".json", "blockstate.model"));
                }
            }
            FileKind::Atlas => {
                let atlas_id = logical.and_then(ResourcePath::of).map(|path| {
                    format!(
                        "{}:{}",
                        path.namespace,
                        path.rest.strip_suffix(".json").unwrap_or(path.rest)
                    )
                });
                facts.atlas = Some(Arc::new(AtlasFile::from_tokens(self.interner, &json)));
                let notes = atlas_notes(self.interner, &json, atlas_id.as_deref());
                acts.extend(notes.into_iter().map(Act::Note));
            }
            FileKind::Equipment => {
                let rule = &self.rules.equipment_textures;
                for token in &json {
                    let layer = token
                        .pointer
                        .0
                        .split('/')
                        .nth(2)
                        .map(unescape)
                        .unwrap_or_default();
                    let folder = format!("{}/{layer}", rule.folder);
                    acts.push(self.file_ref(token, &folder, &rule.suffix, "equipment.layer"));
                }
            }
            FileKind::Font => self.font(&json, &mut acts),
            FileKind::SoundsJson => facts.events = Some(self.sounds(&json, &mut acts)),
            FileKind::PostEffect => {
                let textures = &self.rules.post_effect_textures;
                for token in &json {
                    let pointer = &*token.pointer.0;
                    acts.push(if pointer.ends_with("/vertex_shader") {
                        self.file_ref(token, "shaders", ".vsh", "post_effect.shader")
                    } else if pointer.ends_with("/fragment_shader") {
                        self.file_ref(token, "shaders", ".fsh", "post_effect.shader")
                    } else {
                        self.file_ref(
                            token,
                            &textures.folder,
                            &textures.suffix,
                            "post_effect.texture",
                        )
                    });
                }
            }
            FileKind::Particle => {
                for token in &json {
                    let rule = (Arc::clone(&self.particle_atlases), "", "");
                    acts.push(self.sprite(token, rule, "particle.texture"));
                }
            }
            FileKind::WaypointStyle => {
                let rule = &self.rules.waypoint_sprites;
                for token in &json {
                    let atlases = Arc::clone(&self.waypoint_atlases);
                    acts.push(self.sprite(
                        token,
                        (atlases, &rule.prefix, &rule.suffix),
                        "waypoint_style.sprite",
                    ));
                }
            }
            _ => {}
        }
        facts.acts = acts;
        facts
    }

    fn model(&self, tokens: &[&RawRef], acts: &mut Vec<Act>) {
        for token in tokens {
            match token.role {
                Role::ModelParent => {
                    let builtin = id_parts(&token.raw).is_some_and(|(ns, path)| {
                        ns == "minecraft" && path.starts_with(&*self.rules.builtin_model_prefix)
                    });
                    if !builtin {
                        acts.push(self.file_ref(token, "models", ".json", "model.parent"));
                    }
                }
                Role::TextureSlot => {
                    let rule = (Arc::clone(&self.model_atlases), "", "");
                    acts.push(self.sprite(token, rule, "model.texture"));
                }
                _ => acts.push(
                    match self.file_ref(token, "models", ".json", "model.override") {
                        Act::Ref {
                            note,
                            target,
                            reason,
                            ..
                        } => Act::Ref {
                            note,
                            target,
                            reason,
                            quiet_verified: true,
                        },
                        Act::Missing { target, reason, .. } => Act::Missing {
                            target,
                            reason,
                            unverified_only: true,
                        },
                        act => act,
                    },
                ),
            }
        }
    }

    fn item(&self, tokens: &[&RawRef], acts: &mut Vec<Act>) {
        let mut special_type: Option<&str> = None;
        for token in tokens {
            let pointer = &*token.pointer.0;
            if pointer.ends_with("/type") {
                special_type = Some(&token.raw);
            } else if pointer.ends_with("/texture") {
                let Some(written) = special_type.take() else {
                    continue;
                };
                let type_id = id_parts(written)
                    .map(|(ns, path)| format!("{ns}:{path}"))
                    .unwrap_or_default();
                let reason = format!("item.special.{}", type_name(written));
                match self.rules.special_textures.get(&type_id) {
                    Some(TextureRule::Sprite(SpriteRule {
                        atlases,
                        prefix,
                        suffix,
                    })) => {
                        let atlases = atlases.iter().map(|atlas| atlas.as_str().into()).collect();
                        acts.push(self.sprite(token, (atlases, prefix, suffix), &reason));
                    }
                    Some(TextureRule::File(FileRule { folder, suffix })) => {
                        acts.push(self.file_ref(token, folder, suffix, &reason));
                    }
                    None => {}
                }
            } else if pointer.ends_with("/base") {
                acts.push(self.file_ref(token, "models", ".json", "item.special.base"));
            } else {
                acts.push(self.file_ref(token, "models", ".json", "item.model"));
            }
        }
    }

    fn font(&self, tokens: &[&RawRef], acts: &mut Vec<Act>) {
        let mut provider_type = "";
        for token in tokens {
            let pointer = &*token.pointer.0;
            if pointer.ends_with("/type") {
                provider_type = type_name(&token.raw);
                continue;
            }
            acts.push(match provider_type {
                "bitmap" => self.file_ref(token, "textures", "", "font.bitmap"),
                "ttf" => self.file_ref(token, "font", "", "font.ttf"),
                "reference" => self.file_ref(token, "font", ".json", "font.reference"),
                _ => self.file_ref(token, "", "", "font.unihex"),
            });
        }
    }

    fn sounds(&self, tokens: &[&RawRef], acts: &mut Vec<Act>) -> BTreeSet<Box<str>> {
        let mut events = BTreeSet::new();
        let mut types: HashMap<&str, &str> = HashMap::new();
        for token in tokens {
            let pointer = &*token.pointer.0;
            if token.role == Role::SoundName {
                let object = pointer.strip_suffix("/name");
                let event =
                    object.is_some() && object.and_then(|at| types.get(at)) == Some(&"event");
                acts.push(match id_parts(&token.raw) {
                    None => Act::Missing {
                        target: token.raw.clone(),
                        reason: "sound (invalid id)".into(),
                        unverified_only: false,
                    },
                    Some((ns, path)) if event => Act::SoundEvent {
                        note: note(
                            token,
                            Names::SoundEvent {
                                event: self.id(ns, path),
                            },
                            "sound.event",
                        ),
                        namespace: ns.into(),
                        path: path.into(),
                        shown: format!("{ns}:{path}").into(),
                    },
                    Some((ns, path)) => {
                        let file = format!("assets/{ns}/sounds/{path}.ogg");
                        Act::SoundFile {
                            note: file_note(token, &file, "sounds", ".ogg", "sound.file"),
                            file: file.into(),
                            minecraft: ns == "minecraft",
                        }
                    }
                });
            } else if pointer.matches('/').count() == 1 {
                events.insert(token.raw.clone());
            } else if let Some(object) = pointer.strip_suffix("/type") {
                types.insert(object, &token.raw);
            }
        }
        events
    }

    pub(crate) fn id(&self, ns: &str, path: &str) -> ResourceId {
        intern(self.interner, ns, path)
    }

    /// A reference to the file `assets/<ns>/<folder>/<path><suffix>` the token names.
    fn file_ref(&self, token: &RawRef, folder: &str, suffix: &str, reason: &str) -> Act {
        match id_parts(&token.raw) {
            None => Act::Missing {
                target: token.raw.clone(),
                reason: format!("{reason} (invalid id)").into(),
                unverified_only: false,
            },
            Some((ns, path)) => {
                let folder_part = if folder.is_empty() {
                    String::new()
                } else {
                    format!("{folder}/")
                };
                let target = format!("assets/{ns}/{folder_part}{path}{suffix}");
                Act::Ref {
                    note: file_note(token, &target, folder, suffix, reason),
                    target: target.into(),
                    reason: reason.into(),
                    quiet_verified: false,
                }
            }
        }
    }

    /// A reference to the sprite `<prefix><path><suffix>` the token names in one of `atlases`.
    fn sprite(
        &self,
        token: &RawRef,
        (atlases, prefix, suffix): (Arc<[Box<str>]>, &str, &str),
        reason: &str,
    ) -> Act {
        let Some((ns, path)) = id_parts(&token.raw) else {
            return Act::Missing {
                target: token.raw.clone(),
                reason: format!("{reason} (invalid id)").into(),
                unverified_only: false,
            };
        };
        let sprite_path = format!("{prefix}{path}{suffix}");
        let sprite = self.id(ns, &sprite_path);
        let names = Names::Sprite {
            sprite,
            atlases: atlases
                .iter()
                .filter_map(|atlas| id_parts(atlas).map(|(ns, path)| self.id(ns, path)))
                .collect(),
            prefix: prefix.into(),
            suffix: suffix.into(),
        };
        let missing_sprite =
            self.missing_sprite
                .as_ref()
                .is_some_and(|(missing_ns, missing_path)| {
                    missing_ns == ns && *missing_path == sprite_path
                });
        Act::Sprite {
            note: note(token, names, reason),
            sprite,
            shown: format!("{ns}:{sprite_path}").into(),
            atlases,
            reason: reason.into(),
            missing_sprite,
        }
    }
}

pub(crate) fn intern(interner: &Interner, ns: &str, path: &str) -> ResourceId {
    ResourceId {
        ns: interner.intern(ns),
        path: interner.intern(path),
    }
}

fn note(token: &RawRef, names: Names, reason: &str) -> Note {
    Note {
        pointer: token.pointer.clone(),
        span: token.span.clone(),
        names,
        reason: reason.into(),
    }
}

/// A file a reference names; a target that is no valid pack path (a `..` segment) cannot be named, so it is not
/// noted, and only the walk sees it, as missing.
fn file_note(
    token: &RawRef,
    logical: &str,
    folder: &str,
    suffix: &str,
    reason: &str,
) -> Option<Note> {
    let logical = LogicalPath::new(logical).ok()?;
    Some(note(
        token,
        Names::File {
            logical,
            folder: folder.into(),
            suffix: suffix.into(),
        },
        reason,
    ))
}

/// Every texture and sprite id the sources of the atlas file `atlas_id` write, where the index loop reaches them
/// (arrays only).
fn atlas_notes(interner: &Interner, tokens: &[&RawRef], atlas_id: Option<&str>) -> Vec<Note> {
    let markers: BTreeSet<&str> = tokens
        .iter()
        .filter_map(|token| token.pointer.0.strip_suffix(OBJECT_MARKER))
        .collect();
    let indexed = |pointer: &str| !markers.contains(pointer);
    let mut notes = Vec::new();
    if !indexed("/sources") {
        return notes;
    }
    let atlas = atlas_id
        .and_then(id_parts)
        .map(|(ns, path)| intern(interner, ns, path));
    let sprite_name = |token: &RawRef, (ns, path): (&str, &str), implicit: bool| {
        atlas
            .map(|atlas| Names::SpriteName {
                atlas,
                sprite: intern(interner, ns, path),
                implicit,
            })
            .map(|names| note(token, names, "atlas.sprite"))
    };
    let texture = |token: &RawRef, reason: &str| {
        let (ns, path) = id_parts(&token.raw)?;
        file_note(
            token,
            &format!("assets/{ns}/textures/{path}.png"),
            "textures",
            ".png",
            reason,
        )
    };
    for source in atlas_sources(tokens) {
        let at = source.at;
        match source.kind() {
            "single" => {
                let Some(resource) = source.field("resource") else {
                    continue;
                };
                let Some(id) = id_parts(&resource.raw) else {
                    continue;
                };
                notes.extend(texture(resource, "atlas.single"));
                match source.field("sprite") {
                    Some(sprite) if sprite.role == Role::Sprite => {
                        notes.extend(
                            id_parts(&sprite.raw).and_then(|id| sprite_name(sprite, id, false)),
                        );
                    }
                    Some(_) => {}
                    None => {
                        // The source names no sprite: the note is the source object's, on its resource's bytes.
                        notes.extend(sprite_name(resource, id, true).map(|mut implicit| {
                            implicit.pointer = Pointer::new(at);
                            implicit
                        }));
                    }
                }
            }
            "unstitch" => {
                if let Some(resource) = source.field("resource") {
                    notes.extend(texture(resource, "atlas.unstitch"));
                }
                if indexed(&format!("{at}/regions")) {
                    for sprite in source.region_sprites() {
                        notes.extend(
                            id_parts(&sprite.raw).and_then(|id| sprite_name(sprite, id, false)),
                        );
                    }
                }
            }
            "paletted_permutations" => {
                if indexed(&format!("{at}/textures")) {
                    for token in source.under("textures") {
                        notes.extend(texture(token, "atlas.paletted_permutations"));
                    }
                }
                if let Some(key) = source.field("palette_key") {
                    notes.extend(texture(key, "atlas.palette_key"));
                }
                for token in source.under("permutations") {
                    notes.extend(texture(token, "atlas.palette"));
                }
            }
            _ => {}
        }
    }
    notes
}
