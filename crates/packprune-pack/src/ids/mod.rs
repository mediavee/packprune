use std::fmt;
use std::sync::Arc;

mod interner;

#[cfg(test)]
mod tests;

pub use interner::Interner;

/// What every stage of one run shares, built once by the engine and handed to sources, the graph, passes
/// and the oracle, so a `Sym` means the same text everywhere in the run.
#[derive(Clone, Debug, Default)]
pub struct RunCx {
    pub interner: Arc<Interner>,
}

/// An interned namespace or path, from the run's [`Interner`]. Its number is the order of first interning, which
/// parallel work does not fix: never order output by it, resolve it to text first.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Sym(pub u32);

const DEFAULT_NAMESPACE: &str = "minecraft";

/// `namespace:path`; a missing namespace is `minecraft`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ResourceId {
    pub ns: Sym,
    pub path: Sym,
}

impl ResourceId {
    /// The namespace is `[a-z0-9_.-]`, the path the same plus `/`; an id without a
    /// colon, or with the colon first, gets `minecraft`. Anything else, a second colon included, is `None`.
    pub fn parse(interner: &Interner, value: &str) -> Option<Self> {
        let (ns, path) = match value.find(':') {
            Some(colon) if colon > 0 => (&value[..colon], &value[colon + 1..]),
            Some(_) => (DEFAULT_NAMESPACE, &value[1..]),
            None => (DEFAULT_NAMESPACE, value),
        };
        let valid = |text: &str, slash: bool| {
            text.bytes().all(|byte| {
                matches!(byte, b'a'..=b'z' | b'0'..=b'9' | b'_' | b'.' | b'-')
                    || slash && byte == b'/'
            })
        };
        (valid(ns, false) && valid(path, true)).then(|| Self {
            ns: interner.intern(ns),
            path: interner.intern(path),
        })
    }

    /// `namespace:path`, resolved through the interner that made the id.
    pub fn display(self, interner: &Interner) -> impl fmt::Display {
        struct Shown<'a>(ResourceId, &'a Interner);
        impl fmt::Display for Shown<'_> {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(
                    f,
                    "{}:{}",
                    self.1.resolve(self.0.ns),
                    self.1.resolve(self.0.path)
                )
            }
        }
        Shown(self, interner)
    }
}
