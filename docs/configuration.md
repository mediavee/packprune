# Configuration

`packprune.toml` uses config format `version = 2` and rejects unknown keys. Paths are relative to the config
file. `packprune schema` and the committed [JSON schema](packprune.schema.json) describe its structure; lossy
pass options are checked by each pass when the config loads (table below).

```toml
version = 2

[vanilla]
client_jar = "minecraft/client-26.2.jar"
asset_index = "minecraft/assets/indexes/32.json"

[sources]
keep = "keep.yml"
nexo = "plugins/Nexo"
configs = ["plugins"]
datapacks = ["world/datapacks"]

[profiles.default]
passes = "lossless"
zip = { mode = "conformant" }

[profiles.fast]
inherits = "default"
threads = 4
zip = { mode = "conformant", effort = 1 }

[cache]
dir = ".packprune-cache"
max_bytes = 1_073_741_824

[budgets]
zip_bytes = 12_000_000
atlas_gpu_mib = 128
font_mib = 64
```

- **Vanilla.** Analysis needs the client jar of the target version; the launcher asset index supplies vanilla
  sound names. `--client-jar` and `--asset-index` override them.
- **Profiles.** `--profile`, `--threads` and `--passes` override the profile's choices. `--passes none` still
  prunes. Per-glob `[[profiles.<p>.override]]` tables set file optimiser options; the first matching table that
  sets options for a kind wins.
