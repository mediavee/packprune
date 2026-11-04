use std::collections::HashMap;
use std::fmt;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

use super::Sym;

const SHARDS: usize = 16;
/// Slots live in chunks that double in size, so a slot never moves and `resolve` takes no lock: the first chunk
/// holds `1 << FIRST_BITS` slots, and the chunks cover every `u32` index.
const FIRST_BITS: u32 = 5;
const CHUNKS: usize = 28;

type Chunk = Box<[OnceLock<Arc<str>>]>;

/// One per run, shared as `Arc<Interner>`, append-only. Texts hash into shards that only `intern` locks.
#[derive(Default)]
pub struct Interner {
    shards: [Mutex<HashMap<Arc<str>, Sym>>; SHARDS],
    next: AtomicU32,
    chunks: [OnceLock<Chunk>; CHUNKS],
}

impl Interner {
    pub fn intern(&self, text: &str) -> Sym {
        // A panic elsewhere cannot leave a shard half-updated: its map is written last.
        let mut shard = self.shards[shard_of(text)]
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if let Some(&sym) = shard.get(text) {
            return sym;
        }
        let index = self.next.fetch_add(1, Ordering::Relaxed);
        assert!(index < u32::MAX, "interner is full");
        let (chunk, offset) = locate(index);
        let text: Arc<str> = Arc::from(text);
        self.chunks[chunk].get_or_init(|| (0..chunk_len(chunk)).map(|_| OnceLock::new()).collect())
            [offset]
            .set(Arc::clone(&text))
            .expect("a fresh index has an empty slot");
        let sym = Sym(index);
        shard.insert(text, sym);
        sym
    }

    /// The text of a `Sym` this interner issued; a `Sym` from another interner is a bug and panics.
    pub fn resolve(&self, sym: Sym) -> &str {
        let (chunk, offset) = locate(sym.0);
        self.chunks
            .get(chunk)
            .and_then(OnceLock::get)
            .and_then(|slots| slots.get(offset))
            .and_then(OnceLock::get)
            .unwrap_or_else(|| panic!("{sym:?} was not issued by this interner"))
    }
}

impl fmt::Debug for Interner {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Interner")
            .field("len", &self.next.load(Ordering::Relaxed))
            .finish()
    }
}

/// FNV-1a: only picks a shard, so it need not be keyed.
fn shard_of(text: &str) -> usize {
    let hash = text.bytes().fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01b3)
    });
    (hash % SHARDS as u64) as usize
}

fn chunk_len(chunk: usize) -> usize {
    1 << (chunk as u32 + FIRST_BITS)
}

fn locate(index: u32) -> (usize, usize) {
    let shifted = u64::from(index) + (1 << FIRST_BITS);
    let chunk = shifted.ilog2() - FIRST_BITS;
    (
        chunk as usize,
        (shifted - (1 << (chunk + FIRST_BITS))) as usize,
    )
}
