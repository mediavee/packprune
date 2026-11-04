# Install

## Download a binary

Every [release](https://github.com/mediavee/packprune/releases) has one executable per platform:

| Platform | Asset |
|---|---|
| Linux x86-64 | `packprune-x86_64-unknown-linux-musl` |
| Linux ARM64 | `packprune-aarch64-unknown-linux-musl` |
| macOS Apple Silicon | `packprune-aarch64-apple-darwin` |
| macOS Intel | `packprune-x86_64-apple-darwin` |
| Windows x86-64 | `packprune-x86_64-pc-windows-msvc.exe` |

The Linux binaries are static: they run on any distribution and in distroless images.

Check it against `SHA256SUMS` before installing (this fetches the latest release; add a tag for another one):

```sh
asset=packprune-x86_64-unknown-linux-musl
gh release download --repo mediavee/packprune --pattern "$asset" --pattern SHA256SUMS
grep "  $asset$" SHA256SUMS | sha256sum --check -
install -m 755 "$asset" ~/.local/bin/packprune
```

- **macOS:** use `shasum -a 256 -c` instead of `sha256sum --check`. The binaries are not notarized.
- **Windows:** compare `Get-FileHash -Algorithm SHA256` with `SHA256SUMS`, then rename the file to
  `packprune.exe`.

## Build from source

With Rust 1.99 or later:

```sh
cargo install --locked --git https://github.com/mediavee/packprune packprune-cli
```

## Next

- [Configure it](configuration.md)
- [Use it in Nexo instead of PackSquash](nexo.md)
