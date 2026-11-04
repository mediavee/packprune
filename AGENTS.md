# packprune

packprune is a Rust workspace (`crates/`) that builds the `packprune` binary. `docs/architecture.md` describes
the crates and their allowed dependency edges, the `optimise` pipeline, the passes, the invariants and the gate.
The authoritative edge table is the one `scripts/check` enforces; a new crate goes there.

## Gates

Run `scripts/check` before opening a PR: fmt, clippy, tests, cargo-deny (advisories, licences), crate edges and
config schema drift. A new dependency needs a licence allowed in `deny.toml`.
CI runs the same command as `ci-ok`. Pushes to main also run `scripts/fuzz-smoke` and `scripts/determinism`.
After a config change, regenerate the schema with
`cargo run --quiet --locked -p packprune-cli -- schema > docs/packprune.schema.json`.
