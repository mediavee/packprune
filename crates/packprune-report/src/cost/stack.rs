use std::collections::{BTreeMap, BTreeSet};

use packprune_pack::{Entry, JavaPattern, PackInput, TextureSize, VanillaIndex, View};

use super::Exclusions;

/// The label of a resource that comes from the client jar.
pub(super) const CLIENT_FILE: &str = "<client jar>";

/// A file of the pack as the view resolves it, or of the client jar.
pub(super) enum Found<'a> {
    Pack { file: &'a str, entry: &'a Entry },
    Client,
}

/// One regex of a filter entry: found anywhere in the text, absent matching everything.
enum Part {
    Any,
    Pattern(JavaPattern),
    /// A pattern the Java subset of `packprune-pack` refuses, listed as an exclusion.
    Never,
}

impl Part {
    fn new(source: Option<&str>, context: &str, exclusions: &mut Exclusions) -> Self {
        let Some(source) = source else {
            return Self::Any;
        };
        match JavaPattern::new(source) {
            Ok(pattern) => Self::Pattern(pattern),
            Err(refused) => {
                exclusions.add(format!("{context}: {refused}"));
                Self::Never
            }
        }
    }

    fn find(&self, text: &str) -> bool {
        match self {
            Self::Any => true,
            Self::Pattern(pattern) => pattern.find(text),
            Self::Never => false,
        }
    }
}

/// A `pack.mcmeta` `filter.block` entry or a `filter` sprite source: Java regexes found anywhere in the namespace
/// and in the path.
pub(super) struct Matcher {
    namespace: Part,
    path: Part,
}

impl Matcher {
    /// A refused pattern matches nothing and is listed in `exclusions`.
    pub(super) fn new(
        namespace: Option<&str>,
        path: Option<&str>,
        context: &str,
        exclusions: &mut Exclusions,
    ) -> Self {
        Self {
            namespace: Part::new(namespace, context, exclusions),
            path: Part::new(path, context, exclusions),
        }
    }

    pub(super) fn matches(&self, namespace: &str, path: &str) -> bool {
        self.namespace.find(namespace) && self.path.find(path)
    }
}

/// A pack's view stacked over the client jar, as the client's resource manager does: a path resolves to the pack's
/// file, else to the jar's unless the pack's `filter.block` hides it. The client tests every namespace pattern, then
/// every path pattern, so a path is hidden when some entry matches its namespace and some entry matches its path.
pub(super) struct Stack<'a> {
    pack: &'a PackInput,
    files: BTreeMap<&'a str, &'a packprune_pack::PhysPath>,
    vanilla: &'a VanillaIndex,
    filters: Vec<Matcher>,
}

impl<'a> Stack<'a> {
    pub(super) fn new(
        pack: &'a PackInput,
        view: &'a View,
        vanilla: &'a VanillaIndex,
        filters: &[(Option<String>, Option<String>)],
        exclusions: &mut Exclusions,
    ) -> Self {
        let filters = filters
            .iter()
            .enumerate()
            .map(|(index, (namespace, path))| {
                let context = format!("pack.mcmeta > filter.block[{index}]");
                Matcher::new(namespace.as_deref(), path.as_deref(), &context, exclusions)
            })
            .collect();
        Self {
            pack,
            files: view
                .files
                .iter()
                .map(|(logical, physical)| (logical.as_str(), physical))
                .collect(),
            vanilla,
            filters,
        }
    }

    /// The pack's own logical paths, in order.
    pub(super) fn pack_paths(&self) -> impl Iterator<Item = &'a str> + '_ {
        self.files.keys().copied()
    }

    pub(super) fn pack_file(&self, path: &str) -> Option<(&'a str, &'a Entry)> {
        let physical = self.files.get(path)?;
        Some((physical.as_str(), self.pack.files.get(*physical)?))
    }

    fn hides(&self, path: &str) -> bool {
        let mut parts = path.splitn(3, '/');
        let (Some("assets"), Some(namespace), Some(rest)) =
            (parts.next(), parts.next(), parts.next())
        else {
            return false;
        };
        self.filters.iter().any(|f| f.namespace.find(namespace))
            && self.filters.iter().any(|f| f.path.find(rest))
    }

    /// A texture or `.mcmeta` file: the pack's, else the jar's unless hidden.
    pub(super) fn find(&self, path: &str) -> Option<Found<'a>> {
        if let Some((file, entry)) = self.pack_file(path) {
            return Some(Found::Pack { file, entry });
        }
        let in_client = self.vanilla.textures.contains_key(path)
            || self.vanilla.texture_meta.contains_key(path);
        (in_client && !self.hides(path)).then_some(Found::Client)
    }

    pub(super) fn client_texture(&self, path: &str) -> Option<TextureSize> {
        self.vanilla.textures.get(path).copied()
    }

    pub(super) fn client_mcmeta(&self, path: &str) -> Option<&'a [u8]> {
        self.vanilla.texture_meta.get(path).map(Vec::as_slice)
    }

    /// Every `.png` the client could load: the pack's, then the jar's that the filter leaves.
    pub(super) fn png_paths(&self) -> BTreeSet<&'a str> {
        let pack = self.pack_paths().filter(|path| path.ends_with(".png"));
        let client = self
            .vanilla
            .textures
            .keys()
            .map(String::as_str)
            .filter(|path| !self.hides(path));
        pack.chain(client).collect()
    }

    pub(super) fn has_png(&self, path: &str) -> bool {
        self.pack_file(path).is_some()
            || (self.vanilla.textures.contains_key(path) && !self.hides(path))
    }

    /// The definitions of one atlas, topmost first: the pack's, then the jar's if the filter leaves it. The pack gives
    /// one copy, the view's winner: the client's `CompositePackResources.getResource` returns the topmost overlay's file
    /// and `getResourceStack` asks each pack once, so overlays replace the root's file and never add to it.
    pub(super) fn definitions(&self, path: &str) -> Vec<(&'a str, &'a [u8])> {
        let mut layers = Vec::new();
        if let Some((file, entry)) = self.pack_file(path) {
            layers.push((file, &*entry.blob));
        }
        if self.hides(path) {
            return layers;
        }
        if let Some(bytes) = self.vanilla.atlases.get(path) {
            layers.push((CLIENT_FILE, bytes.as_slice()));
        }
        layers
    }
}
