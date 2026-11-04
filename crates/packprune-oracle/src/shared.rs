//! What one comparison shares between views and between its two sides: each file parsed or decoded once, by
//! content, and the digests of the texel blocks faces sample.

use std::collections::HashMap;
use std::hash::{BuildHasher, Hash, RandomState};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

use packprune_codec::{audio, png};

use crate::canon::{self, Digest};
use packprune_pack::json::{self, Node};

const SHARDS: usize = 32;

/// A concurrent compute-once map: one computation per key, whichever thread asks first.
pub(crate) struct Memo<K, V> {
    shards: Vec<Mutex<HashMap<K, Arc<OnceLock<V>>>>>,
    hasher: RandomState,
}

impl<K: Hash + Eq + Clone, V: Clone> Memo<K, V> {
    pub(crate) fn new() -> Self {
        Self {
            shards: (0..SHARDS).map(|_| Mutex::default()).collect(),
            hasher: RandomState::new(),
        }
    }

    pub(crate) fn clear(&self) {
        for shard in &self.shards {
            shard.lock().unwrap_or_else(PoisonError::into_inner).clear();
        }
    }

    pub(crate) fn get(&self, key: &K, compute: impl FnOnce() -> V) -> V {
        let shard = self.hasher.hash_one(key) as usize % SHARDS;
        let cell = Arc::clone(
            self.shards[shard]
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .entry(key.clone())
                .or_default(),
        );
        cell.get_or_init(compute).clone()
    }
}

/// A PNG as the client's `NativeImage` holds it: 8-bit RGBA, row by row.
pub(crate) struct Image {
    pub(crate) width: i32,
    pub(crate) height: i32,
    pub(crate) rgba: Vec<u8>,
}

impl Image {
    pub(crate) fn alpha(&self, x: i32, y: i32) -> u8 {
        self.rgba[((y * self.width + x) * 4 + 3) as usize]
    }
}

/// An Ogg as the client's Vorbis decoder hears it: its format and the digest of its samples, never the samples.
pub(crate) struct Sound {
    pub(crate) rate: u32,
    pub(crate) channels: usize,
    pub(crate) frames: usize,
    pub(crate) samples: Arc<str>,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct Block {
    file: Digest,
    x: i32,
    y: i32,
    width: i32,
    height: i32,
}

pub(crate) struct Shared {
    parsed: Memo<Digest, Option<Arc<Node>>>,
    parsed_lenient: Memo<Digest, Option<Arc<Node>>>,
    decoded: Memo<Digest, Option<Arc<Image>>>,
    heard: Memo<Digest, Option<Arc<Sound>>>,
    blocks: Memo<Block, Arc<str>>,
}

impl Shared {
    pub(crate) fn new() -> Self {
        Self {
            parsed: Memo::new(),
            parsed_lenient: Memo::new(),
            decoded: Memo::new(),
            heard: Memo::new(),
            blocks: Memo::new(),
        }
    }

    /// Drops the parsed files: a side's trees are not kept while the other side resolves.
    pub(crate) fn forget_parsed(&self) {
        self.parsed.clear();
        self.parsed_lenient.clear();
    }

    /// The JSON tree of a file with content digest `digest`; `None` when it does not parse.
    pub(crate) fn json(&self, digest: Digest, bytes: &[u8]) -> Option<Arc<Node>> {
        self.parsed
            .get(&digest, || json::parse(bytes).ok().map(Arc::new))
    }

    /// [`Self::json`] with the grammar Gson's `LENIENT` reads a language file with.
    pub(crate) fn json_lenient(&self, digest: Digest, bytes: &[u8]) -> Option<Arc<Node>> {
        self.parsed_lenient
            .get(&digest, || json::parse_lenient(bytes).ok().map(Arc::new))
    }

    /// The image of a file with content digest `digest`, decoded as the client's stb_image does; `None` when the
    /// client would not load it.
    pub(crate) fn image(&self, digest: Digest, bytes: &[u8]) -> Option<Arc<Image>> {
        self.decoded.get(&digest, || {
            let image = png::decode(bytes).ok()?;
            Some(Arc::new(Image {
                width: i32::try_from(image.width).ok()?,
                height: i32::try_from(image.height).ok()?,
                rgba: image.rgba,
            }))
        })
    }

    /// What a file with content digest `digest` sounds like, decoded as the client's JOrbis does; `None` when the
    /// client would not play it. Only the digest of the samples stays: the PCM is dropped with the decode.
    pub(crate) fn sound(&self, digest: Digest, bytes: &[u8]) -> Option<Arc<Sound>> {
        self.heard.get(&digest, || {
            if !bytes.starts_with(b"OggS") {
                return None;
            }
            let pcm = audio::decode(bytes).ok()?;
            let mut hasher = blake3::Hasher::new();
            let mut chunk = Vec::with_capacity(4 * 4096);
            for channel in &pcm.channels {
                for samples in channel.chunks(4096) {
                    chunk.clear();
                    // `+ 0.0` turns -0.0 into 0.0: the codec's own PCM equality does not tell them apart.
                    chunk.extend(samples.iter().flat_map(|s| (s + 0.0).to_le_bytes()));
                    hasher.update(&chunk);
                }
            }
            Some(Arc::new(Sound {
                rate: pcm.rate,
                channels: pcm.channels.len(),
                frames: pcm.frames(),
                samples: canon::encode(&canon::truncate(hasher.finalize())).into(),
            }))
        })
    }

    /// The digest of the `width` by `height` block at `x, y` of an image, clipped to it.
    pub(crate) fn block(
        &self,
        file: Digest,
        image: &Image,
        x: i32,
        y: i32,
        width: i32,
        height: i32,
    ) -> Arc<str> {
        let key = Block {
            file,
            x,
            y,
            width,
            height,
        };
        self.blocks.get(&key, || {
            let x0 = x.max(0);
            let y0 = y.max(0);
            let x1 = image.width.min(x.saturating_add(width));
            let y1 = image.height.min(y.saturating_add(height));
            let mut hasher = blake3::Hasher::new();
            hasher.update(&(x1 - x0).to_be_bytes());
            hasher.update(&(y1 - y0).to_be_bytes());
            if x1 > x0 {
                for row in y0..y1 {
                    let start = ((row * image.width + x0) * 4) as usize;
                    let end = ((row * image.width + x1) * 4) as usize;
                    hasher.update(&image.rgba[start..end]);
                }
            }
            canon::encode(&canon::truncate(hasher.finalize())).into()
        })
    }
}
