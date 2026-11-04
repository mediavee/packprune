# Install packprune

packprune is one native executable. Download it from the
[releases](https://github.com/mediavee/packprune/releases).

| Platform | Release asset |
|---|---|
| Linux amd64 (static musl, runs in distroless images) | `packprune-x86_64-unknown-linux-musl` |
| Linux arm64 (static musl) | `packprune-aarch64-unknown-linux-musl` |
| macOS Apple Silicon | `packprune-aarch64-apple-darwin` |
| macOS Intel | `packprune-x86_64-apple-darwin` |
| Windows amd64 | `packprune-x86_64-pc-windows-msvc.exe` |

Each release also ships `SHA256SUMS`. Verify the asset before installing it:

```sh
tag=<release tag>
asset=packprune-x86_64-unknown-linux-musl
gh release download "$tag" --repo mediavee/packprune --pattern "$asset" --pattern SHA256SUMS
grep "  $asset$" SHA256SUMS | sha256sum --check -
install -m 755 "$asset" ./packprune
./packprune --version
```

On macOS, use `shasum -a 256 -c` in place of `sha256sum --check`. On Windows, compare
`Get-FileHash -Algorithm SHA256` with `SHA256SUMS`, then rename the asset to `packprune.exe`. The macOS
binaries are not signed or notarized, so your machine's policy has to allow them.

To build from source: `cargo build --release --locked -p packprune-cli` (the toolchain version is pinned in
`rust-toolchain.toml`).

## Nexo: replace PackSquash

Put the Linux executable on a path the game server's process can see, for example
`/home/container/tools/packprune/packprune` in a Pterodactyl container. A path that exists only on the
host does not work. Make the file executable, and make its directory writable by the server user, because
the cache lives there.

Create `packprune.toml` beside the executable. packprune resolves symlinks before looking for it.

```toml
version = 2

[vanilla]
client_jar = "/home/container/tools/minecraft/26.2.jar"
asset_index = "/home/container/tools/minecraft/assets/indexes/32.json"

[sources]
nexo = "/home/container/plugins/Nexo"
configs = ["/home/container/plugins"]
datapacks = ["/home/container/world/datapacks"]

[profiles.default]
passes = "lossless"
zip = { mode = "conformant" }

[cache]
dir = ".packprune-cache"
max_bytes = 1_073_741_824
```

- `version = 2` is the config format version.
- Supply the vanilla client jar of the server's Minecraft version and its launcher asset index. A full run
  needs the jar.
- Adjust the source paths to your server.
- Relative paths resolve beside this file, whatever Nexo's working directory is.
- To use a config file elsewhere, set `PACKPRUNE_CONFIG` in the server process's environment.

In `plugins/Nexo/settings.yml`, keep your PackSquash options and change only the executable path. Nexo
1.29 reads the case-sensitive `Pack.generation.packsquash` section:

```yaml
Pack:
  generation:
    packsquash:
      enabled: true
      executable_path: /home/container/tools/packprune/packprune
      settings_path: plugins/Nexo/pack/packsquash/packsquash.toml
```

Nexo runs `<executable_path> <settings_path>`. packprune reads the PackSquash settings, including
`pack_directory` and `output_file_path`, and writes the zip where Nexo expects it. Nexo rewrites that file
on every generation, so keep packprune's own options in `packprune.toml`. Lossy PackSquash options are
reported and never applied: lossy work comes only from `lossy.<pass>` tables in `packprune.toml`.

Restart the server after you change `executable_path`: `nexo reload pack` does not reread it. Nexo keeps its
own cache of built zips, separate from packprune's. If `plugins/Nexo/pack/packsquash/.cache/<hash>.zip`
exists for a pack, Nexo does not call the optimiser at all. To try a new binary or config on an unchanged
pack, delete that entry during maintenance, then regenerate the pack.

packprune's cache defaults to `.packprune-cache` beside its config. `[cache] dir` moves it and
`PACKPRUNE_CACHE` overrides both. The executable's digest is part of every cache key, so a new binary never
reuses an old build's entries. When a cache write fails, packprune logs it and the run continues.

Drop-in failures, as Nexo's log shows them (`!` error, `*` warning, `-` information):

- Invalid PackSquash settings or an unreadable input are refused, and no zip is written.
- If the own config is missing or invalid, a source is refused, the client jar is unreadable, or the
  analysis fails, the original input is shipped as it is.
- If a pass, the oracle, an optimiser or the self-check fails, the pruned pack is shipped.

A fallback exits 0, so check the `!` lines to confirm the full optimisation ran. The last run's report is in
`reports/latest/` beside the config (`[report] dir` changes it).

## Opt into the lean zip layout

`lean` makes the zip smaller. It shares one local record between identical entries and leaves local entry
names empty, while the central directory stays complete. Only readers that use the central directory
alone can open it. Use `conformant` for any other consumer.

| Reader | `lean` |
|---|---|
| `java.util.zip.ZipFile` (Java 8, 17, 21, 25) and NIO zipfs: what the Minecraft client uses | reads every entry |
| vanilla 26.2 and 1.16.5 clients, launched with a `lean` pack | load it |
| Nexo 1.29 upload and hash (S3), Paper | bytes kept as they are; Paper never opens the zip |
| Scaffolding's pack canonicalizer (Bedrock conversion) | reads every entry |
| Java `ZipInputStream` | entries with empty names |
| CPython `zipfile` | refuses the archive |
| info-zip `unzip`, 7-Zip | warnings or refusal |

`optimise` writes `lean` unless the profile sets `zip.mode`. The drop-in writes `lean` only when both of
these are set. In `packprune.toml`:

```toml
[profiles.default]
passes = "lossless"
zip = { allow = ["lean"] }
```

and in Nexo's PackSquash settings:

```toml
zip_spec_conformance_level = "balanced"
```

Remove `mode = "conformant"` from the own profile, since own config values override the mapped settings.
The `java8_zip_parsing` quirk in the PackSquash settings forces `conformant`.

## CLI

```sh
packprune --help
packprune optimise ./pack -o ./pack.zip --config ./packprune.toml
packprune diff ./pack ./pack.zip --config ./packprune.toml
packprune squash ./packsquash.toml
packprune schema > packprune.schema.json
```

The input can be a directory or a zip. Unlike the drop-in, `optimise` does not look for `packprune.toml`
beside the binary: pass `--config`, or give `--client-jar` (and optionally `--asset-index`) directly. Lossy
work needs a `lossy.<pass>` table with globs. `optimise` publishes the zip only after every check passes,
and writes its report to `<zip>.report/`.

Exit codes:

| Code | Meaning |
|---|---|
| 0 | success |
| 1 | findings, differences, or a budget exceeded |
| 2 | refused input or config |
| 3 | processing failure |
