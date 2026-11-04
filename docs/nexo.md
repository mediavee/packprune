# Replace PackSquash in Nexo

Nexo runs PackSquash each time it builds the server pack. packprune reads the same settings file, so you only
change the executable path.

## 1. Put the binary where the server can run it

Use the Linux binary from the [releases](https://github.com/mediavee/packprune/releases), at a path the server
process sees: in a Pterodactyl container, `/home/container/tools/packprune/packprune`, not a host path.

```sh
chmod +x /home/container/tools/packprune/packprune
```

The directory must be writable by the server user: the cache lives there.

## 2. Add `packprune.toml` beside it

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
```

| Setting | Set it to |
|---|---|
| `client_jar` | the vanilla client jar of the server's Minecraft version |
| `asset_index` | its launcher asset index |
| `[sources]` | your server's folders ([sources](configuration.md#server-sources)) |

Relative paths resolve beside this file. To keep it elsewhere, set `PACKPRUNE_CONFIG` in the server's
environment.

## 3. Point Nexo at packprune

In `plugins/Nexo/settings.yml`, keep your PackSquash options and change only `executable_path`:

```yaml
Pack:
  generation:
    packsquash:
      enabled: true
      executable_path: /home/container/tools/packprune/packprune
      settings_path: plugins/Nexo/pack/packsquash/packsquash.toml
```

Then **restart the server**: `nexo reload pack` does not reread `executable_path`.

## How it behaves

- packprune reads `pack_directory` and `output_file_path` from the PackSquash settings and writes the zip where
  Nexo expects it.
- PackSquash's lossy options are listed in the report but never applied. Lossy work comes only from
  [`lossy` tables](configuration.md#lossy-passes) in `packprune.toml`.
- Nexo skips the optimiser when `plugins/Nexo/pack/packsquash/.cache/<hash>.zip` exists. Delete that entry to
  rebuild an unchanged pack.
- packprune's own cache is `.packprune-cache` beside its config. A new binary never reuses an old one's
  entries.
- The last report is in `reports/latest/` beside the config.

## When something fails

| What failed | What Nexo gets |
|---|---|
| PackSquash settings invalid, input unreadable | no zip |
| `packprune.toml`, a source, the client jar or the analysis | the original pack |
| a pass, the equivalence check, an optimiser or the self-check | the pruned pack, without optimisation |

These fallbacks exit 0 so the server still gets a pack. Look for `!` lines in Nexo's log to know whether the
full optimisation ran.

## Smaller zips

The [`lean` zip layout](configuration.md#zip-layout) is smaller and loads in the client. In the drop-in, it
needs both:

```toml
# packprune.toml: remove mode = "conformant"
[profiles.default]
passes = "lossless"
zip = { allow = ["lean"] }
```

```toml
# Nexo's PackSquash settings
zip_spec_conformance_level = "balanced"
```

The `java8_zip_parsing` quirk in the PackSquash settings forces `conformant`.
