# Benchmarks

packprune against [PackSquash](https://github.com/ComunidadAylas/PackSquash) 0.4.1, both at their strongest
settings, on five public packs and two server packs.

## Public packs

[`bench` workflow](../.github/workflows/bench.yml) run
[41917093762](https://github.com/mediavee/packprune/actions/runs/41917093762), on a 4-vCPU GitHub-hosted
runner. One run at a time; the time is the wall clock of the whole command, zip written. Hosted runners vary in
speed from one run to the next, so compare times within a run, not across runs.

### Lossless

Conformant zip on both sides: PackSquash `zip_spec_conformance_level = 'high'`, packprune `mode = "conformant"`.

| Pack | Input | PackSquash | packprune | vs PackSquash | PackSquash time | packprune time | cached |
|---|---:|---:|---:|---:|---:|---:|---:|
| Faithful 32x | 13,116,019 | 8,020,863 | **8,008,444** | −0.15 % | 339.2 s | **163.6 s** | 1.3 s |
| Faithful 64x | 19,411,638 | fails¹ | **13,796,494** | | | 281.9 s | 1.5 s |
| Guns++ | 25,568,422 | 6,711,796 | **6,491,396** | −3.28 % | 139.7 s | **110.4 s** | 3.0 s |
| CTM Overhaul | 40,806,610 | **486,867** | 487,524 | +0.13 % | 20.5 s | **6.8 s** | 1.0 s |
| Enhanced Audio | 18,977,120 | **17,214,940** | 17,222,998 | +0.05 % | 30.4 s | **24.8 s** | 0.9 s |

### Lossy

PNG quantisation, transparent-colour clearing and Vorbis transcoding on every texture and sound, without loss
thresholds. Each tool's smallest zip layout: PackSquash `balanced`, packprune `lean`. The audio settings differ:
see [below](#where-the-gap-comes-from).

| Pack | PackSquash | packprune | vs PackSquash | PackSquash time | packprune time | cached |
|---|---:|---:|---:|---:|---:|---:|
| Faithful 32x | 5,078,133 | **4,784,591** | −5.78 % | 297.5 s | **274.7 s** | 1.4 s |
| Faithful 64x | fails¹ | **8,255,183** | | | 500.5 s | 1.5 s |
| Guns++ | fails² | **4,541,404** | | | 133.2 s | 8.1 s |
| CTM Overhaul | 446,691 | **378,512** | −15.26 % | 20.8 s | **6.8 s** | 1.0 s |
| Enhanced Audio | **8,756,996** | 9,829,127 | +12.24 % | **36.7 s** | 51.1 s | 26.1 s |

1. `background_dark.png.mcmeta: JSON error: trailing characters`.
2. `blasterpistol_shoot.ogg: Unknown or invalid channel count`.

Sizes are in bytes. *Cached* is a second packprune run on the same input with the first run's cache.

## Server packs

Two Nexo packs from production servers, pruned with their server's plugin folder (`nexo`, `configs` and
`datapacks` sources). Neither pack is public. Measured on a 16-core Linux host, one run at a time.

| Pack | Input | PackSquash | packprune | vs PackSquash | PackSquash time | packprune time | cached |
|---|---:|---:|---:|---:|---:|---:|---:|
| Server A, lossless | 14,790,783 | 11,721,172 | **8,468,845** | −27.7 % | 98.5 s | **74.0 s** | 9.2 s |
| Server B, lossless | 7,784,827 | 5,825,949 | **4,929,429** | −15.4 % | 77.2 s | **58.4 s** | 6.5 s |
| Server A, lossy | | 9,174,963 | **6,996,835** | −23.7 % | | | |
| Server B, lossy | | 5,118,262 | **4,276,945** | −16.4 % | | | |

Same settings as the public packs, with each pack's own PackSquash options.

## Where the gap comes from

- **Pruning.** Public packs ship to everyone, so nearly every file has a use. A server pack accumulates textures,
  models and sounds that no item, glyph or config names any more: that is where most of the gap with PackSquash
  is.
- **Zip.** Zopfli on both sides; packprune skips it on entries libdeflate barely shrinks.
- **PNG.** Clients from 1.13 decode textures with stb_image, which ignores ancillary chunks and keeps the high
  byte of 16-bit samples: packprune drops both.
- **Lossy audio.** PackSquash's defaults transcode at quality 0.25 (stereo) or 0.0 (mono) and resample to
  40.05 or 32 kHz; packprune's `ogg-transcode` keeps the rate at quality 0.6. Its Enhanced Audio is larger, at a
  higher quality. Transcoding runs outside the cache, so a cached lossy run on an audio pack still
  transcodes every sound.

## Reproduce

```sh
cargo build --release --locked -p packprune-cli
PACKSQUASH=/path/to/packsquash PACKPRUNE=target/release/packprune bench/run /tmp/bench
bench/summary /tmp/bench/results.tsv
```

`bench/run` downloads the packs listed in [`bench/packs.tsv`](../bench/packs.tsv), the vanilla 26.2 client and
its asset index, and checks their hashes. Settings: [`bench/packsquash/`](../bench/packsquash) and
[`bench/packprune-*.toml`](../bench). `bench/summary --svg docs/assets` redraws the README chart.