- **Zip.** Without `zip.mode`, `optimise` writes `lean`, which only central-directory readers such as the
  Minecraft client accept. Use `conformant` for general zip tools ([reader table](install.md#opt-into-the-lean-zip-layout)).
- **Cache.** Content addressed; `PACKPRUNE_CACHE` overrides its directory.
- **Budgets.** Exceeding one makes `optimise` and `check` exit 1.
- **Review gate.** `--expect-report report/report.json` refuses a run whose analysis inputs or planned removals
  differ from an earlier report.

## Lossy passes

Lossy work is opt-in per pass, with explicit globs:

```toml
[profiles.default.lossy.texel-downscale]
globs = ["assets/example/textures/**"]
ceiling = 4
min_ssim = 0.98
```

A candidate that fails its threshold keeps the original file and is listed in the report. Omitted thresholds
impose no limit. Names and options are case-sensitive.

| Name | Options (defaults and limits) |
|---|---|
| `uv-crop-particles` | none; also crops model particle sprites |
| `prune-unnamed-sprites` | none; removes unnamed single-atlas sprites |
| `texel-downscale` | `ceiling = 4` (> 0), optional `min_ssim`, `max_delta` |
| `overlay-floor` | required `oldest_format` (0–2,147,483,647); removes overlays below it |
| `stream-sounds` | `seconds = 5` (≥ 0); globs select the `sounds.json` files to edit |
| `sound-mono` | `quality = 0.6`, optional `max_lsd`, `min_snr` |
| `sound-rate` | `cap = 48000` (> 0 Hz), `quality = 0.6`, optional `max_lsd`, `min_snr` |
| `ogg-transcode` | `quality = 0.6`, optional `max_lsd`, `min_snr` |
| `audio-to-ogg` | `quality = 0.6`, optional `max_lsd`, `min_snr`; converts non-Ogg audio |
| `png-alpha` | `effort = 15` (0–255), optional `min_ssim`, `max_delta` |
| `png-quantize` | `effort = 15`, `max_colors = 256` (2–256), `quality = 100` (0–100), `dithering = 1` (0–1), optional `min_ssim`, `max_delta` |
| `png-single-color` | `effort = 15`, optional `min_ssim`, `max_delta` |

`min_ssim` is in [-1, 1]; `max_delta` is a channel difference in [0, 255]. Audio `quality` is Vorbis quality in
[-2, 10]; `max_lsd` is a log-spectral distance and `min_snr` a signal-to-noise ratio, both in dB.
`[profiles.<p>] oldest_format = <n>` is a shorthand for `overlay-floor` on every path; do not combine both.

## Usage sources

Server-side data names resources the pack alone cannot reveal. Sources turn it into roots; the report records
each usage with its provenance (`file:line`) and whether it matched. Built-in source IDs: `keep`, `nexo`,
`configs`, `datapacks`. Any `--source ID=PATH` or `--keep` flag replaces **all** configured sources: repeat every
one you need.

### Keep-list

```yaml
version: 1
keep:
  - path: "assets/example/shaders/**"
    why: "loaded by a plugin"
  - item_model: "example:furniture/*"
    why: "placed furniture"
  - namespace: "vendor"
    why: "vendor pack, keep whole"
```

Each entry has exactly one selector and a non-empty `why`. Selectors: `path`, `namespace`, `item_model`,
`model`, `texture`, `font`, `equipment`, `sound_event`. In paths `*` stays within a folder and `**` crosses
folders; in ID selectors `*` matches across `/`. Unmatched entries are reported.

### Nexo

Reads `items/**.yml`, `glyphs/**.yml`, `sounds.yml`, `settings.yml` and `recipes/<type>/**.yml`: the models,
textures, equipment, sounds, fonts and generated item definitions they name. Templates and placeholders that
cannot be resolved are reported, never guessed. Point packprune at the built pack: assets Nexo generates at build
time may be absent from its source folder.

Item definitions are roots by default. `prune_unused = ["nexo:items/example/**"]` (or `--prune-unused`) makes
the selected declarations conditional: a definition stays only when another usage names it, and files only it
reaches are dropped. A run is refused if no non-declaration usage names any item definition, since it would have
no evidence to keep items. World, player and database data is not read: an item that exists only there needs a
keep-list entry. Exclude Nexo's own configs from the `configs` scan, or they count as uses.

### Configs

`configs` takes a plugins folder or one file and scans `.yml`, `.yaml`, `.json` and `.properties`, skipping
resource-pack folders. It reads IDs under keys of the built-in
[generic profile](../crates/packprune-sources/data/generic.json) (`item_model`, `tooltip_style`, `font`,
`equippable.asset_id`, sound keys), MiniMessage `<font:…>` and `<sprite:…>` tags, JSON text components, and
namespaced IDs under other keys as quiet item usages. Invalid files are refused with their name.

Plugin-specific keys go in `<scanned folder>/packprune-profiles/*.json`:

```json
{
  "version": 1,
  "name": "shops",
  "why": "Shop menus name bare Nexo item ids and GUI sprites",
  "files": ["Shops/**"],
  "exclude": ["Shops/cache/**", "Nexo/**"],
  "placeholders": ["@@"],
  "rules": [
    {"key": "material", "kind": "item_definition", "id": "nexo:{value}"},
    {"key": "icon", "under": "display", "kind": "sprite", "atlas": "minecraft:gui", "id": "icons/{path}"}
  ]
}
```

A rule reads a string under `key` at any depth (`under` names its parent); keys normalise snake, kebab and
camel case. `kind` is `item_definition`, `model`, `sprite`, `font`, `sound_event`, `equipment` or
`tooltip_style`. `id` supports `{id}` (default), `{namespace}`, `{path}` and `{value}`. `exclude` applies to every
profile's scan; `quiet: true` hides unmatched usages.

### Datapacks

Takes a data-pack folder, a zip, or a folder of packs such as `world/datapacks`. The embedded
[registry rules](../crates/packprune-sources/data/datapack-rules.json) read asset references from painting,
trim, banner, jukebox, instrument, mob variant and sound variant registries, advancement backgrounds and dialog
text components. Other registries, including loot tables, are not read.

## Reports

`optimise` writes `<output>.report/`; `report --out <dir>` writes to the chosen directory.

| File | Content |
|---|---|
| `report.md` | findings, missing references, orphans, usage matches, atlas and font memory, budgets, pass results, measured loss |
| `report.json` | structured analysis and input identities, for automation and `--expect-report` |
| `graph.json` | roots, references and reachability: why a resource is kept |
| `passes.json` | changes and refusals per pass |
| `run.json` | run results, costs, budgets and loss measurements |

Atlas and font memory are client cost estimates, distinct from zip size. Read warnings and unverified views
before treating an orphan as safe to remove.
