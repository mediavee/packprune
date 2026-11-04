use std::collections::BTreeSet;

use globset::GlobMatcher;

use crate::{Config, Options, Profile, Zip, schema_types, validation::glob};

impl Config {
    /// Resolves scalar overrides, inherited lossy options, and child list replacements.
    pub fn resolve_profile(&self, name: &str) -> Result<Profile, String> {
        let mut chain = Vec::new();
        let mut seen = BTreeSet::new();
        let mut current = Some(name);
        while let Some(name) = current {
            if !seen.insert(name) {
                return Err(format!("profile inheritance cycle at {name}"));
            }
            let profile = self
                .profiles
                .get(name)
                .ok_or_else(|| format!("unknown profile {name}"))?;
            chain.push(profile);
            current = profile.inherits.as_deref();
        }
        let mut resolved = Profile::default();
        for profile in chain.into_iter().rev() {
            resolved.layer(profile);
        }
        Ok(resolved)
    }
}

impl Profile {
    /// `child` over this profile, as a profile over the one it inherits: scalars it sets win, its non-empty lists
    /// replace, its lossy tables merge into ours.
    pub fn layer(&mut self, child: &Profile) {
        if child.passes.is_some() {
            self.passes = child.passes.clone();
        }
        self.threads = child.threads.or(self.threads);
        self.oldest_format = child.oldest_format.or(self.oldest_format);
        if !child.disable.is_empty() {
            self.disable = child.disable.clone();
        }
        if !child.keep_mod_files.is_empty() {
            self.keep_mod_files = child.keep_mod_files.clone();
        }
        if let Some(zip) = &child.zip {
            let parent = self.zip.get_or_insert_with(Zip::default);
            if zip.mode.is_some() {
                parent.mode = zip.mode.clone();
            }
            if zip.effort.is_some() {
                parent.effort = zip.effort.clone();
            }
            parent.recompress_compressed =
                zip.recompress_compressed.or(parent.recompress_compressed);
            if zip.comment.is_some() {
                parent.comment = zip.comment.clone();
            }
            if !zip.allow.is_empty() {
                parent.allow = zip.allow.clone();
            }
        }
        for (name, lossy) in &child.lossy {
            let mut merged = lossy.clone();
            if let Some(parent) = self.lossy.get(name) {
                let mut options = parent.options.clone();
                options.extend(merged.options);
                merged.options = options;
            }
            self.lossy.insert(name.clone(), merged);
        }
        if !child.overrides.is_empty() {
            self.overrides = child.overrides.clone();
        }
    }

    /// The first matching rule for the kind wins as a whole. Properties require OptiFine support.
    pub fn options_for(&self, kind: &str, path: &str) -> Result<Options, globset::Error> {
        Ok(self.overrides()?.options_for(kind, path))
    }

    /// Whether a kind's optimiser runs at all: properties files are read only by OptiFine, so they are minified
    /// only when its support is kept.
    pub fn kind_enabled(&self, kind: &str) -> bool {
        kind != "properties"
            || self
                .keep_mod_files
                .iter()
                .any(|name| name == schema_types::MOD_OPTIFINE)
    }

    /// The override globs compiled once, for a caller that asks [`Self::options_for`] of every file.
    pub fn overrides(&self) -> Result<Overrides<'_>, globset::Error> {
        let matchers = self
            .overrides
            .iter()
            .map(|rule| Ok(glob(&rule.glob)?.compile_matcher()))
            .collect::<Result<_, globset::Error>>()?;
        Ok(Overrides {
            profile: self,
            matchers,
        })
    }
}

/// A profile's overrides with their globs compiled.
pub struct Overrides<'a> {
    profile: &'a Profile,
    matchers: Vec<GlobMatcher>,
}

impl Overrides<'_> {
    /// [`Profile::options_for`].
    pub fn options_for(&self, kind: &str, path: &str) -> Options {
        self.rule_for(kind, path)
            .map(|(_, options)| options)
            .unwrap_or_default()
    }

    /// The first rule that sets `kind` for `path`: its glob and its options.
    pub fn rule_for(&self, kind: &str, path: &str) -> Option<(&str, Options)> {
        if !self.profile.kind_enabled(kind) {
            return None;
        }
        self.profile
            .overrides
            .iter()
            .zip(&self.matchers)
            .find_map(|(rule, matcher)| {
                rule.kinds
                    .get(kind)
                    .filter(|_| matcher.is_match(path))
                    .map(|options| (rule.glob.as_str(), options.clone()))
            })
    }
}
