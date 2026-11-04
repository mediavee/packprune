//! Cross-run memo keyed on resolution queries, on rustc's incremental red-green model
//! (<https://rustc-dev-guide.rust-lang.org/queries/incremental-compilation.html>).
//!
//! A resolution is a pure function of the answers to the [`Query`]s it asks its view and of its environment, so each
//! is recorded as a node: the facts it asked, and the nodes of the models and fonts it reused. A memoised digest is
//! reused only when every fact of its node, and of each node below, has the same answer in the new view; a lookup
//! that found nothing is a fact like any other.

#[cfg(test)]
mod tests;

use std::cell::RefCell;
use std::collections::{BTreeSet, HashMap};
use std::rc::Rc;
#[cfg(test)]
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, PoisonError, RwLock, RwLockReadGuard, RwLockWriteGuard};

use packprune_pack::FileKind;
use packprune_pack::meta::Format;
use rayon::prelude::*;

use crate::canon::{self, Digest};
use crate::resolver::{Ctx, Resolver, Side};

/// Digests kept per key and environment: one per side of a run (pruned, proved, written).
const CANDIDATES: usize = 4;
const MAGIC: &[u8; 8] = b"PPOMEMO1";

/// One question a resolution asks its view.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Query {
    /// The pack file a logical path resolves to in the view: its layer, content and kind.
    File(Box<str>),
    /// Whether vanilla has the path under the pack's `filter.block`.
    Vanilla(Box<str>),
    /// The client jar's bytes at the path.
    VanillaFile(Box<str>),
    /// Whether something outside the pack names the path.
    Named(Box<str>),
    /// What a sprite of an atlas is built from.
    Sprite(Box<str>, Box<str>),
    /// A file at the pack's root, outside every view.
    Root(Box<str>),
    /// Every file of the view with its layer and kind.
    Listing,
    /// Every sprite of every atlas of the view.
    Sprites,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Answer {
    Absent,
    Present,
    Digest(Digest),
    File {
        physical: Box<str>,
        content: Option<Digest>,
        kind: Option<FileKind>,
    },
}

impl Answer {
    pub(crate) fn of(present: bool) -> Self {
        if present {
            Answer::Present
        } else {
            Answer::Absent
        }
    }

    pub(crate) fn digest<T>(found: Option<(T, Digest)>) -> Self {
        found.map_or(Answer::Absent, |(_, digest)| Answer::Digest(digest))
    }
}

type Fact = (Query, Answer);

/// What one resolution asked, and the resolutions it reused.
#[derive(Clone)]
pub(crate) struct Trace(Rc<Frame>);

#[derive(Default)]
struct Frame {
    /// Numbers in the recorder's facts.
    facts: Vec<u32>,
    children: Vec<Trace>,
    /// The result depends on the order the resolver met things in: never memoised.
    poisoned: bool,
}

/// The facts one resolver met, each once, and the frames of the resolutions in progress, innermost last.
#[derive(Default)]
pub(crate) struct Recorder {
    stack: RefCell<Vec<Frame>>,
    facts: RefCell<Vec<Fact>>,
    numbers: RefCell<HashMap<Fact, u32>>,
}

impl Recorder {
    pub(crate) fn begin(&self) {
        self.stack.borrow_mut().push(Frame::default());
    }

    pub(crate) fn end(&self) -> Trace {
        let frame = self.stack.borrow_mut().pop().expect("a frame begun");
        Trace(Rc::new(frame))
    }

    pub(crate) fn fact(&self, query: Query, answer: Answer) {
        let mut stack = self.stack.borrow_mut();
        let Some(frame) = stack.last_mut() else {
            return;
        };
        let fact = (query, answer);
        let mut numbers = self.numbers.borrow_mut();
        let number = match numbers.get(&fact) {
            Some(&number) => number,
            None => {
                let mut facts = self.facts.borrow_mut();
                let number = facts.len() as u32;
                facts.push(fact.clone());
                numbers.insert(fact, number);
                number
            }
        };
        frame.facts.push(number);
    }

