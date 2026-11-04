# Golden tests

Each row runs packprune on a fixture pack with one configuration and compares its output with the one recorded in
`expected/<row>/`. A row passes when the oracle (`packprune diff`: the resolved views per overlay view, as the client
reads them) finds no difference and both hold the same files, since the oracle cannot see a file no view reaches.
The rows cover every pass and lossy option on a fixture built so that the pass changes a file, the drop-in squash
command, the usage sources, and the analysis fixtures of `../fixtures/packs/`.

## Layout

| Path | What |
|---|---|
| `rows` | the rows: a pack, its configurations, its analysis options |
| `configurations` | each configuration's `optimise` options; `drop-in` and `transcode` markers |
| `configs/` | the config files of the lossy configurations |
| `fixtures/` | one pack per pass family, written by `fixtures/generate.py` |
| `expected/<row>/` | the recorded output of each row, extracted |
| `check` | runs every row and compares it with `expected/` (`scripts/check` runs it) |
| `record` | runs every row and records its output in `expected/` |
| `audio/` | `golden-audio`: decodes the sounds of two packs and compares them |
| `audio-tolerances` | its tolerances |

## Running

```sh
tests/golden/check                  # every row; JOBS rows at once (4)
tests/golden/check 'models.*'       # the rows matching the patterns
```

Both scripts build packprune from the workspace; `PACKPRUNE_BIN` (and `GOLDEN_AUDIO_BIN` for `check`) use another
binary instead.

## Re-recording

After a change meant to alter an output, or a fixture change (`python3 fixtures/generate.py` first), re-record the
rows it affects and review the diff of `expected/` before committing it:

```sh
tests/golden/record 'sounds.*'
git diff --stat tests/golden/expected
```

`record` refuses a row whose configuration names a pass that changes no file of its fixture.

## Sounds

Vorbis encodes differ from one platform to another: the same packprune gives other bytes on macOS and Linux, arm64
and x86_64. The rows of a `transcode` configuration therefore leave the sounds' sample digests out of the oracle's
comparison and compare the decoded sounds with `golden-audio` instead, which refuses a sound whose channels or rate
differ or that goes past one of the tolerances of `audio-tolerances`: the frame count, the largest per-channel RMS
level difference, the LSD between the two outputs (`packprune_codec::metrics::sound_loss`), the difference of each
output's LSD against the input sound, and over a row the medians of the RMS difference and of the difference against
the input, which catch a shift every sound shares.

Each tolerance is about ten times the largest difference measured between macOS arm64, Linux arm64 and Linux x86_64
on the transcode rows: 0 frames, 0.003 dB RMS, 0.043 dB LSD, 0.007 dB LSD against the input, medians 0.003 and
0.006 dB. A changed quality (`sound-mono` against `sound-mono-q3`: 0.29 dB RMS, 1.2 dB LSD, 0.67 dB against the
input) fails every one of them.
