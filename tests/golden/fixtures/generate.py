#!/usr/bin/env python3
"""Writes the fixture packs of the golden rows (../rows), one per pass family, built so that each pass its rows name
changes a file. Run it, then `../record`, after changing a fixture."""
import json, os, shutil, struct, zlib

ROOT = os.path.dirname(os.path.abspath(__file__))
SOUNDS = os.path.join(ROOT, "../../fixtures/sounds")


def png(w, h, px):
    """An 8-bit RGBA PNG whose pixel (x, y) is px(x, y) as 0xRRGGBBAA."""
    raw = b"".join(b"\0" + b"".join(struct.pack(">I", px(x, y)) for x in range(w)) for y in range(h))

    def chunk(kind, data):
        return struct.pack(">I", len(data)) + kind + data + struct.pack(">I", zlib.crc32(kind + data))

    return (b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", w, h, 8, 6, 0, 0, 0))
            + chunk(b"IDAT", zlib.compress(raw, 9)) + chunk(b"IEND", b""))


def grad(base):
    """Opaque, every texel a different colour."""
    return lambda x, y: ((base + (x << 16) + (y << 8)) & 0xFFFFFF00) | 0xFF


class Pack:
    def __init__(self, name, mcmeta=None):
        self.dir = os.path.join(ROOT, name)
        shutil.rmtree(self.dir, ignore_errors=True)
        self.text("pack.mcmeta", mcmeta or '{"pack": {"min_format": 88, "max_format": 88, "description": "golden"}}\n')

    def put(self, path, data):
        full = os.path.join(self.dir, path)
        os.makedirs(os.path.dirname(full), exist_ok=True)
        with open(full, "wb") as f:
            f.write(data)

    def text(self, path, text):
        self.put(path, text.encode())

    def png(self, path, w, h, px):
        self.put(path, png(w, h, px))

    def item(self, name, model):
        self.text(f"assets/fx/items/{name}.json", json.dumps({"model": {"type": "minecraft:model", "model": model}}) + "\n")

    def model(self, name, body):
        """The model fx:item/<name> and the item definition that shows it."""
        self.text(f"assets/fx/models/item/{name}.json", body + "\n")
        self.item(name, f"fx:item/{name}")


def cube(faces, to="[16, 16, 16]"):
    return '{"from": [0, 0, 0], "to": %s, "faces": {%s}}' % (to, faces)


# models: degenerate-faces, identical-models, shared-display
p = Pack("models")
p.png("assets/fx/textures/item/a.png", 4, 4, grad(0x10000000))
p.png("assets/fx/textures/item/b.png", 4, 4, grad(0x80800000))
p.model("flat", """{"textures": {"a": "fx:item/a"}, "elements": [
 {"from": [0, 0, 8], "to": [16, 16, 8], "faces": {"north": {"texture": "#a"}, "south": {"texture": "#a"}, "up": {"texture": "#a"}, "east": {"texture": "#a"}}},
 {"from": [4, 0, 0], "to": [4, 0, 16], "faces": {"up": {"texture": "#a"}, "west": {"texture": "#a"}}},
 {"from": [1, 1, 1], "to": [2, 2, 2], "faces": {"down": {"texture": "#a"}}}]}""")
CUBE = '"elements": [{"from": [0, 0, 0], "to": [16, 16, 16], "faces": {"north": {"texture": "#a"}, "south": {"texture": "#a"}}}]'
p.model("twin_a", '{"textures": {"a": "fx:item/a"}, %s}' % CUBE)
p.model("twin_b", '{ "elements": [{"faces": {"south": {"texture": "#a"}, "north": {"texture": "#a"}}, "to": [16, 16, 16], "from": [0, 0, 0]}],\n  "textures": {"a": "fx:item/a"} }')
p.model("other", '{"textures": {"a": "fx:item/b"}, %s}' % CUBE)
p.model("child", '{"parent": "fx:item/twin_b"}')
DISPLAY = ('"display": {"gui": {"rotation": [30, 225, 0], "translation": [0, 1, 0], "scale": [0.625, 0.625, 0.625]},'
           ' "head": {"rotation": [0, 90, 0], "translation": [0, 4, 0], "scale": [1.2, 1.2, 1.2]},'
           ' "thirdperson_righthand": {"rotation": [75, 45, 0], "translation": [0, 2.5, 0], "scale": [0.375, 0.375, 0.375]}}')
for i, t in enumerate(["a", "b", "a", "b"]):
    p.model(f"shown_{i}", '{"textures": {"a": "fx:item/%s"}, "elements": [%s], %s}' % (
        t, cube('"north": {"texture": "#a"}', to="[16, 16, %d]" % (i + 1)), DISPLAY))

# textures: identical-sprites, animation-frames
p = Pack("textures")
for name in ["s1", "s2"]:
    p.png(f"assets/fx/textures/item/{name}.png", 4, 4, grad(0x20400000))