    pub(crate) fn reuse(&self, trace: &Trace) {
        if let Some(frame) = self.stack.borrow_mut().last_mut() {
            frame.poisoned |= trace.0.poisoned;
            frame.children.push(trace.clone());
        }
    }

    pub(crate) fn poison(&self) {
        if let Some(frame) = self.stack.borrow_mut().last_mut() {
            frame.poisoned = true;
        }
    }
}

/// The nodes one resolver recorded, over its recorder's facts, to be merged into the memo.
struct Recorded {
    facts: Vec<Fact>,
    nodes: Vec<(Vec<u32>, Vec<u32>)>,
}

/// Numbers the traces of one resolver, each frame once, children first.
#[derive(Default)]
struct Numbering {
    nodes: Vec<(Vec<u32>, Vec<u32>)>,
    by_frame: HashMap<usize, u32>,
    /// Every trace numbered stays alive until [`Self::seal`]: a frame freed earlier could leave its address, and so
    /// its number, to another.
    alive: Vec<Trace>,
}

impl Numbering {
    /// The local number of `trace`'s node; `None` when poisoned.
    fn add(&mut self, trace: &Trace) -> Option<u32> {
        if trace.0.poisoned {
            return None;
        }
        let at = Rc::as_ptr(&trace.0) as usize;
        if let Some(&id) = self.by_frame.get(&at) {
            return Some(id);
        }
        self.alive.push(trace.clone());
        let children = trace
            .0
            .children
            .iter()
            .map(|child| self.add(child))
            .collect::<Option<Vec<u32>>>()?;
        let id = self.nodes.len() as u32;
        self.nodes.push((trace.0.facts.clone(), children));
        self.by_frame.insert(at, id);
        Some(id)
    }

