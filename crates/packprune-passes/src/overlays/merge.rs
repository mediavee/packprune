use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use packprune_pack::meta::{Format, next_format, previous_format};
use packprune_pack::{Blob, FormatRange, PhysPath};
use packprune_pass::{Edits, Loss, Pass, PassCx, PassError, Stage};

use super::entries::{Entry, Shape, read, replace, touches, union};

pub struct OverlayMerge;

impl Pass for OverlayMerge {
    fn name(&self) -> &'static str {
        "overlay-merge"
    }
    fn stage(&self) -> Stage {
        Stage::Overlays
    }
    fn loss(&self) -> Loss {
        Loss::Lossless
    }
    fn options_schema(&self) -> schemars::Schema {
        schemars::schema_for!(crate::overlays::json::NoOptions)
    }

    fn run(&self, cx: &PassCx<'_>) -> Result<Edits, PassError> {
        let mut edits = Edits::default();
        let path = phys("pack.mcmeta");
        let Some(mcmeta) = cx.pack.files.get(&path) else {
            return Ok(edits);
        };
        let mut merger = Merger::new(cx, read(&mcmeta.blob, self.name())?);
        merger.run();
        for from in &merger.removed {
            if merger.added.remove(from).is_none() {
                edits.removed.insert(phys(from));
            }
        }
        edits.added = merger
            .added
            .iter()
            .map(|(path, bytes)| (phys(path), Arc::clone(bytes)))
            .collect();
        edits.moves = merger
            .moves
            .iter()
            .map(|(from, to)| (phys(from), phys(to)))
            .collect();
        if merger.merges > 0 {
            edits.replaced.insert(
                path,
                replace(&mcmeta.blob, &merger.order, self.name())?.into(),
            );
        }
        let new = merger
            .order
            .iter()
            .filter(|entry| merger.created.contains(&entry.directory))
            .count();
        edits.changes.push(format!(
            "{} redundant copies removed in {} merges, into {new} new overlays",
            merger.copies, merger.merges
        ));
        let dropped = if merger.dropped.is_empty() {
            String::new()
        } else {
            format!(": {}", merger.dropped.join(", "))
        };
        edits.changes.push(format!(
            "{} overlays left empty and dropped{dropped}",
            merger.dropped.len()
        ));
        Ok(edits)
    }
}

fn phys(path: &str) -> PhysPath {
    PhysPath::new(path).expect("valid pack path")
}

struct Merger<'a> {
    held: BTreeMap<String, BTreeMap<String, Blob>>,
    file_count: BTreeMap<String, usize>,
    entries_of: BTreeMap<String, usize>,
    names: BTreeSet<String>,
    samples: BTreeSet<Format>,
    pinned: &'a BTreeSet<PhysPath>,
    removed: BTreeSet<String>,
    added: BTreeMap<String, Blob>,
    moves: BTreeMap<String, String>,
    order: Vec<Entry>,
    created: Vec<String>,
    dropped: Vec<String>,
    copies: usize,
    merges: usize,
}

impl<'a> Merger<'a> {
    fn new(cx: &'a PassCx<'_>, order: Vec<Entry>) -> Self {
        let mut merger = Self {
            held: BTreeMap::new(),
            file_count: BTreeMap::new(),
            entries_of: BTreeMap::new(),
            names: BTreeSet::new(),
            samples: BTreeSet::new(),
            pinned: cx.pinned,
            removed: BTreeSet::new(),
            added: BTreeMap::new(),
            moves: BTreeMap::new(),
            order,
            created: Vec::new(),
            dropped: Vec::new(),
            copies: 0,
            merges: 0,
        };
        for entry in &merger.order {
            merger.held.entry(entry.directory.clone()).or_default();
            *merger
                .entries_of
                .entry(entry.directory.clone())
                .or_default() += 1;
            merger.samples.extend([
                previous_format(entry.range.min),
                entry.range.min,
                entry.range.max,
                next_format(entry.range.max),
            ]);
        }
        for (path, entry) in &cx.pack.files {
            let (directory, rest) = path.as_str().split_once('/').unwrap_or((path.as_str(), ""));
            merger.names.insert(directory.to_owned());
            if let Some(held) = merger.held.get_mut(directory) {
                *merger.file_count.entry(directory.to_owned()).or_default() += 1;
                if rest.starts_with("assets/") {
                    held.insert(rest.to_owned(), Arc::clone(&entry.blob));
                }
            }
        }
        merger
    }

    fn run(&mut self) {
        let mut holders = BTreeMap::<String, usize>::new();
        for paths in self.held.values() {
            for path in paths.keys() {
                *holders.entry(path.clone()).or_default() += 1;
            }
        }
        for (path, count) in holders {
            if count > 1 {
                while self.merge_one(&path) {}
            }
        }
    }