p.png("assets/fx/textures/item/s3.png", 4, 4, grad(0x60400000))
strip = [0xFF0000FF, 0x00FF00FF, 0xFF0000FF, 0x0000FFFF, 0x00FF00FF]
p.png("assets/fx/textures/item/anim.png", 4, 4 * len(strip), lambda x, y: strip[y // 4])
p.text("assets/fx/textures/item/anim.png.mcmeta", '{"animation": {"frametime": 2, "frames": [0, 1, {"index": 2, "time": 7}, 3, 4]}}\n')
p.png("assets/fx/textures/item/blend.png", 4, 12, lambda x, y: [0xFF0000FF, 0xFF0000FF, 0x00FF00FF][y // 4])
p.text("assets/fx/textures/item/blend.png.mcmeta", '{"animation": {"interpolate": true}}\n')
faces = ", ".join('"%s": {"texture": "#%s"}' % (d, s) for d, s in zip(["down", "up", "north", "south", "west", "east"], ["s1", "s2", "s3", "anim", "blend", "s1"]))
p.model("box", '{"textures": {"s1": "fx:item/s1", "s2": "fx:item/s2", "s3": "fx:item/s3", "anim": "fx:item/anim", "blend": "fx:item/blend"}, "elements": [%s]}' % cube(faces))

# crop: uv-crop, uv-crop-particles
p = Pack("crop")
p.png("assets/fx/textures/item/skin.png", 16, 16, grad(0x00000000))
p.png("assets/fx/textures/item/hide.png", 16, 16, grad(0x40000000))
p.png("assets/fx/textures/item/wrap.png", 16, 16, grad(0x80000000))
p.model("box", '{"textures": {"skin": "fx:item/skin"}, "elements": [%s]}' % cube(
    '"north": {"uv": [4, 4, 8, 12], "texture": "#skin"}, "south": {"uv": [12, 0, 4, 4], "texture": "#skin"}, "up": {"uv": [6, 2, 7, 3], "rotation": 90, "texture": "#skin"}'))
p.model("dusty", '{"textures": {"hide": "fx:item/hide", "particle": "#hide"}, "elements": [%s]}' % cube('"north": {"uv": [0, 0, 4, 4], "texture": "#hide"}'))
p.model("flat", '{"parent": "minecraft:item/generated", "textures": {"layer0": "fx:item/wrap"}}')
p.model("wrapped", '{"textures": {"w": "fx:item/wrap"}, "elements": [%s]}' % cube('"north": {"uv": [0, 0, 4, 4], "texture": "#w"}'))

# overlays: overlay-merge, overlay-floor
entries = [
    {"directory": "a", "formats": [34, 41]},
    {"directory": "b", "formats": [42, 45]},
    {"directory": "c", "formats": [46, 54], "min_format": 46, "max_format": 54},
    {"directory": "d", "formats": [55, 62]},
    {"directory": "cur", "min_format": 84, "max_format": 88},
    {"directory": "next", "formats": [89, 199], "min_format": 89, "max_format": 199},
]
p = Pack("overlays", json.dumps({"pack": {"min_format": 34, "max_format": 88, "description": "golden"}, "overlays": {"entries": entries}}) + "\n")
p.text("assets/fx/lang/en_us.json", '{"item.fx.box": "Box"}\n')
for d, v in [("a", "X"), ("b", "X"), ("c", "X"), ("d", "Y"), ("cur", "C"), ("next", "N")]:
    p.text(f"{d}/assets/fx/lang/en_us.json", '{"item.fx.box": "Box %s"}\n' % v)
p.png("assets/fx/textures/item/gem.png", 4, 4, grad(0x30300000))
p.model("gem", '{"parent": "minecraft:item/generated", "textures": {"layer0": "fx:item/gem"}}')
p.text("b/assets/fx/models/item/gem.json", '{"parent": "minecraft:item/generated", "textures": {"layer0": "fx:item/gem_old"}}\n')
p.png("b/assets/fx/textures/item/gem_old.png", 4, 4, grad(0x90300000))

# names: short-names, atlas-directory
U = "00000000-0000-4000-8000-0000000000%s"
p = Pack("names")
p.png(f"assets/fx/textures/{U % 'a1'}.png", 4, 4, grad(0x11000000))
p.png(f"assets/fx/textures/{U % 'b2'}.png", 4, 4, grad(0x22000000))
p.text(f"assets/fx/models/{U % '0a'}.json", '{"textures": {"a": "fx:%s", "b": "fx:%s"}, "elements": [%s]}\n' % (
    U % "a1", U % "b2", cube('"north": {"texture": "#a"}, "south": {"texture": "#b"}')))
p.item("box", "fx:" + U % "0a")
p.png("assets/fx/textures/g/one.png", 4, 4, grad(0x33000000))
p.png("assets/fx/textures/g/two.png", 4, 4, grad(0x44000000))
p.model("grouped", '{"textures": {"a": "fx:g/one", "b": "fx:g/two"}, "elements": [%s]}' % cube('"north": {"texture": "#a"}, "south": {"texture": "#b"}'))
p.text("assets/minecraft/atlases/items.json", json.dumps({"sources": [
    {"type": "single", "resource": "fx:" + U % "a1"}, {"type": "single", "resource": "fx:" + U % "b2"},
    {"type": "single", "resource": "fx:g/one"}, {"type": "single", "resource": "fx:g/two"}]}) + "\n")
p.text("assets/fx/sounds.json", '{"boom": {"sounds": ["fx:%s"]}}\n' % (U % "c3"))
with open(os.path.join(SOUNDS, "blip_22k_20ms.ogg"), "rb") as f:
    p.put(f"assets/fx/sounds/{U % 'c3'}.ogg", f.read())

# json: short-json
p = Pack("json")
p.png("assets/fx/textures/item/gem.png", 4, 4, grad(0x55000000))
p.model("gem", '{"parent": "minecraft:item/generated", "textures": {"layer0": "minecraft:item/paper", "layer1": "fx:item/gem"},'
        ' "display": {"gui": {"rotation": [30.0, 45.50, -0.0], "scale": [0.6250, 1.0e0, 1.0]}}}')
p.text("assets/fx/items/vanilla_like.json", '{"model": {"type": "minecraft:model", "model": "minecraft:item/paper", "tints": [{"type": "minecraft:constant", "value": -1}]}}\n')
p.text("assets/fx/lang/en_us.json", '{"item.fx.gem": "Gem 16.0", "x": "minecraft:stone"}\n')

# lossy: texel-downscale, prune-unnamed-sprites
p = Pack("lossy")
p.png("assets/fx/textures/item/big.png", 32, 32, lambda x, y: ((x * 8) << 24) | ((y * 8) << 16) | (((x * 7 + y * 13) % 256) << 8) | 0xFF)
p.model("big", '{"textures": {"a": "fx:item/big"}, "elements": [%s]}' % cube('"north": {"uv": [0, 0, 16, 16], "texture": "#a"}', to="[4, 4, 4]"))
p.png("assets/fx/textures/custom/used.png", 4, 4, grad(0x66000000))
p.png("assets/fx/textures/custom/orphan.png", 4, 4, grad(0x77000000))
p.model("used", '{"textures": {"a": "fx:custom/used"}, "elements": [%s]}' % cube('"north": {"texture": "#a"}'))
p.text("assets/minecraft/atlases/items.json", json.dumps({"sources": [
    {"type": "single", "resource": "fx:custom/used"},
    {"type": "single", "resource": "fx:custom/orphan", "sprite": "fx:named/orphan"}]}) + "\n")

# sounds: stream-sounds, sound-mono, sound-rate
p = Pack("sounds")
os.makedirs(os.path.join(p.dir, "assets/fx/sounds"), exist_ok=True)
for src, dst in [("other_44k_6s.ogg", "long"), ("mono_22k_1s.ogg", "short"), ("blip_22k_20ms.ogg", "blip"), ("stereo_96k_1s.ogg", "fast")]:
    shutil.copy(os.path.join(SOUNDS, src), os.path.join(p.dir, f"assets/fx/sounds/{dst}.ogg"))
p.text("assets/fx/sounds.json", json.dumps({
    "long": {"sounds": ["fx:long", {"name": "fx:long", "stream": False, "volume": 0.5}]},
    "short": {"sounds": ["fx:short"]},
    "blip": {"sounds": ["fx:blip"]},
    "fast": {"sounds": [{"name": "fx:fast", "pitch": 1.5}]},
}, indent=2) + "\n")

# sources: what only a usage source keeps (rows `sources.*`): a nexo parent model, sound and item,
# an entity variant a data pack names, and the nexo items --prune-unused drops unless a config uses them.
p = Pack("sources/pack")
p.png("assets/fx/textures/item/gem.png", 4, 4, grad(0x12340000))
p.text("assets/fx/models/item/blade.json", '{"parent": "minecraft:item/generated", "textures": {"layer0": "fx:item/gem"}}\n')
for gem in ["used_gem", "spare_gem"]:
    p.text(f"assets/fx/models/item/{gem}.json", '{"parent": "fx:item/blade"}\n')
    p.text(f"assets/nexo/items/{gem}.json", json.dumps({"model": {"type": "minecraft:model", "model": f"fx:item/{gem}"}}) + "\n")
with open(os.path.join(SOUNDS, "blip_22k_20ms.ogg"), "rb") as f:
    p.put("assets/fx/sounds/ui/click.ogg", f.read())
p.png("assets/fx/textures/entity/cat/ginger.png", 8, 8, grad(0x56780000))
p.text("../Nexo/items/gems.yml", "\n".join(
    f"{gem}:\n  material: EMERALD\n  Pack:\n    model: fx:item/{gem}\n    parent_model: fx:item/blade\n" for gem in ["used_gem", "spare_gem"]))
p.text("../Nexo/sounds.yml", "settings:\n  generate_sounds: true\n\nsounds:\n  - id: fx:ui.click\n    sound: fx:ui/click.ogg\n")
p.text("../plugins/Shop/shop.yml", 'menu:\n  items:\n    - item-model: "nexo:used_gem"\n')
p.text("../datapacks/data/fx/cat_variant/ginger.json", '{"asset_id": "fx:entity/cat/ginger", "spawn_conditions": []}\n')