    fn seal(self, recorder: Recorder) -> Recorded {
        Recorded {
            facts: recorder.facts.into_inner(),
            nodes: self.nodes,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct Node {
    facts: Box<[u32]>,
    children: Box<[u32]>,
}

struct Candidate {
    node: u32,
    digest: Option<Digest>,
    /// Reused or recorded by this process: saved for the next.
    used: AtomicBool,
}

struct KeySet {
    node: u32,
    keys: Box<[u32]>,
    used: AtomicBool,
}

/// `(environment, key)`: the environment is what a resolution reads outside its facts.
type Slot = (Digest, u32);

#[derive(Default)]
struct Tables {
    facts: Vec<Arc<Fact>>,
    fact_ids: HashMap<Arc<Fact>, u32>,
    /// Children always before their parents.
    nodes: Vec<Arc<Node>>,
    node_ids: HashMap<Arc<Node>, u32>,
    keys: Vec<Arc<str>>,
    key_ids: HashMap<Arc<str>, u32>,
    /// Most recently used first.
    entries: HashMap<Slot, Vec<Candidate>>,
    key_sets: HashMap<Digest, Vec<KeySet>>,
}

impl Tables {
    fn fact(&mut self, fact: &Fact) -> u32 {
        if let Some(&id) = self.fact_ids.get(fact) {
            return id;
        }
        let id = self.facts.len() as u32;
        let fact = Arc::new(fact.clone());
        self.facts.push(Arc::clone(&fact));
        self.fact_ids.insert(fact, id);
        id
    }

    fn node(&mut self, node: Node) -> u32 {
        if let Some(&id) = self.node_ids.get(&node) {
            return id;
        }
        let id = self.nodes.len() as u32;
        let node = Arc::new(node);
        self.nodes.push(Arc::clone(&node));
        self.node_ids.insert(node, id);
        id
    }

    fn key(&mut self, key: &str) -> u32 {
        if let Some(&id) = self.key_ids.get(key) {
            return id;
        }
        let id = self.keys.len() as u32;
        let key: Arc<str> = key.into();
        self.keys.push(Arc::clone(&key));
        self.key_ids.insert(key, id);
        id
    }

    /// The global number of each node `recorded` holds.
    fn merge(&mut self, recorded: &Recorded) -> Vec<u32> {
        let facts: Vec<u32> = recorded.facts.iter().map(|fact| self.fact(fact)).collect();
        let mut nodes = Vec::with_capacity(recorded.nodes.len());
        for (local_facts, local_children) in &recorded.nodes {
            let mut node_facts: Vec<u32> = local_facts.iter().map(|&f| facts[f as usize]).collect();
            node_facts.sort_unstable();
            node_facts.dedup();
            let mut children: Vec<u32> =
                local_children.iter().map(|&c| nodes[c as usize]).collect();
            children.sort_unstable();
            children.dedup();
            let id = self.node(Node {
                facts: node_facts.into(),
                children: children.into(),
            });
            nodes.push(id);
        }
        nodes
    }
}

/// The memo of one process: what earlier runs recorded, and what this one used or recorded, which alone is saved.
pub struct Memo {
    tables: RwLock<Tables>,
    hits: AtomicU64,
    misses: AtomicU64,
    /// Keys on the files a resolution read only, a design tests show reuses wrong digests.
    #[cfg(test)]
    files_read: bool,
    /// Every hit resolved again and checked against its digest, for tests.
    #[cfg(test)]
    pub(crate) stale: Mutex<Vec<String>>,
}

impl Default for Memo {
    fn default() -> Self {
        Self::new()
    }
}

impl Memo {
    pub fn new() -> Self {
        Self::with_tables(Tables::default())
    }

    fn with_tables(tables: Tables) -> Self {
        Self {
            tables: RwLock::new(tables),
            hits: AtomicU64::new(0),
            misses: AtomicU64::new(0),
            #[cfg(test)]
            files_read: false,
            #[cfg(test)]
            stale: Mutex::new(Vec::new()),
        }
    }

    /// Key digests reused and resolved so far.
    pub fn counts(&self) -> (u64, u64) {
        (
            self.hits.load(Ordering::Relaxed),
            self.misses.load(Ordering::Relaxed),
        )
    }

    /// Records the digest of each key in each view of `side`, reusing every memoised one whose facts still hold.
    /// Each view checks the memo under a read lock, then resolves what missed and merges it at once, so only the
    /// views in progress hold recordings.
    pub(crate) fn digests(
        &self,
        ctx: &Ctx<'_>,
        side: &Side<'_>,
        wanted: BTreeSet<String>,
        only_wanted: bool,
    ) -> (Vec<String>, Vec<Vec<Option<Digest>>>) {
        let views = side.views.len();
        let envs: Vec<Digest> = side
            .views
            .iter()
            .map(|state| environment(ctx, state.format))
            .collect();
        let mut keys = wanted;
        if !only_wanted {
            let found: Vec<Option<BTreeSet<String>>> = {
                let tables = self.read();
                (0..views)
                    .into_par_iter()
                    .map(|view| self.memoised_keys(&tables, ctx, side, view, envs[view]))
                    .collect()
            };
            let per_view: Vec<BTreeSet<String>> = found
                .into_par_iter()
                .enumerate()
                .map(|(view, found)| match found {
                    Some(keys) => keys,
                    None => self.enumerate(ctx, side, view, envs[view]),
                })
                .collect();
            per_view.into_iter().for_each(|view| keys.extend(view));
        }
        let keys: Vec<String> = keys.into_iter().collect();
        let checked: Vec<(Vec<Option<Digest>>, Vec<usize>)> = {
            let tables = self.read();
            (0..views)
                .into_par_iter()
                .map(|view| self.memoised(&tables, ctx, side, view, envs[view], &keys))
                .collect()
        };
        let values = checked
            .into_par_iter()
            .enumerate()
            .map(|(view, (values, missed))| {
                self.resolve(ctx, side, view, envs[view], &keys, values, missed)
            })
            .collect();
        (keys, values)
    }

    /// The keys of a view as a memoised enumeration whose facts hold, marked used.
    fn memoised_keys(
        &self,
        tables: &Tables,
        ctx: &Ctx<'_>,
        side: &Side<'_>,
        view: usize,
        env: Digest,
    ) -> Option<BTreeSet<String>> {
        let resolver = Resolver::new(ctx, side, view);
        let mut check = Check::new(self, tables, &resolver);
        let set = tables
            .key_sets
            .get(&env)?
            .iter()
            .find(|set| check.node(set.node))?;
        set.used.store(true, Ordering::Relaxed);
        Some(
            set.keys
                .iter()
                .map(|&key| tables.keys[key as usize].to_string())
                .collect(),
        )
    }

    /// The keys of a view enumerated again, the enumeration recorded.
    fn enumerate(
        &self,
        ctx: &Ctx<'_>,
        side: &Side<'_>,
        view: usize,
        env: Digest,
    ) -> BTreeSet<String> {
        let recorder = Recorder::default();
        let resolver = Resolver::recording(ctx, side, view, &recorder);
        recorder.begin();
        let keys = resolver.keys();
        let trace = recorder.end();
        drop(resolver);
        let mut numbering = Numbering::default();
        let node = numbering.add(&trace);
        drop(trace);
        let Some(node) = node else {
            return keys;
        };
        let recorded = numbering.seal(recorder);
        let mut tables = self.write();
        let node = tables.merge(&recorded)[node as usize];
        let ids: Box<[u32]> = keys.iter().map(|key| tables.key(key)).collect();
        let sets = tables.key_sets.entry(env).or_default();
        sets.retain(|set| set.node != node);
        sets.insert(
            0,
            KeySet {
                node,
                keys: ids,
                used: AtomicBool::new(true),
            },
        );
        bound(sets, |set| set.used.load(Ordering::Relaxed));
        keys
    }

    /// The digest of each key whose memoised candidate holds in the view, marked used, and the keys that missed.
    fn memoised(
        &self,
        tables: &Tables,
        ctx: &Ctx<'_>,
        side: &Side<'_>,
        view: usize,
        env: Digest,
        keys: &[String],
    ) -> (Vec<Option<Digest>>, Vec<usize>) {
        let resolver = Resolver::new(ctx, side, view);
        let mut check = Check::new(self, tables, &resolver);
        let mut values = vec![None; keys.len()];
        let mut missed = Vec::new();
        #[cfg(test)]
        let mut hits = Vec::new();
        for (index, key) in keys.iter().enumerate() {
            let hit = tables
                .key_ids
                .get(key.as_str())
                .and_then(|&id| tables.entries.get(&(env, id)))
                .and_then(|candidates| candidates.iter().find(|c| check.node(c.node)));
            match hit {
                Some(candidate) => {
                    candidate.used.store(true, Ordering::Relaxed);
                    values[index] = candidate.digest;
                    #[cfg(test)]
                    hits.push(index);
                }
                None => missed.push(index),
            }
        }
        drop(check);
        self.hits
            .fetch_add((keys.len() - missed.len()) as u64, Ordering::Relaxed);
        self.misses
            .fetch_add(missed.len() as u64, Ordering::Relaxed);
        #[cfg(test)]
        self.check_hits(&resolver, keys, &values, &hits);
        (values, missed)
    }

    /// `values` with the keys that missed resolved, their resolutions recorded.
    #[expect(
        clippy::too_many_arguments,
        reason = "one view's lookup state, passed down from `digests`"
    )]
    fn resolve(
        &self,
        ctx: &Ctx<'_>,
        side: &Side<'_>,
        view: usize,
        env: Digest,
        keys: &[String],
        mut values: Vec<Option<Digest>>,
        missed: Vec<usize>,
    ) -> Vec<Option<Digest>> {
        if missed.is_empty() {
            return values;
        }
        let recorder = Recorder::default();
        let resolver = Resolver::recording(ctx, side, view, &recorder);
        let mut numbering = Numbering::default();
        let mut recorded = Vec::new();
        for index in missed {
            recorder.begin();
            let digest = resolver
                .resolve(&keys[index])
                .map(|descriptor| canon::digest(&descriptor));
            let trace = recorder.end();
            values[index] = digest;
            if let Some(node) = numbering.add(&trace) {
                recorded.push((index, node));
            }
        }
        drop(resolver);
        let nodes = numbering.seal(recorder);
        let mut tables = self.write();
        let ids = tables.merge(&nodes);
        for (index, local) in recorded {
            let slot = (env, tables.key(&keys[index]));
            let candidates = tables.entries.entry(slot).or_default();
            let (node, digest) = (ids[local as usize], values[index]);
            candidates.retain(|c| c.node != node || c.digest != digest);
            candidates.insert(
                0,
                Candidate {
                    node,
                    digest,
                    used: AtomicBool::new(true),
                },
            );
            bound(candidates, |c| c.used.load(Ordering::Relaxed));
        }
        values
    }

    #[cfg(test)]
    fn check_hits(
        &self,
        resolver: &Resolver<'_>,
        keys: &[String],
        values: &[Option<Digest>],
        hits: &[usize],
    ) {
        for &index in hits {
            let fresh = resolver
                .resolve(&keys[index])
                .map(|descriptor| canon::digest(&descriptor));
            if fresh != values[index] {
                self.stale
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .push(keys[index].clone());
            }
        }
    }

    fn read(&self) -> RwLockReadGuard<'_, Tables> {
        self.tables.read().unwrap_or_else(PoisonError::into_inner)
    }

