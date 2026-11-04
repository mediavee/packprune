//! The tokens of a file: every string the walk reads from it, in the order it reads them, with the JSON pointer and
//! byte span of each. Besides references, a token can be context the attachment needs to tell what a reference
//! names: the `type` of a typed object, an event name `sounds.json` defines, or the marker of a container that is an
//! object where the index reads only arrays. These are [`Role::Other`].

use packprune_pack::FileKind;
use packprune_pack::json::{self, Pointer};

use crate::glsl;
use crate::json::{Node, Seg, child, node_type};
use crate::{Invalid, RawRef, Role};

/// The pointer of a marker token: the container at `pointer` is an object, so the index loop skips it.
pub(crate) const OBJECT_MARKER: &str = "{";

pub(crate) fn tokenize(bytes: &[u8], kind: FileKind) -> Result<Vec<RawRef>, Invalid> {
    let walk: Option<fn(&Node, &mut Out)> = match kind {
        FileKind::Model => Some(model),
        FileKind::ItemDefinition => Some(|root, out| item(root, "", out)),
        FileKind::Blockstate => Some(|root, out| blockstate(root, "", out)),
        FileKind::Atlas => Some(atlas),
        FileKind::Equipment => Some(equipment),
        FileKind::Font => Some(font),
        FileKind::SoundsJson => Some(sounds),
        FileKind::PostEffect => Some(post_effect),
        FileKind::Particle => Some(|root, out| sprites(root, "textures", out)),
        FileKind::WaypointStyle => Some(|root, out| sprites(root, "sprites", out)),
        _ => None,
    };
    let mut out = Out(Vec::new());
    if let Some(walk) = walk {
        let root = json::parse(bytes).map_err(|error| Invalid(error.to_string()))?;
        walk(&root, &mut out);
    }
    // The client reads any file a shader imports as GLSL text, whatever its kind.
    if bytes.windows(10).any(|window| window == b"moj_import") {
        out.0.extend(glsl::imports(bytes));
    }
    Ok(out.0)
}

struct Out(Vec<RawRef>);

impl Out {
    fn push(&mut self, pointer: &str, node: &Node, raw: &str, role: Role) {
        self.0.push(RawRef {
            pointer: Pointer::new(pointer),
            span: node.span.clone(),
            raw: raw.into(),
            role,
        });
    }

    /// The node's own string, when it is one.
    fn string(&mut self, pointer: &str, node: Option<&Node>, role: Role) {
        if let Some(node) = node
            && let Some(text) = node.as_str()
        {
            self.push(pointer, node, text, role);
        }
    }

    fn field(&mut self, at: &str, node: &Node, name: &str, role: Role) {
        self.string(&child(at, Seg::Name(name)), node.string(name), role);
    }

    /// A member as `asString("")` reads it, when that is not empty: an unstitch region's numbers.
    fn scalar(&mut self, at: &str, node: &Node, name: &str) {
        if let Some(member) = node.get(name) {
            let text = member.as_string("");
            if !text.is_empty() {
                self.push(&child(at, Seg::Name(name)), member, &text, Role::Other);
            }
        }
    }

    fn marker_if_object(&mut self, at: &str, node: &Node) {
        if node.is_object() {
            self.push(&format!("{at}{OBJECT_MARKER}"), node, "", Role::Other);
        }
    }
}

fn model(root: &Node, out: &mut Out) {
    out.field("", root, "parent", Role::ModelParent);
    for member in root.path("textures").members() {
        let at = child("/textures", Seg::Name(&member.name));
        let (at, node, text) = if member.node.is_object() {
            match member.node.string("sprite") {
                Some(sprite) => (
                    child(&at, Seg::Name("sprite")),
                    sprite,
                    sprite.as_string(""),
                ),
                None => continue,
            }
        } else {
            (at, &member.node, member.node.as_string(""))
        };
        if !text.is_empty() && !text.starts_with('#') {
            out.push(&at, node, &text, Role::TextureSlot);
        }
    }
    for (index, entry) in root.path("overrides").items().iter().enumerate() {
        out.field(
            &child("/overrides", Seg::Index(index)),
            entry,
            "model",
            Role::Other,
        );
    }
}

/// The whole tree, so every branch of select, condition, range_dispatch and composite is read.
fn item(node: &Node, at: &str, out: &mut Out) {
    if node.is_object() {
        match node_type(node) {
            "model" => out.field(at, node, "model", Role::Other),
            "special" => {
                out.field(at, node, "base", Role::Other);
                let special = node.path("model");
                if special.string("type").is_some() && special.string("texture").is_some() {
                    let special_at = child(at, Seg::Name("model"));
                    out.field(&special_at, special, "type", Role::Other);
                    out.field(&special_at, special, "texture", Role::Other);
                }
            }
            _ => {}
        }
    }
    for (seg, child_node) in node.children_at() {
        item(child_node, &child(at, seg), out);
    }
}

fn blockstate(node: &Node, at: &str, out: &mut Out) {
    if node.is_object() {
        out.field(at, node, "model", Role::Other);
    }
    for (seg, child_node) in node.children_at() {
        blockstate(child_node, &child(at, seg), out);
    }
}

