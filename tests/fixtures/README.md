# Fixtures

Small packs and inputs shared by every crate's tests, read-only.

| Directory | What |
|---|---|
| `packs/<trap>/` | one pack per trap: `pack/` is the pack, `expect.txt` what the trap is about, `expected/report.json` its expected report |
| `vanilla/` | the client jar's tree, zipped into a `client.jar` by the tests that need one |
| `sounds/` | Ogg files of the sound passes and codec tests |
| `sources/configs/<case>/` | plugin config inputs and their expected usages |
| `sources/datapacks/<case>/` | data pack inputs and their expected usages |
| `sources/nexo/` | a Nexo plugin folder, its built pack, the expected usages |
| `sources/combined/` | a pack with a plugins folder and data packs, every source at once |

`expect.txt` has one line per expectation, `<what> <argument>`: a file status (`reached`, `orphan`, `unused`,
`dead_blockstate`, `atlas_loaded_orphan`, `unknown`) followed by its path; `missing` or `other_missing` and a target;
`root <root> <path>`; `not_stitched <sprite>`; `warning <prefix>`; `usage <source> <location> <matched|unmatched|vanilla>`.