    fn write(&self) -> RwLockWriteGuard<'_, Tables> {
        self.tables.write().unwrap_or_else(PoisonError::into_inner)
    }

    #[cfg(test)]
    pub(crate) fn keyed_on_files_read() -> Self {
        Self {
            files_read: true,
            ..Self::new()
        }
    }
}

/// At most [`CANDIDATES`], the oldest unused out first: what this process used is kept for the next.
fn bound<T>(candidates: &mut Vec<T>, used: impl Fn(&T) -> bool) {
    while candidates.len() > CANDIDATES {
        let at = candidates
            .iter()
            .rposition(|c| !used(c))
            .unwrap_or(candidates.len() - 1);
        candidates.remove(at);
    }
}

/// What a resolution reads beside its facts: the view's format (atlas files or stitched model textures) and the
/// vanilla client's version (its rules) and whether its sound files are known.
fn environment(ctx: &Ctx<'_>, format: Format) -> Digest {
    let mut hasher = blake3::Hasher::new_derive_key("packprune oracle memo environment v1");
    hasher.update(&format.0.to_le_bytes());
    hasher.update(&format.1.to_le_bytes());
    hasher.update(ctx.vanilla.version.as_bytes());
    hasher.update(&[u8::from(ctx.vanilla.sound_files)]);
    canon::truncate(hasher.finalize())
}

