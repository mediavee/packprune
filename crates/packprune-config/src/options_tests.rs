use camino::Utf8Path;

use crate::{Options, OptionsCheck, OptionsOwner, parse_with};

struct Registry;

impl OptionsCheck for Registry {
    fn check_owner(&self, owner: OptionsOwner<'_>) -> Result<(), String> {
        match owner {
            OptionsOwner::LossyPass("texel-downscale") | OptionsOwner::Kind("png" | "audio") => {
                Ok(())
            }
            OptionsOwner::LossyPass(name) => Err(format!("unknown lossy pass {name}")),
            OptionsOwner::Kind(name) => Err(format!("unknown kind {name}")),
        }
    }

    /// A lossless owner refuses lossy values; the lossy pass takes them.
    fn check_options(&self, owner: OptionsOwner<'_>, options: &Options) -> Result<(), String> {
        for (key, value) in options {
            let valid = match (owner, key.as_str()) {
                (OptionsOwner::Kind("png"), "alpha") => value.as_str() == Some("keep"),
                (OptionsOwner::LossyPass("texel-downscale"), "alpha") => {
                    matches!(value.as_str(), Some("keep" | "rewrite"))
                }
                (OptionsOwner::Kind("png"), "quantization") => value.as_str() == Some("none"),
                (OptionsOwner::Kind("audio"), "transcode") => value.as_bool() == Some(false),
                (OptionsOwner::LossyPass("texel-downscale"), "ceiling") => value.as_u64().is_some(),
                (OptionsOwner::LossyPass("texel-downscale"), "max_loss") => true,
                _ => false,
            };
            if !valid {
                return Err(format!("invalid option {key}"));
            }
        }
        Ok(())
    }
}

#[test]
fn owner_hook_refusals_and_lossy_values_are_located() {
    let cases = [
        (
            "[profiles.default.lossy.texel-downscal]\nglobs = ['**']",
            "texel-downscal",
            2,
            "texel-downscal",
        ),
        (
            "[profiles.default.lossy.texel-downscale]\nglobs = ['**']\nceilling = 4",
            "texel-downscale",
            2,
            "ceilling",
        ),
        (
            "[[profiles.default.override]]\nglob = '**'\npnng = { alpha = 'keep' }",
            "pnng",
            4,
            "pnng",
        ),
        (
            "[[profiles.default.override]]\nglob = '**'\npng = { typo = true }",
            "png",
            4,
            "typo",
        ),
        (
            "[[profiles.default.override]]\nglob = '**'\npng = { alpha = 'rewrite' }",
            "png",
            4,
            "alpha",
        ),
        (
            "[[profiles.default.override]]\nglob = '**'\npng = { quantization = 'eight_bit_depth' }",
            "png",
            4,
            "quantization",
        ),
        (
            "[[profiles.default.override]]\nglob = '**'\naudio = { transcode = true }",
            "audio",
            4,
            "transcode",
        ),
        (
            "[profiles.default]\noverride = [{ glob = '**', png = { alpha = 'rewrite' } }]",
            "png",
            3,
            "alpha",
        ),
    ];
    let errors: Vec<_> = cases
        .iter()
        .map(|(text, ..)| {
            parse_with(
                &format!("version = 2\n{text}"),
                Utf8Path::new("packprune.toml"),
                &Registry,
            )
            .err()
            .map(|error| error.to_string())
        })
        .collect();
    assert!(errors.iter().all(Option::is_some), "{errors:?}");
    for ((text, table, line, key), error) in cases.iter().zip(errors.into_iter().flatten()) {
        let col = text.lines().nth(line - 2).unwrap().find(table).unwrap() + 1;
        assert!(
            error.starts_with(&format!("packprune.toml:{line}:{col}:")),
            "{error}"
        );
        assert!(error.contains(key), "{error}");
    }
}

#[test]
fn owner_hook_accepts_lossy_values_only_in_explicit_lossy_tables() {
    let text = "version = 2\n[profiles.default.lossy.texel-downscale]\nglobs = ['assets/example/**']\nalpha = 'rewrite'\nceiling = 4\nmax_loss = { ssim_min = 0.98 }\n[[profiles.default.override]]\nglob = '**'\npng = { alpha = 'keep', quantization = 'none' }\naudio = { transcode = false }";
    let config = parse_with(text, Utf8Path::new("packprune.toml"), &Registry).unwrap();
    assert_eq!(
        config.profiles["default"].lossy["texel-downscale"].options["alpha"],
        "rewrite"
    );
}
