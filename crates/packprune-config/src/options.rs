use crate::{Config, ConfigError, OptionsCheck, OptionsOwner, validation::Source};

/// Every option table of every profile, read whole by its owner; a refusal is located at the table.
pub(crate) fn validate(
    config: &Config,
    source: &Source<'_>,
    check: &dyn OptionsCheck,
) -> Result<(), ConfigError> {
    for (name, profile) in &config.profiles {
        for (pass, lossy) in &profile.lossy {
            let owner = OptionsOwner::LossyPass(pass);
            let base = ["profiles", name, "lossy", pass];
            check
                .check_owner(owner)
                .and_then(|()| check.check_options(owner, &lossy.options))
                .map_err(|why| source.error(&base, &why))?;
        }
        for (index, rule) in profile.overrides.iter().enumerate() {
            let index = index.to_string();
            for (kind, options) in &rule.kinds {
                let owner = OptionsOwner::Kind(kind);
                let base = ["profiles", name, "override", &index, kind];
                check
                    .check_owner(owner)
                    .and_then(|()| check.check_options(owner, options))
                    .map_err(|why| source.error(&base, &why))?;
            }
        }
    }
    Ok(())
}