/// Which memoised nodes still hold in one view: each fact and node checked once.
struct Check<'a> {
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "read only by the test-only checks")
    )]
    memo: &'a Memo,
    tables: &'a Tables,
    resolver: &'a Resolver<'a>,
    facts: Vec<u8>,
    nodes: Vec<u8>,
}

const UNKNOWN: u8 = 0;
const HOLDS: u8 = 1;
const FAILS: u8 = 2;

impl<'a> Check<'a> {
    fn new(memo: &'a Memo, tables: &'a Tables, resolver: &'a Resolver<'a>) -> Self {
        Self {
            memo,
            tables,
            resolver,
            facts: vec![UNKNOWN; tables.facts.len()],
            nodes: vec![UNKNOWN; tables.nodes.len()],
        }
    }

    fn node(&mut self, id: u32) -> bool {
        match self.nodes[id as usize] {
            HOLDS => return true,
            FAILS => return false,
            _ => {}
        }
        let node = &self.tables.nodes[id as usize];
        let holds = node.facts.iter().all(|&fact| self.fact(fact))
            && node.children.iter().all(|&child| self.node(child));
        self.nodes[id as usize] = if holds { HOLDS } else { FAILS };
        holds
    }

    fn fact(&mut self, id: u32) -> bool {
        match self.facts[id as usize] {
            HOLDS => return true,
            FAILS => return false,
            _ => {}
        }
        let (query, answer) = &*self.tables.facts[id as usize];
        let holds = self.holds(query, answer);
        self.facts[id as usize] = if holds { HOLDS } else { FAILS };
        holds
    }

