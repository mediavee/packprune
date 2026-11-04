# packprune architecture

packprune is a Rust workspace that builds one binary. It reads a Minecraft Java resource pack, drops what the
client never loads, rewrites the rest into fewer bytes, and proves the player sees the same pack. This page
covers what each crate owns, how a run flows, and the rules the code keeps.

## Crates

Every crate lives under `crates/`. Only `packprune-cli` is a binary. The libraries return typed errors and
never print. `scripts/check` enforces the dependency edges below, whatever the dependency kind: a crate may
depend only on the crates listed for it, and a new crate must be added to that table.

| Crate | Owns | May depend on |
|---|---|---|
| `packprune-zip` | zip reader with input limits, deterministic writer, compression backends, zip layouts | none |
| `packprune-codec` | single-file byte transforms: PNG, audio (Ogg, Vorbis, non-Ogg decode), loss metrics (SSIM, LSD, SNR), JSON, GLSL, lang and mcfunction minifiers, the properties parser, NBT | none |
| `packprune-cache` | content-addressed on-disk store, atomic writes, eviction | none |
| `packprune-config` | `packprune.toml` types and validation, the JSON schema, the PackSquash settings mapping | none |
| `packprune-pack` | `PackInput`, ids, interner, paths, `FileKind`, `pack.mcmeta`, overlays and views, vanilla index, client rules, spanned JSON | `zip` |
| `packprune-graph` | reference tokens and their attachment, reachability per view, usage model and roots, prune plan | `pack` |
| `packprune-sources` | usage sources: `keep`, `configs`, `datapacks`, `nexo` | `graph`, `pack`, `codec` |
| `packprune-oracle` | the equivalence oracle, `Remap`, the cross-run oracle memo | `pack`, `graph`, `codec` |
| `packprune-pass` | the `Pass` and `FileOptimizer` traits, `Edits`, scopes, edit application, the proof harness | `pack`, `graph`, `oracle` |
| `packprune-passes` | every pass and file optimiser, one module per family, and their registry | `pass`, `pack`, `graph`, `codec` |
| `packprune-report` | client cost (atlas GPU memory, font memory), `report.json`/`report.md`/`graph.json`, budgets | `pack`, `graph`, `codec` |
| `packprune-engine` | a run: `optimise`, `check`, `report`, `diff`, `self-test`, the drop-in run and its fallbacks, the self-check | every crate above |
| `packprune-cli` | the `packprune` binary: clap, logging, exit codes, the drop-in entry point | `engine`, `config` |

The four leaves know nothing of packs and are fuzzed and tested alone. `fuzz/` holds the cargo-fuzz targets
(not a workspace member), `tests/fixtures/` the shared small packs and vanilla stub, `vendor/` patched
OptiVorbis and saphyr-parser. Version-dependent client facts are embedded data in `crates/packprune-pack/data/`
and `crates/packprune-sources/data/`; a client version with no rules gets the newest ones and a warning.

## Commands

