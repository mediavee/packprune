# Configuration

- [The config file](#the-config-file)
- [Profiles](#profiles)
- [Lossy passes](#lossy-passes)
- [Server sources](#server-sources)
- [Zip layout](#zip-layout)
- [Cache, budgets, review](#cache-budgets-review)
- [Reports](#reports)

## The config file

Pass it with `--config packprune.toml`. Without one, give `--client-jar` (and optionally `--asset-index`) on
the command line.

```toml
version = 2

[vanilla]
client_jar = "minecraft/client-26.2.jar"
asset_index = "minecraft/assets/indexes/32.json"

[sources]
nexo = "plugins/Nexo"
configs = ["plugins"]

[profiles.default]
passes = "lossless"
```

- Paths are relative to the config file. Unknown keys are refused.
- `client_jar` is the vanilla client of the pack's Minecraft version; analysis needs it.
- `asset_index` is the launcher's index (`assets/indexes/<id>.json`); it names vanilla sounds.
- `packprune schema` prints the [JSON schema](packprune.schema.json), for editor completion.

## Profiles

A profile picks the passes, threads and zip settings. `default` is used unless `--profile` names another.

```toml
[profiles.default]
passes = "lossless"            # or "none" (prune only), or pass names
zip = { mode = "conformant" }

[profiles.fast]
inherits = "default"
threads = 4
zip = { mode = "conformant", effort = 1 }
```

`--passes`, `--threads` and `--profile` override the file. `[[profiles.<name>.override]]` tables set
optimiser options for a glob; the first matching table wins.

## Lossy passes

Nothing lossy runs unless you add a table for it, with the files it may touch:

```toml
[profiles.default.lossy.png-quantize]
globs = ["assets/example/textures/**"]
max_colors = 128
min_ssim = 0.98
```

A file that misses its threshold stays as it was, and the report lists it.

| Pass | What it does | Options (default) |
|---|---|---|
| `png-quantize` | reduces PNG colours to a palette | `max_colors` (256), `quality` (100), `dithering` (1), `effort` (15) |
| `png-alpha` | rewrites the colour of fully transparent pixels | `effort` (15) |
| `png-single-color` | downsizes single-colour textures | `effort` (15) |
| `texel-downscale` | downscales textures denser than `ceiling` texels per model unit | `ceiling` (4) |
| `uv-crop-particles` | also crops model particle sprites | none |
| `prune-unnamed-sprites` | removes unnamed single-atlas sprites | none |
| `overlay-floor` | removes overlays older than a format | `oldest_format` (required) |
| `ogg-transcode` | re-encodes Ogg Vorbis, kept only when smaller | `quality` (0.6) |
| `sound-mono` | downmixes stereo sounds to mono | `quality` (0.6) |
| `sound-rate` | resamples sounds above `cap` Hz | `cap` (48000), `quality` (0.6) |
| `audio-to-ogg` | converts MP3, AAC, FLAC and WAV sounds to Ogg | `quality` (0.6) |
| `stream-sounds` | marks sounds longer than `seconds` as streamed | `seconds` (5) |

Quality thresholds, all optional:

| Option | Applies to | Meaning |
|---|---|---|
| `min_ssim` | images | structural similarity, -1 to 1 |
| `max_delta` | images | largest channel difference, 0 to 255 |
| `max_lsd` | audio | log-spectral distance, dB |
| `min_snr` | audio | signal-to-noise ratio, dB |

Audio `quality` is the Vorbis quality, -2 to 10. `oldest_format = <n>` on a profile is a shorthand for
`overlay-floor` on every path.

## Server sources

Some resources are used only by the server: an item a plugin gives, a font in a chat message. Sources read the
server's files so packprune keeps them. The report lists each usage with its `file:line`.

```toml
[sources]
keep = "keep.yml"
nexo = "plugins/Nexo"
configs = ["plugins"]
datapacks = ["world/datapacks"]
```

On the command line, `--source <id>=<path>` and `--keep <file>` replace **all** configured sources.

### `keep`: a hand-written list

```yaml
version: 1
keep:
  - path: "assets/example/shaders/**"
    why: "loaded by a plugin"
  - item_model: "example:furniture/*"
    why: "placed furniture"
```

Each entry has one selector and a `why`. Selectors: `path`, `namespace`, `item_model`, `model`, `texture`,
`font`, `equipment`, `sound_event`. In paths, `*` stays in a folder and `**` crosses folders.

### `nexo`

Reads Nexo's items, glyphs, sounds, settings and recipes. Unresolved templates are reported, never guessed.

- Point it at the built pack: assets Nexo generates may be missing from its source folder.
- Item definitions are kept by default. `prune_unused = ["nexo:items/example/**"]` keeps the selected ones
  only when something else uses them.
- World and player data are not read: an item that exists only there needs a `keep` entry.
- Exclude Nexo's own folder from `configs`, or its files count as uses.

### `configs`: plugin configurations

Scans `.yml`, `.yaml`, `.json` and `.properties` files. It recognises:

- keys such as `item_model`, `font`, `tooltip_style` and sound keys ([generic profile](../crates/packprune-sources/data/generic.json));
- MiniMessage `<font:…>` and `<sprite:…>` tags and JSON text components;
- namespaced IDs under other keys.

For plugin-specific keys, add a profile in `<scanned folder>/packprune-profiles/<name>.json`:

```json
{
  "version": 1,
  "name": "shops",
  "why": "Shop menus name Nexo items and GUI sprites",
  "files": ["Shops/**"],
  "rules": [
    {"key": "material", "kind": "item_definition", "id": "nexo:{value}"},
    {"key": "icon", "under": "display", "kind": "sprite", "atlas": "minecraft:gui", "id": "icons/{path}"}
  ]
}
```

A rule reads `key` at any depth (`under` names its parent). `kind` is `item_definition`, `model`, `sprite`,
`font`, `sound_event`, `equipment` or `tooltip_style`. `id` can use `{id}`, `{namespace}`, `{path}` and
`{value}`. Optional fields: `exclude`, `placeholders`, `quiet`.

### `datapacks`

Takes a data pack, a zip, or a folder of packs. It reads asset references from paintings, trims, banners,
jukebox songs, instruments, mob and sound variants, advancements and dialogs
([rules](../crates/packprune-sources/data/datapack-rules.json)). Loot tables are not read.

## Zip layout

| `zip.mode` | Size | Opens in |
|---|---|---|
| `lean` (default for `optimise`) | smallest | Minecraft, any `java.util.zip.ZipFile` reader |
| `conformant` | standard | every zip tool |

`lean` shares one local record between identical files and leaves local names empty; the central directory
stays complete. Tested readers:

| Reader | `lean` |
|---|---|
| Minecraft 26.2 and 1.16.5 clients; `java.util.zip.ZipFile` on Java 8, 17, 21, 25 | loads |
| Nexo 1.29 upload and hash, Paper | unaffected: they do not open the zip |
| Java `ZipInputStream` | empty entry names |
| Python `zipfile`, `unzip`, 7-Zip | refuse it or warn |

Use `conformant` when anything other than the game reads the zip.

## Cache, budgets, review

```toml
[cache]
dir = ".packprune-cache"      # PACKPRUNE_CACHE overrides it
max_bytes = 1_073_741_824

[budgets]
zip_bytes = 12_000_000
atlas_gpu_mib = 128
font_mib = 64
```

- **Cache:** content-addressed, so an unchanged file is not optimised again.
- **Budgets:** `optimise` and `check` exit 1 when one is exceeded. Atlas and font budgets are client memory
  estimates.
- **Review:** `--expect-report report.json` refuses a run whose inputs or planned removals differ from a report
  you reviewed.

## Reports

`optimise` writes `<output>.report/`; `report --out <dir>` writes one without optimising.

| File | Content |
|---|---|
| `report.md` | readable summary: findings, removals, memory, budgets, loss |
| `report.json` | orphans, missing references, warnings, input hashes |
| `graph.json` | every reference: why each file is kept |
| `passes.json` | what each pass changed, measured loss |
| `run.json` | output, removals by reason, costs |

Check the warnings and unverified views before treating an orphan as safe to delete.