    #[cfg(not(test))]
    fn holds(&self, query: &Query, answer: &Answer) -> bool {
        self.resolver.holds(query, answer)
    }

    #[cfg(test)]
    fn holds(&self, query: &Query, answer: &Answer) -> bool {
        if !self.memo.files_read {
            let holds = self.resolver.holds(query, answer);
            assert_eq!(holds, self.resolver.answer(query) == *answer, "{query:?}");
            return holds;
        }
        // Only the files read count, by their own path and content: lookups that found nothing, layer choices,
        // vanilla's presence, names and sprites are not inputs.
        match (query, answer) {
            (
                Query::File(_),
                Answer::File {
                    physical, content, ..
                },
            ) => self.resolver.content(physical) == *content,
            (Query::VanillaFile(_) | Query::Root(_), Answer::Digest(_)) => {
                self.resolver.answer(query) == *answer
            }
            _ => true,
        }
    }
}

/// Serialisation: what this process used or recorded, with the nodes and facts they reach, renumbered.
impl Memo {
    /// The memo as bytes for the next run: only what this process used or recorded.
    pub fn to_bytes(&self) -> Vec<u8> {
        let tables = self.read();
        let mut nodes: HashMap<u32, u32> = HashMap::new();
        let mut node_order: Vec<u32> = Vec::new();
        let mut facts: HashMap<u32, u32> = HashMap::new();
        let mut fact_order: Vec<u32> = Vec::new();
        let mut keys: HashMap<u32, u32> = HashMap::new();
        let mut key_order: Vec<u32> = Vec::new();
        fn keep(
            tables: &Tables,
            id: u32,
            nodes: &mut HashMap<u32, u32>,
            order: &mut Vec<u32>,
            facts: &mut HashMap<u32, u32>,
            fact_order: &mut Vec<u32>,
        ) -> u32 {
            if let Some(&kept) = nodes.get(&id) {
                return kept;
            }
            let node = &tables.nodes[id as usize];
            for &child in &node.children {
                keep(tables, child, nodes, order, facts, fact_order);
            }
            for &fact in &node.facts {
                facts.entry(fact).or_insert_with(|| {
                    fact_order.push(fact);
                    fact_order.len() as u32 - 1
                });
            }
            let kept = order.len() as u32;
            order.push(id);
            nodes.insert(id, kept);
            kept
        }
        let mut key_of = |key: u32, keys: &mut HashMap<u32, u32>| {
            *keys.entry(key).or_insert_with(|| {
                key_order.push(key);
                key_order.len() as u32 - 1
            })
        };
        let mut entries = Vec::new();
        let mut slots: Vec<&Slot> = tables.entries.keys().collect();
        slots.sort_unstable();
        for slot in slots {
            let used: Vec<&Candidate> = tables.entries[slot]
                .iter()
                .filter(|c| c.used.load(Ordering::Relaxed))
                .collect();
            if used.is_empty() {
                continue;
            }
            let key = key_of(slot.1, &mut keys);
            let used: Vec<(u32, Option<Digest>)> = used
                .iter()
                .map(|c| {
                    let node = keep(
                        &tables,
                        c.node,
                        &mut nodes,
                        &mut node_order,
                        &mut facts,
                        &mut fact_order,
                    );
                    (node, c.digest)
                })
                .collect();
            entries.push((slot.0, key, used));
        }
        let mut sets = Vec::new();
        let mut envs: Vec<&Digest> = tables.key_sets.keys().collect();
        envs.sort_unstable();
        for env in envs {
            for set in tables.key_sets[env]
                .iter()
                .filter(|set| set.used.load(Ordering::Relaxed))
            {
                let node = keep(
                    &tables,
                    set.node,
                    &mut nodes,
                    &mut node_order,
                    &mut facts,
                    &mut fact_order,
                );
                let ids: Vec<u32> = set.keys.iter().map(|&key| key_of(key, &mut keys)).collect();
                sets.push((*env, node, ids));
            }
        }

        let mut out = Vec::new();
        out.extend_from_slice(MAGIC);
        put_u32(&mut out, fact_order.len() as u32);
        for &fact in &fact_order {
            let (query, answer) = &*tables.facts[fact as usize];
            put_query(&mut out, query);
            put_answer(&mut out, answer);
        }
        put_u32(&mut out, node_order.len() as u32);
        for &id in &node_order {
            let node = &tables.nodes[id as usize];
            put_u32(&mut out, node.facts.len() as u32);
            for fact in &node.facts {
                put_u32(&mut out, facts[fact]);
            }
            put_u32(&mut out, node.children.len() as u32);
            for child in &node.children {
                put_u32(&mut out, nodes[child]);
            }
        }
        put_u32(&mut out, key_order.len() as u32);
        for &key in &key_order {
            put_str(&mut out, &tables.keys[key as usize]);
        }
        put_u32(&mut out, entries.len() as u32);
        for (env, key, candidates) in entries {
            out.extend_from_slice(&env);
            put_u32(&mut out, key);
            put_u32(&mut out, candidates.len() as u32);
            for (node, digest) in candidates {
                put_u32(&mut out, node);
                put_digest(&mut out, digest.as_ref());
            }
        }
        put_u32(&mut out, sets.len() as u32);
        for (env, node, ids) in sets {
            out.extend_from_slice(&env);
            put_u32(&mut out, node);
            put_u32(&mut out, ids.len() as u32);
            for id in ids {
                put_u32(&mut out, id);
            }
        }
        out
    }

