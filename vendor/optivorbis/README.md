# OptiVorbis 0.3.2: preserve codebook entries

Vendored from the crates.io `optivorbis` 0.3.2 source, upstream revision
[`77d14c9`](https://github.com/OptiVorbis/OptiVorbis/tree/77d14c9dd4d0b30208e28ca2e0fccc4561096276).
Crates.io archive SHA-256: `a20c671af93a9e6a44835fb32c37ddd933874659cdc671178a1594f138a4ed30`.
The upstream BSD-3-Clause and dual-license texts are included unchanged.

The only production change removes the call to `truncate_unused_codebook_entry_suffixes`
in `src/vorbis/optimizer/setup_header_rewrite.rs`. Its implementation uses
`rposition(...).map_or(0, ...)` and can turn unused codebooks into zero-entry books.
JOrbis 0.0.17 allocates `entries * 2` decode-tree nodes and then accesses node zero,
so those books crash the Minecraft decoder. The truncation module is test-only;
upstream sources are formatted with stable rustfmt and `hard_tabs = true`.

Older OptiVorbis releases preserved entry counts but pull a second `getrandom` major version, and the
public settings offer no switch for codebook truncation, hence the patch.

The caller additionally checks the client-safe codebook subset, setup invariants, packet timing and decoded
PCM, keeping the original bytes when a rewrite cannot be proven.