| Command | Does |
|---|---|
| `optimise <pack> -o <zip>` (alias `optimize`) | the run below; reports go to `<zip>.report/` |
| `check <pack>` | lint: missing references, orphans, broken models, budgets |
| `report <pack> --out <dir>` | `report.json`, `report.md`, `graph.json` |
| `diff <a> <b>` | the oracle as a tool: what changes for the player between two packs |
| `self-test` | the graph over the client's own assets |
| `squash <settings.toml>` | the PackSquash drop-in, explicit form (see [Drop-in mode](#drop-in-mode)) |
| `schema` | prints the config JSON schema (`docs/packprune.schema.json`) |

Exit codes: 0 clean; 1 findings (lint findings, a budget exceeded, `diff` differences); 2 refused (unsafe
or invalid input, config, source or limits); 3 error (a pass failure, an oracle refusal, a self-check failure,
I/O). Logs go to stderr through `tracing` (`-v`, `-q`, `PACKPRUNE_LOG`), and results go to stdout.

## Pipeline

`packprune_engine::run::optimise` runs one pack through these steps. Each state is dropped once no later
step reads it, and a failed step writes nothing.

1. **Input.** `PackInput::open` reads a directory or a zip, and every other reader goes through it too
   (self-check, drop-in, `diff`). The pack becomes a `BTreeMap<PhysPath, Entry>`, and each entry carries its
   blake3 digest. The client jar and the launcher asset index are read in parallel into the vanilla index,
   and so are the usage sources. The output path must not be the input, inside it, or around it.
2. **Layout.** `PackMeta` reads the pack's format range and its overlay entries. A `View` is one format
   interval with its ordered layers. Inside a view, a logical path (`assets/...`) resolves to the file of the
   highest layer that has it. A view whose format the given client jar matches is *verified*. An unverified
   view keeps every `minecraft:` file and every shader rather than guess.
3. **Reference graph.** Every file is tokenized into its reference-bearing tokens (`tokenize`, a pure
   function of the bytes and the kind, memoised across runs on the content digest). Each token is then
   attached to what it names in each view: namespace, folder, shader import context. Reachability runs per
   view from the roots: vanilla paths, root kinds (item definitions, fonts, `sounds.json`, equipment, lang,
   shaders, texts, waypoint styles) and usages. Texture references resolve through atlas sources only, as the
   client draws them. A file reached in any view is kept.
4. **Usage sources.** Server-side data names items, models, sprites, sounds and paths that no pack file
   references. `keep` (a YAML keep-list), `configs` (plugin YAML, JSON and properties files), `datapacks` and
   `nexo` turn that data into `Usage`s, which become roots. `prune_unused` turns declared items into roots only
   when a source uses them. Sources never read the run's own cache or report folders. `--expect-report` refuses
   the run when this analysis differs from a report an operator reviewed.
5. **Prune.** Orphans, dead block states and unused items are dropped.
6. **Lossless passes.** These run on every path, sorted by `(stage, name)`. The framework applies each
   pass's edits in place, updates the layout and the reference index incrementally, and rewrites every
   reference that a remap names by splicing bytes. A pass never edits a reference itself. If any pass changed
   something, the pack is re-pruned and the **strict oracle** compares the pruned pack with the result. One
   differing key refuses the run (`RunError::NotEquivalent`, exit 3).
7. **Lossy passes.** These run only when the profile enables them, each on its globs, after every lossless
   pass. Each glob is matched against the file's input path, followed through earlier renames. Ogg selections
   are merged so that each sound is decoded and encoded once. The pack is then re-pruned, and the oracle's
   differences are reported as the measured loss, never as a refusal.
8. **File optimisers.** Lossless optimisers run per file, in parallel, through the content cache, with
   options from the profile's per-glob overrides. Then the lossy optimisers run on their globs. Each optimiser
   checks its own output on the canonical form of its kind (pixels, PCM, JSON tokens, GLSL tokens, properties
   entries, NBT payload). Client quirks come from the formats of the views that show the file.
9. **Write.** Entries are compressed in parallel, through the cache, and the zip is staged beside the
   output.
10. **Read-back.** The staged zip is read again through `PackInput`. Three checks run at once:
    - the **self-check** analyses the written pack again: no file still removable, no reference or sprite newly
      broken, no file newly unreadable, and every path with the exact bytes the writer was given;
    - the **oracle on the zip** compares the pack the optimisers were given with what the lossless optimisers
      made of it, and any difference refuses the run. A second comparison across the lossy optimisers
      reports their loss;
    - the client **cost** of the output is computed.
11. **Report and publish.** Budgets are checked. The report files are written first: `report.json`,
    `report.md`, `graph.json`, `passes.json` and `run.json`. Then the zip is renamed over the output. The token
    and oracle memos are saved, and the cache is evicted down to `cache.max_bytes`.

## Oracle

`packprune-oracle` decides whether two packs are the same for the player. In every view that either pack
declares, it enumerates keys and resolves each one, pack over vanilla, to a descriptor. `item`, `blockstate`
and `equipment` keys resolve to baked models: parent chain, elements, display, texture slots, per-face texels
by animation frame, tint and cull. The other keys are `model` (a model a source names), `font` and `glyph`,
`sound` (files as decoded audio), `sprite` (pixels), `file` (a file read by path, compared by what its reader
keeps: pixels, samples, lang entries, GLSL tokens, JSON tokens or bytes), `pack icon` and `pack description`.

Renames compare under their new names through `Remap`. Only a 128-bit digest per key and view is kept, and
differing keys are resolved again so the difference can be located (the first 50 are located). The cross-run
memo (`oracle::memo`) is keyed on the resolution's queries, including lookups that found nothing. A digest
is reused only when every query gives the same answer in the new pack.

## Passes

The registry is `packprune_passes::passes()` (graph passes) and `packprune_passes::optimisers()` (file
optimisers). `passes = "lossless"` (the default) enables every lossless pass and optimiser, `disable` removes
names from that set, and a lossy pass runs only when its `lossy.<name>` table and globs exist. The stages, in
order: `Models`, `Textures`, `Fonts`, `Overlays`, `Names`, `Json`, `Lossy`.

Lossless graph passes:

| Pass | Stage | Does |
|---|---|---|
| `degenerate-faces` | Models | removes zero-area model faces, and elements left with none |
| `identical-models` | Models | keeps one of several models with identical content; references follow |
| `shared-display` | Models | moves repeated `display` blocks into shared generated parent models |
| `identical-sprites` | Textures | keeps one of several textures with the same pixels and `.mcmeta`; models follow |
| `animation-frames` | Textures | rewrites animated strips with each distinct frame once and a `frames` list |
| `uv-crop` | Textures | crops textures to the texels their model faces sample, every mip level unchanged |
| `overlay-merge` | Overlays | removes redundant overlay copies, merges overlays, drops those left empty |
| `short-names` | Names | renames UUID-named models, textures and sounds to short ids in a free namespace |
| `atlas-directory` | Json | turns runs of `single` atlas sources into `directory` sources |
| `short-json` | Json | writes numbers in their shortest exact form and drops a redundant `minecraft:` from ids |
| `bloat-keys` | Json | removes the root keys of a model that Blockbench or the vanilla models write and no client reads (`credit`, `groups`, `__comment`) |
| `drop-unread` | Json | removes files no client of a declared view reads: outside the declared layers' `assets/`, hidden or system files no client can name, Blockbench projects, backups, `.jsonc` sources |

Lossless file optimisers (their names are also the override kinds in `[[profiles.<p>.override]]`):

| Optimiser | Does |
|---|---|
| `json` | strips whitespace between JSON tokens of `.json` and `.mcmeta`; every token's bytes kept |
| `png` | oxipng, output decoded and compared to the input pixels |
| `audio` | OptiVorbis remux, and silent sounds replaced by the smallest stream of the same length; PCM compared |
| `shader` | removes GLSL comments and spaces, keeps line breaks and preprocessor lines byte for byte |
| `properties` | removes comments and blank lines; enabled only with `keep_mod_files = ["optifine"]` |
| `legacy_lang` | removes comment and blank lines, and the UTF-8 BOM, from legacy `.lang` files |
| `mcfunction` | removes comment and blank lines |
| `nbt` | recompresses gzip NBT with Zopfli, payload compared |

Lossy graph passes:

| Pass | Stage | Does |
|---|---|---|
| `uv-crop-particles` | Textures | `uv-crop` that also crops the textures models use as their particle |
| `overlay-floor` | Overlays | drops overlay entries below `oldest_format`, and their files; `oldest_format` in the profile enables it on every path |
| `prune-unnamed-sprites` | Lossy | removes `single` atlas sprites nothing names |
| `texel-downscale` | Lossy | downscales textures whose tightest face exceeds `ceiling` texels per model unit |
| `stream-sounds` | Lossy | marks sounds longer than `seconds` as streamed |
| `sound-mono` | Lossy | downmixes stereo sounds to mono |
| `sound-rate` | Lossy | resamples sounds above `cap` Hz |
| `ogg-transcode` | Lossy | re-encodes Ogg Vorbis at `quality`, kept only when smaller |
| `audio-to-ogg` | Lossy | converts `.aac`, `.flac`, `.m4a`, `.mp3`, `.wav` and `.wave` sounds to `.ogg`; references follow |

Lossy file optimisers:

| Optimiser | Does |
|---|---|
| `png-alpha` | rewrites the RGB of fully transparent texels (shaders can read it) |
| `png-quantize` | palette quantisation with imagequant, kept only when it beats the lossless result |
| `png-single-color` | downsizes single-colour textures, keeping the mip levels the original size allowed |

Each pass checks its options at config load with the reader it runs with; `packprune schema` lists them.

## Invariants

- **Determinism.** The same input, config and packprune build give a byte-identical zip and report. Every
  collection that reaches output is a `BTreeMap`/`BTreeSet` or is sorted. `clippy-determinism.toml`, linked as
  `clippy.toml` in `packprune-zip`, `packprune-passes`, `packprune-report` and `packprune-engine`, bans
  `std::collections::HashMap` and `HashSet` there. Parallel results are gathered by key, and a pass's edits
  are applied in order, so the thread count never shows in the output. Zip entries are sorted with fixed
  times. Ogg stream serials are fixed (OptiVorbis with `randomize_stream_serials = false`, the Vorbis
  encoder through `new_with_serial`). No clock, host data or random number reaches the output.
- **Lossless by default.** The default profile enables only lossless work. Every graph change is proved
  by the strict oracle before anything is written, and every optimiser and writer change is proved on the
  zip as written, on all four channels of every texel. The framework refuses the run when a pass touches what
  it must not: it remaps a kept name or a file a shader imports by text, edits outside its scope, or
  removes, moves or renames a pinned path (named by a source or by `force_include`).
- **Lossy is opt-in and measured.** Lossy work needs an explicit `lossy.<name>` table with globs (or
  `oldest_format` for `overlay-floor`). The PackSquash settings mapping never enables it. Thresholds
  (`min_ssim`, `max_delta` for images; `max_lsd`, `min_snr` for sounds) keep the original file when they are
  exceeded, and the oracle reports what changed for the player.
- **Content-addressed cache.** Keys are `blake3(build, step, canonical options, input digests)`, where
  `build` is the blake3 of the running executable, so a rebuild never reads another build's entries. Cached
  steps: file optimiser outputs (content, path, kind, quirks, options), zip entry compression (content,
  class, effort), the vanilla index (jar digest), per-file usages of the configs source, the token memo
  and the oracle memo. Whole-pack passes always rerun. Entries are written to a temporary file and renamed. A
  corrupt entry is a miss, and a failed write is logged without failing the run. The location is
  `PACKPRUNE_CACHE`, else `[cache] dir`, else `.packprune-cache` beside the config; without a config or the
  variable, there is no cache.
- **Input limits.** Defaults: 262 144 entries, 2 GiB in total, 512 MiB per entry, an inflated/compressed
  ratio of at most 1 000 for entries over 1 MiB, and an inflated size equal to the declared size. Paths must be
  UTF-8, `/`-separated and relative, with no `\`, NUL, drive prefix, or `.`/`..` segment. Duplicate paths,
  encrypted entries, methods other than stored and deflate, and symlinks in a directory input are refused.
  Each refusal names the entry and exits 2. `[input.limits]` raises a limit.
- **Panics are bugs.** User input never panics (the fuzz targets check it); the drop-in catches a panic and
  falls back as for any failure of that step.

## Zip layouts

`zip.mode` picks the layout. `conformant` follows APPNOTE and any zip tool reads it. `lean` is the default
for `optimise`: entries with identical method, CRC, size and payload share one local record, and local
headers carry an empty name. The central directory stays complete and exact, which is all
`java.util.zip.ZipFile`, the client's reader, uses. Effort 0 stores entries, 1-254 uses libdeflate level
12, and 255 also tries Zopfli where libdeflate saves over 1 %, with fewer iterations as the entry grows. The smallest encoding
wins. ZIP64 entry counts are written, but entry sizes and offsets must stay below 4 GiB. The writer stages `<out>.partial`, syncs it, and renames it.

## Drop-in mode

Nexo runs `<executable_path> <settings_path>` in place of PackSquash. packprune runs the drop-in when it
gets one argument that is an existing file and not a subcommand name, or through `packprune squash`. It
reads the PackSquash 0.4.1 settings file (`packprune_config::packsquash`): `pack_directory` and
`output_file_path` are the input and output, lossless options map to packprune keys, and lossy options are
reported and never applied. packprune's own config is `PACKPRUNE_CONFIG`, else `packprune.toml` beside the
executable with symlinks resolved. That config supplies what the settings cannot say: the vanilla client,
the sources, the lossy tables, the cache and `[report] dir`. Its profile overrides the mapped settings.
`zip_spec_conformance_level = "balanced"` maps to `lean` only when `zip.allow` contains `lean`. The
`java8_zip_parsing` quirk forces `conformant`.

Output lines carry the PackSquash marks Nexo logs (`!` error, `*` warning, `-` information). Once the
settings and the input are accepted, a pack always ships, and never one that packprune could not prove:

| Failure | Artefact |
|---|---|
| invalid settings, or input refused by `PackInput::open` | none, exit 2 |
| own config absent or invalid, a source refused, client jar unreadable, analysis error | the validated input, written as it is (`conformant`) |
| pass error or panic, oracle difference, optimiser error, self-check failure | the pruned pack (no pass, no optimiser), self-checked; else the original |
| write or compression error | none, exit 3 |

The report of the last run goes to `latest/` in `[report] dir` (default `reports` beside the config). Its
files are replaced by name, and nothing else in that folder is touched. See [install](install.md) for the
setup.

## Gate

`scripts/check` is the gate, and CI runs the same command as `ci-ok` on every pull request:
`cargo fmt --check`, `cargo clippy --workspace --all-targets --locked -- -D warnings`,
`cargo test --workspace --locked`, `cargo deny` (advisories, bans, licences, sources), the crate-edge
check, and the config schema drift check against `docs/packprune.schema.json`. Pushes to `main` also run:

- `scripts/fuzz-smoke`: every target in `fuzz/` (`zip_read`, `json_tokens`, `pack_meta`, `png`, `ogg`,
  `glsl_tokens`, `properties`, `config_toml`, `yaml_source`) for 60 seconds on a pinned nightly;
- `scripts/determinism`: every fixture pack optimised four times, cold and warm on its own cache with
  `--threads 1` and `--threads 8`. All four runs must succeed and the zips must be byte-identical.
  `scripts/determinism <pack> <optimise args>` runs the same proof on any pack.