    /// The memo [`Self::to_bytes`] wrote; `None` for bytes of another format or cut short, which the caller treats
    /// as no memo.
    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        let mut input = Reader { bytes, at: 0 };
        if input.take(MAGIC.len())? != MAGIC {
            return None;
        }
        let mut tables = Tables::default();
        for _ in 0..input.u32()? {
            let fact = (input.query()?, input.answer()?);
            if tables.fact(&fact) as usize != tables.facts.len() - 1 {
                return None;
            }
        }
        for _ in 0..input.u32()? {
            let facts = input.ids(tables.facts.len())?;
            let children = input.ids(tables.nodes.len())?;
            let node = Node {
                facts: facts.into(),
                children: children.into(),
            };
            if tables.node(node) as usize != tables.nodes.len() - 1 {
                return None;
            }
        }
        for _ in 0..input.u32()? {
            let key = input.str()?;
            if tables.key(&key) as usize != tables.keys.len() - 1 {
                return None;
            }
        }
        for _ in 0..input.u32()? {
            let env = input.digest()?;
            let key = input.id(tables.keys.len())?;
            let mut candidates = Vec::new();
            for _ in 0..input.u32()? {
                let node = input.id(tables.nodes.len())?;
                let digest = input.optional_digest()?;
                candidates.push(Candidate {
                    node,
                    digest,
                    used: AtomicBool::new(false),
                });
            }
            tables.entries.insert((env, key), candidates);
        }
        for _ in 0..input.u32()? {
            let env = input.digest()?;
            let node = input.id(tables.nodes.len())?;
            let keys = input.ids(tables.keys.len())?;
            tables.key_sets.entry(env).or_default().push(KeySet {
                node,
                keys: keys.into(),
                used: AtomicBool::new(false),
            });
        }
        (input.at == bytes.len()).then(|| Self::with_tables(tables))
    }
}

fn put_u32(out: &mut Vec<u8>, value: u32) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn put_str(out: &mut Vec<u8>, value: &str) {
    put_u32(out, value.len() as u32);
    out.extend_from_slice(value.as_bytes());
}

fn put_digest(out: &mut Vec<u8>, digest: Option<&Digest>) {
    match digest {
        Some(digest) => {
            out.push(1);
            out.extend_from_slice(digest);
        }
        None => out.push(0),
    }
}