    fn merge_one(&mut self, path: &str) -> bool {
        // Groups are tried in the order of their first entry in `self.order`: the first that merges wins.
        let mut groups: Vec<(Blob, bool, Vec<Entry>)> = Vec::new();
        for entry in &self.order {
            let Some(bytes) = self.held[&entry.directory].get(path) else {
                continue;
            };
            if self.entries_of[&entry.directory] != 1
                || !entry.plain()
                || self
                    .pinned
                    .contains(&phys(&format!("{}/{path}", entry.directory)))
            {
                continue;
            }
            if let Some(group) = groups
                .iter_mut()
                .find(|(b, legacy, _)| b == bytes && *legacy == entry.shape.legacy)
            {
                group.2.push(entry.clone());
            } else {
                groups.push((Arc::clone(bytes), entry.shape.legacy, vec![entry.clone()]));
            }
        }
        for (_, _, mut group) in groups {
            group.sort_by_key(|entry| (entry.range.min, entry.range.max));
            for length in (2..=group.len()).rev() {
                for members in group.windows(length) {
                    let mut range = members[0].range;
                    let follows = members[1..].iter().all(|entry| {
                        let follows = touches(range, entry.range);
                        range = union(range, entry.range);
                        follows
                    });
                    if follows && self.merge(path, members, range) {
                        return true;
                    }
                }
            }
        }
        false
    }

    fn merge(&mut self, path: &str, members: &[Entry], range: FormatRange) -> bool {
        let shape = Shape {
            legacy: members[0].shape.legacy,
            modern: members.iter().any(|entry| entry.shape.modern),
        };
        let bytes = Arc::clone(&self.held[&members[0].directory][path]);
        for directory in &self.created {
            if let Some(target) = self
                .order
                .iter()
                .find(|entry| &entry.directory == directory)
                && target.range == range
                && target.shape.legacy == shape.legacy
                && !self.held[directory].contains_key(path)
                && self.resolves_alike(path, members, target, &bytes, &self.order)
            {
                let target = target.clone();
                self.move_files(path, members, &target, bytes);
                return true;
            }
        }
        for gap in 0..=self.order.len() {
            let target = Entry::created(self.name(range), range, shape);
            let mut after = self.order.clone();
            after.insert(gap, target.clone());
            if self.resolves_alike(path, members, &target, &bytes, &after) {
                self.order.insert(gap, target.clone());
                self.created.push(target.directory.clone());
                self.held.insert(target.directory.clone(), BTreeMap::new());
                self.entries_of.insert(target.directory.clone(), 1);
                self.names.insert(target.directory.clone());
                self.move_files(path, members, &target, bytes);
                return true;
            }
        }
        false
    }

    fn resolves_alike(
        &self,
        path: &str,
        members: &[Entry],
        target: &Entry,
        bytes: &Blob,
        after: &[Entry],
    ) -> bool {
        let winner = |order: &[Entry], format: Format, changed: bool| -> Option<Blob> {
            order
                .iter()
                .filter(|entry| entry.range.contains(format))
                .filter_map(|entry| {
                    if changed && entry.directory == target.directory {
                        Some(Arc::clone(bytes))
                    } else if changed
                        && members
                            .iter()
                            .any(|member| member.directory == entry.directory)
                    {
                        None
                    } else {
                        self.held
                            .get(&entry.directory)
                            .and_then(|held| held.get(path))
                            .cloned()
                    }
                })
                .next_back()
        };
        self.samples
            .iter()
            .all(|format| winner(&self.order, *format, false) == winner(after, *format, true))
    }

    fn move_files(&mut self, path: &str, members: &[Entry], target: &Entry, bytes: Blob) {
        let to = format!("{}/{path}", target.directory);
        for member in members {
            self.held
                .get_mut(&member.directory)
                .expect("known directory")
                .remove(path);
            let from = format!("{}/{path}", member.directory);
            self.removed.insert(from.clone());
            let mut followed = false;
            for moved in self.moves.values_mut() {
                if *moved == from {
                    *moved = to.clone();
                    followed = true;
                }
            }
            if !followed {
                self.moves.insert(from, to.clone());
            }
            let count = self
                .file_count
                .get_mut(&member.directory)
                .expect("holds a file");
            *count -= 1;
            if *count == 0 {
                self.order
                    .retain(|entry| entry.directory != member.directory);
                self.dropped.push(member.directory.clone());
            }
        }
        self.held
            .get_mut(&target.directory)
            .expect("target exists")
            .insert(path.to_owned(), Arc::clone(&bytes));
        *self.file_count.entry(target.directory.clone()).or_default() += 1;
        self.added.insert(to, bytes);
        self.copies += members.len() - 1;
        self.merges += 1;
    }

    fn name(&self, range: FormatRange) -> String {
        let base = format!("packprune_{}_{}", range.min.0, range.max.0);
        let mut name = base.clone();
        let mut n = 2;
        while self.names.contains(&name) {
            name = format!("{base}_{n}");
            n += 1;
        }
        name
    }
}