/// What the atlas decoding and its references read. A non-string `sprite` of a single source is a
/// [`Role::Other`] token: it is there (no implicit sprite name) but names nothing.
fn atlas(root: &Node, out: &mut Out) {
    let sources = root.path("sources");
    out.marker_if_object("/sources", sources);
    for (seg, source) in sources.children_at() {
        let at = child("/sources", seg);
        out.field(&at, source, "type", Role::Other);
        match node_type(source) {
            "directory" => {
                out.field(&at, source, "source", Role::Other);
                out.field(&at, source, "prefix", Role::Other);
            }
            "single" => {
                out.field(&at, source, "resource", Role::Other);
                match source.get("sprite") {
                    Some(sprite) if sprite.as_str().is_some() => {
                        out.field(&at, source, "sprite", Role::Sprite);
                    }
                    Some(sprite) => {
                        out.push(&child(&at, Seg::Name("sprite")), sprite, "", Role::Other)
                    }
                    None => {}
                }
            }
            "filter" => {
                let pattern = source.path("pattern");
                let pattern_at = child(&at, Seg::Name("pattern"));
                out.field(&pattern_at, pattern, "namespace", Role::Other);
                out.field(&pattern_at, pattern, "path", Role::Other);
            }
            "unstitch" => {
                out.field(&at, source, "resource", Role::Other);
                out.scalar(&at, source, "divisor_x");
                out.scalar(&at, source, "divisor_y");
                let regions = source.path("regions");
                let regions_at = child(&at, Seg::Name("regions"));
                out.marker_if_object(&regions_at, regions);
                for (seg, region) in regions.children_at() {
                    let region_at = child(&regions_at, seg);
                    out.field(&region_at, region, "sprite", Role::Sprite);
                    for field in ["x", "y", "width", "height"] {
                        out.scalar(&region_at, region, field);
                    }
                }
            }
            "paletted_permutations" => {
                let textures = source.path("textures");
                let textures_at = child(&at, Seg::Name("textures"));
                out.marker_if_object(&textures_at, textures);
                for (seg, texture) in textures.children_at() {
                    out.string(&child(&textures_at, seg), Some(texture), Role::Other);
                }
                out.field(&at, source, "palette_key", Role::Other);
                let permutations_at = child(&at, Seg::Name("permutations"));
                for member in source.path("permutations").members() {
                    out.string(
                        &child(&permutations_at, Seg::Name(&member.name)),
                        Some(&member.node),
                        Role::Other,
                    );
                }
                out.field(&at, source, "separator", Role::Other);
            }
            _ => {}
        }
    }
}

fn equipment(root: &Node, out: &mut Out) {
    for layer in root.path("layers").members() {
        let layer_at = child("/layers", Seg::Name(&layer.name));
        for (index, entry) in layer.node.items().iter().enumerate() {
            out.field(
                &child(&layer_at, Seg::Index(index)),
                entry,
                "texture",
                Role::Other,
            );
        }
    }
}

fn font(root: &Node, out: &mut Out) {
    for (index, provider) in root.path("providers").items().iter().enumerate() {
        let at = child("/providers", Seg::Index(index));
        let field = match node_type(provider) {
            "bitmap" | "ttf" => "file",
            "reference" => "id",
            "unihex" => "hex_file",
            _ => continue,
        };
        if provider.string(field).is_some() {
            out.field(&at, provider, "type", Role::Other);
            out.field(&at, provider, field, Role::Other);
        }
    }
}

/// Each event `sounds.json` defines (a name token at its own pointer), then its sounds; an object sound's `type`
/// comes before its name.
fn sounds(root: &Node, out: &mut Out) {
    for event in root.members() {
        let event_at = child("", Seg::Name(&event.name));
        out.0.push(RawRef {
            pointer: Pointer::new(&event_at),
            span: event.name_span.clone(),
            raw: event.name.clone(),
            role: Role::Other,
        });
        let sounds_at = child(&event_at, Seg::Name("sounds"));
        for (index, sound) in event.node.path("sounds").items().iter().enumerate() {
            let at = child(&sounds_at, Seg::Index(index));
            if sound.is_object() {
                out.field(&at, sound, "type", Role::Other);
                let name_at = child(&at, Seg::Name("name"));
                match sound.string("name") {
                    Some(name) => out.push(&name_at, name, &name.as_string(""), Role::SoundName),
                    None => out.push(&name_at, sound, "", Role::SoundName),
                }
            } else {
                out.push(&at, sound, &sound.as_string(""), Role::SoundName);
            }
        }
    }
}

fn post_effect(root: &Node, out: &mut Out) {
    for (index, pass) in root.path("passes").items().iter().enumerate() {
        let at = child("/passes", Seg::Index(index));
        out.field(&at, pass, "vertex_shader", Role::Other);
        out.field(&at, pass, "fragment_shader", Role::Other);
        let inputs_at = child(&at, Seg::Name("inputs"));
        for (input_index, input) in pass.path("inputs").items().iter().enumerate() {
            out.field(
                &child(&inputs_at, Seg::Index(input_index)),
                input,
                "location",
                Role::Other,
            );
        }
    }
}

fn sprites(root: &Node, field: &str, out: &mut Out) {
    let at = child("", Seg::Name(field));
    for (index, sprite) in root.path(field).items().iter().enumerate() {
        out.push(
            &child(&at, Seg::Index(index)),
            sprite,
            &sprite.as_string(""),
            Role::Sprite,
        );
    }
}
