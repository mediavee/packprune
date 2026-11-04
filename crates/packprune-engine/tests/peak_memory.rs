//! The peak heap of a cold run on a real pack, named by environment variables
//! (`cargo test --release -p packprune-engine --test peak_memory -- --ignored`):
//!
//! - `PACKPRUNE_CORPUS_PACK`, `_JAR`, `_ASSET_INDEX`: the pack, the client jar, the launcher asset index.
//! - `PACKPRUNE_CORPUS_SOURCES` (optional): usage sources as `<id>=<path>`, separated by commas.
//! - `PACKPRUNE_CORPUS_MAX_HEAP_MIB` (optional): the peak the run must stay under.
//!
//! The heap is counted by this binary's allocator, on one thread: the same count on every host and allocator, where
//! RSS adds the allocator's own retention and grows with the thread count.

use std::alloc::{GlobalAlloc, Layout, System};
use std::env;
use std::sync::atomic::{AtomicUsize, Ordering};

use camino::{Utf8Path, Utf8PathBuf};
use packprune_config::Profile;
use packprune_engine::run::{Request, SourceRequest, VanillaInputs, optimise};

struct Counting;

static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

// SAFETY: every method forwards to `System` with the caller's arguments and only adds atomic counting, so `System`'s
// guarantees hold unchanged.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: the caller upholds `GlobalAlloc::alloc`'s contract for `layout`.
        let block = unsafe { System.alloc(layout) };
        if !block.is_null() {
            grew(layout.size());
        }
        block
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        // SAFETY: the caller upholds `GlobalAlloc::alloc_zeroed`'s contract for `layout`.
        let block = unsafe { System.alloc_zeroed(layout) };
        if !block.is_null() {
            grew(layout.size());
        }
        block
    }

    unsafe fn dealloc(&self, block: *mut u8, layout: Layout) {
        // SAFETY: `block` was allocated by this allocator, so by `System`, with `layout`.
        unsafe { System.dealloc(block, layout) };
        LIVE.fetch_sub(layout.size(), Ordering::Relaxed);
    }

    unsafe fn realloc(&self, block: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        // SAFETY: `block` was allocated by `System` with `layout`, and the caller upholds `realloc`'s size contract.
        let moved = unsafe { System.realloc(block, layout, size) };
        if !moved.is_null() {
            if size >= layout.size() {
                grew(size - layout.size());
            } else {
                LIVE.fetch_sub(layout.size() - size, Ordering::Relaxed);
            }
        }
        moved
    }
}

fn grew(bytes: usize) {
    let live = LIVE.fetch_add(bytes, Ordering::Relaxed) + bytes;
    PEAK.fetch_max(live, Ordering::Relaxed);
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

const MIB: usize = 1 << 20;

fn var(name: &str) -> Utf8PathBuf {
    env::var(name)
        .unwrap_or_else(|_| panic!("set {name}"))
        .into()
}

fn sources() -> Vec<SourceRequest> {
    let Ok(list) = env::var("PACKPRUNE_CORPUS_SOURCES") else {
        return Vec::new();
    };
    list.split(',')
        .filter(|source| !source.is_empty())
        .map(|source| {
            let (id, path) = source
                .split_once('=')
                .expect("PACKPRUNE_CORPUS_SOURCES: <id>=<path>,...");
            SourceRequest {
                id: id.into(),
                path: path.into(),
            }
        })
        .collect()
}

#[test]
#[ignore = "needs a pack and a client jar: see the module docs"]
fn peak_heap() {
    let work = tempfile::tempdir().unwrap();
    let request = Request {
        input: var("PACKPRUNE_CORPUS_PACK"),
        output: Utf8Path::from_path(work.path()).unwrap().join("out.zip"),
        vanilla: VanillaInputs {
            client_jar: var("PACKPRUNE_CORPUS_JAR"),
            asset_index: Some(var("PACKPRUNE_CORPUS_ASSET_INDEX")),
        },
        sources: sources(),
        prune_unused: Vec::new(),
        profile: Profile {
            threads: Some(1),
            ..Profile::default()
        },
        limits: Default::default(),
        budgets: Default::default(),
        cache: None,
        expect: None,
    };
    let before = LIVE.load(Ordering::Relaxed);
    PEAK.store(before, Ordering::Relaxed);
    optimise(&request).unwrap();
    let peak = (PEAK.load(Ordering::Relaxed) - before) / MIB;
    println!("peak heap: {peak} MiB");
    if let Ok(max) = env::var("PACKPRUNE_CORPUS_MAX_HEAP_MIB") {
        let max: usize = max
            .parse()
            .expect("PACKPRUNE_CORPUS_MAX_HEAP_MIB: a number");
        assert!(peak <= max, "the run peaks at {peak} MiB, over {max} MiB");
    }
}
