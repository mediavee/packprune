//! The run's view of the content cache: optional, keyed by the build, the step, its options and its input digests.

use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};

use packprune_cache::{Key, Store};
use packprune_graph::{Computed, TokenMemo, TokenSource};
use packprune_pass::SubCache;

/// The token memo's entry: one per build, rewritten by every run with what it asked.
const TOKEN_MEMO: &str = "token memo";

/// The store of the run, or none, with what it answered.
pub(crate) struct Cache {
    store: Option<Store>,
    /// Entries are read but never written or evicted.
    read_only: bool,
    /// Hex digest of the running executable: the identity of every codec, pass and format whose bytes an entry
    /// holds, so a rebuild under the same version never reads another build's outputs.
    build: String,
    hits: AtomicU64,
    misses: AtomicU64,
    /// Set once read: until then, and without a store, every file is tokenized.
    tokens: OnceLock<TokenMemo>,
}

impl Cache {
    /// Without a readable executable no entry can be tied to this build: the run goes on without a cache.
    pub(crate) fn new(store: Option<Store>) -> Self {
        let build = store.as_ref().map(|_| {
            std::env::current_exe()
                .and_then(std::fs::read)
                .map(|bytes| blake3::hash(&bytes).to_hex().to_string())
        });
        match build {
            Some(Ok(build)) => Self::with_build(store, build),
            Some(Err(error)) => {
                tracing::warn!(%error, "running executable unreadable: no cache this run");
                Self::with_build(None, String::new())
            }
            None => Self::with_build(None, String::new()),
        }
    }

    pub(crate) fn with_build(store: Option<Store>, build: String) -> Self {
        Self {
            store,
            read_only: false,
            build,
            hits: AtomicU64::new(0),
            misses: AtomicU64::new(0),
            tokens: OnceLock::new(),
        }
    }

    /// The token memo an earlier run saved, or an empty one; `None` without a store.
    pub(crate) fn load_tokens(&self) -> Option<TokenMemo> {
        self.enabled().then(|| {
            self.get(TOKEN_MEMO, b"", &[], TokenMemo::from_bytes)
                .unwrap_or_default()
        })
    }

    /// Every index the run builds from now on takes its tokens from `memo`.
    pub(crate) fn use_tokens(&self, memo: TokenMemo) {
        let _ = self.tokens.set(memo);
    }

    /// The tokens this run asked for, for [`Self::save_tokens`].
    pub(crate) fn token_bytes(&self) -> Option<Vec<u8>> {
        self.tokens.get().map(TokenMemo::to_bytes)
    }

    pub(crate) fn save_tokens(&self, bytes: &[u8]) {
        self.put(TOKEN_MEMO, b"", &[], bytes);
    }

    pub(crate) fn read_only(mut self) -> Self {
        self.read_only = true;
        self
    }

    /// The value `decode` reads from the entry of this step, options and inputs; an entry it cannot read is a miss.
    pub(crate) fn get<T>(
        &self,
        step: &str,
        options: &[u8],
        inputs: &[[u8; 32]],
        decode: impl FnOnce(&[u8]) -> Option<T>,
    ) -> Option<T> {
        let store = self.store.as_ref()?;
        let value = store
            .get(&Key::derive(&self.build, step, options, inputs))
            .and_then(|bytes| decode(&bytes));
        let counter = if value.is_some() {
            &self.hits
        } else {
            &self.misses
        };
        counter.fetch_add(1, Ordering::Relaxed);
        value
    }

    /// A failed write costs the next run its hit, not this run its output: it is logged and the run goes on.
    pub(crate) fn put(&self, step: &str, options: &[u8], inputs: &[[u8; 32]], value: &[u8]) {
        if let Some(store) = self.store.as_ref().filter(|_| !self.read_only)
            && let Err(error) = store.put(&Key::derive(&self.build, step, options, inputs), value)
        {
            tracing::warn!(%error, step, "cache write failed");
        }
    }

    /// Whether the run has a store: a value costly to serialise is not built for nothing.
    pub(crate) fn enabled(&self) -> bool {
        self.store.is_some()
    }

    /// Hits and misses so far.
    pub(crate) fn counts(&self) -> (u64, u64) {
        (
            self.hits.load(Ordering::Relaxed),
            self.misses.load(Ordering::Relaxed),
        )
    }

    /// Least recently written entries out, down to the configured size.
    pub(crate) fn evict(&self) {
        if let Some(store) = self.store.as_ref().filter(|_| !self.read_only)
            && let Err(error) = store.evict()
        {
            tracing::warn!(%error, "cache eviction failed");
        }
    }
}

impl SubCache for Cache {
    fn get(&self, step: &str, inputs: &[u8]) -> Option<Vec<u8>> {
        Cache::get(
            self,
            step,
            b"",
            &[*blake3::hash(inputs).as_bytes()],
            |bytes| Some(bytes.to_vec()),
        )
    }

    fn put(&self, step: &str, inputs: &[u8], value: &[u8]) {
        Cache::put(self, step, b"", &[*blake3::hash(inputs).as_bytes()], value);
    }

    fn tokens(&self) -> &dyn TokenSource {
        match self.tokens.get() {
            Some(memo) => memo,
            None => &Computed,
        }
    }
}