fn put_query(out: &mut Vec<u8>, query: &Query) {
    match query {
        Query::File(path) => (out.push(0), put_str(out, path)).1,
        Query::Vanilla(path) => (out.push(1), put_str(out, path)).1,
        Query::VanillaFile(path) => (out.push(2), put_str(out, path)).1,
        Query::Named(path) => (out.push(3), put_str(out, path)).1,
        Query::Sprite(atlas, sprite) => {
            out.push(4);
            put_str(out, atlas);
            put_str(out, sprite);
        }
        Query::Root(path) => (out.push(5), put_str(out, path)).1,
        Query::Listing => out.push(6),
        Query::Sprites => out.push(7),
    }
}

fn put_answer(out: &mut Vec<u8>, answer: &Answer) {
    match answer {
        Answer::Absent => out.push(0),
        Answer::Present => out.push(1),
        Answer::Digest(digest) => {
            out.push(2);
            out.extend_from_slice(digest);
        }
        Answer::File {
            physical,
            content,
            kind,
        } => {
            out.push(3);
            put_str(out, physical);
            put_digest(out, content.as_ref());
            out.push(kind.map_or(u8::MAX, |kind| kind as u8));
        }
    }
}

struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl Reader<'_> {
    fn take(&mut self, len: usize) -> Option<&[u8]> {
        let end = self.at.checked_add(len)?;
        let out = self.bytes.get(self.at..end)?;
        self.at = end;
        Some(out)
    }

    fn u8(&mut self) -> Option<u8> {
        Some(self.take(1)?[0])
    }

    fn u32(&mut self) -> Option<u32> {
        Some(u32::from_le_bytes(self.take(4)?.try_into().ok()?))
    }

    fn id(&mut self, below: usize) -> Option<u32> {
        self.u32().filter(|&id| (id as usize) < below)
    }

    fn ids(&mut self, below: usize) -> Option<Vec<u32>> {
        let len = self.u32()? as usize;
        // Every id takes four bytes: a length past the end is refused before allocating.
        if len > (self.bytes.len() - self.at) / 4 {
            return None;
        }
        (0..len).map(|_| self.id(below)).collect()
    }

    fn str(&mut self) -> Option<Box<str>> {
        let len = self.u32()? as usize;
        std::str::from_utf8(self.take(len)?).ok().map(Into::into)
    }

    fn digest(&mut self) -> Option<Digest> {
        self.take(16)?.try_into().ok()
    }

    fn optional_digest(&mut self) -> Option<Option<Digest>> {
        match self.u8()? {
            0 => Some(None),
            1 => Some(Some(self.digest()?)),
            _ => None,
        }
    }

    fn query(&mut self) -> Option<Query> {
        Some(match self.u8()? {
            0 => Query::File(self.str()?),
            1 => Query::Vanilla(self.str()?),
            2 => Query::VanillaFile(self.str()?),
            3 => Query::Named(self.str()?),
            4 => Query::Sprite(self.str()?, self.str()?),
            5 => Query::Root(self.str()?),
            6 => Query::Listing,
            7 => Query::Sprites,
            _ => return None,
        })
    }

    fn answer(&mut self) -> Option<Answer> {
        Some(match self.u8()? {
            0 => Answer::Absent,
            1 => Answer::Present,
            2 => Answer::Digest(self.digest()?),
            3 => Answer::File {
                physical: self.str()?,
                content: self.optional_digest()?,
                kind: match self.u8()? {
                    u8::MAX => None,
                    kind => Some(*KINDS.iter().find(|known| **known as u8 == kind)?),
                },
            },
            _ => return None,
        })
    }
}

/// Every [`FileKind`], read back by its discriminant.
const KINDS: [FileKind; 21] = [
    FileKind::PackMeta,
    FileKind::ItemDefinition,
    FileKind::Model,
    FileKind::Texture,
    FileKind::TextureMeta,
    FileKind::Blockstate,
    FileKind::Atlas,
    FileKind::Equipment,
    FileKind::Font,
    FileKind::FontFile,
    FileKind::SoundsJson,
    FileKind::Sound,
    FileKind::Lang,
    FileKind::Shader,
    FileKind::ShaderInclude,
    FileKind::PostEffect,
    FileKind::Particle,
    FileKind::WaypointStyle,
    FileKind::Text,
    FileKind::Other,
    FileKind::Unknown,
];
