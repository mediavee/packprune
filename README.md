# packprune

Makes Minecraft Java resource packs smaller: it drops the files the client never loads, recompresses the rest,
and checks that players see and hear the same pack.

- **Prunes what nothing uses.** It follows every reference the client resolves. Server plugins (Nexo, plugin
  configs, data packs) can name the resources only the server uses.
- **Checks its own work.** Every lossless change is compared with what the client would load; one difference
  stops the run.
- **Lossy only where you ask.** PNG quantisation, downscaling, Vorbis transcoding, with quality thresholds.
- **Reproducible.** Same input, same zip, byte for byte; a cache makes repeated runs fast.
- **Drop-in for PackSquash** in Nexo.

## Install

Download a binary for Linux, macOS or Windows from the [releases](https://github.com/mediavee/packprune/releases),
or build it with Rust 1.99 or later:

```sh
cargo install --locked --git https://github.com/mediavee/packprune packprune-cli
```

## Usage

```sh
packprune optimise ./pack -o pack.zip --client-jar client-26.2.jar
packprune check ./pack --client-jar client-26.2.jar        # missing references, orphans, budgets
packprune diff ./pack pack.zip --client-jar client-26.2.jar   # what changes for the player
```

The pack is a folder or a zip. `--client-jar` is the vanilla client jar of the pack's Minecraft version. Lossy
passes, server sources and budgets go in a `packprune.toml`: see [configuration](docs/configuration.md).

## Results

Zip size in MB against PackSquash 0.4.1, on the same machine.

| Pack | Input | PackSquash lossless | packprune lossless | PackSquash lossy | packprune lossy |
|---|---:|---:|---:|---:|---:|
| Faithful 32x | 13.12 | **8.02** | 8.18 | 5.08 | **4.78** |
| Faithful 64x | 19.41 | failed | **13.96** | failed | **8.25** |
| Guns++ | 25.57 | 6.71 | **6.54** | failed | **4.61** |
| CTM Overhaul | 40.81 | **0.49** | 0.50 | 0.45 | **0.39** |
| Enhanced Audio | 18.98 | **17.21** | 17.36 | **8.76** | 9.89 |
| Server pack A | 14.79 | 11.72 | **8.55** | 9.17 | **7.03** |
| Server pack B | 7.78 | 5.83 | **4.96** | 5.12 | **4.29** |

Both tools at their strongest settings with a standard zip layout. The server packs are two Nexo packs pruned with
their server's plugin folder.

## For scripts and agents

- `packprune <command> --help` documents every flag; `packprune schema` prints the JSON schema of `packprune.toml`.
- Exit codes: `0` success, `1` findings or a budget exceeded, `2` input or config refused, `3` processing failure.
- `optimise` writes `<output>.report/`: `report.json` (orphans, missing references, warnings), `passes.json`
  (what each pass changed, measured loss) and `run.json` (output, removals by reason, costs). Read them rather
  than the logs.
- `--expect-report report.json` refuses a run whose inputs or removals differ from a report you reviewed.

## Documentation

- [Configuration](docs/configuration.md): profiles, lossy passes, server sources, reports.
- [Install and Nexo](docs/install.md): release assets, replacing PackSquash, the `lean` zip layout.
- [Architecture](docs/architecture.md): crates, the pipeline, every pass.

## Contributing

`scripts/check` runs the same gate as CI; it needs `cargo-deny`, `unzip` and Perl. See [AGENTS.md](AGENTS.md).

## License

[GPL-3.0-or-later](LICENSE). Vendored crates in `vendor/` keep their own licences; release binaries ship with
`THIRD-PARTY-LICENSES.txt`.
