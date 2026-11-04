use std::fs::{self, File};
use std::path::Path;
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, SystemTime};

use camino::{Utf8Path, Utf8PathBuf};

use super::*;

const WORKER_ENV: &str = "PACKPRUNE_CACHE_TEST_WORKER";
const HEADER: usize = HEADER_LEN;

fn store_in(dir: &tempfile::TempDir, max_bytes: u64) -> Store {
    Store {
        dir: Utf8PathBuf::from_path_buf(dir.path().to_owned()).unwrap(),
        max_bytes,
    }
}

fn entry_files(store: &Store) -> Vec<(PathBuf, u64)> {
    let mut files = Vec::new();
    let Ok(shards) = fs::read_dir(store.dir.join(VERSION_DIR)) else {
        return files;
    };
    for shard in shards {
        for file in fs::read_dir(shard.unwrap().path()).unwrap() {
            let file = file.unwrap();
            if is_entry_name(file.file_name().to_str().unwrap()) {
                files.push((file.path(), file.metadata().unwrap().len()));
            }
        }
    }
    files
}

fn set_written(path: &Path, at: SystemTime) {
    File::options()
        .write(true)
        .open(path)
        .unwrap()
        .set_modified(at)
        .unwrap();
}

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn bytes(&mut self, max_len: usize) -> Vec<u8> {
        let len = self.next() as usize % (max_len + 1);
        (0..len).map(|_| self.next() as u8).collect()
    }

    fn digest(&mut self) -> [u8; 32] {
        std::array::from_fn(|_| self.next() as u8)
    }
}

#[test]
fn put_then_get_returns_the_value() {
    let dir = tempfile::tempdir().unwrap();
    let store = store_in(&dir, u64::MAX);
    let key = Key::derive("2.0.0", "step", b"", &[]);
    assert_eq!(store.get(&key), None);
    store.put(&key, b"value").unwrap();
    assert_eq!(store.get(&key).as_deref(), Some(&b"value"[..]));
    store.put(&key, b"").unwrap();
    assert_eq!(store.get(&key).as_deref(), Some(&b""[..]));
}

#[test]
fn entries_sit_at_v1_two_hex_sixty_two_hex() {
    let dir = tempfile::tempdir().unwrap();
    let store = store_in(&dir, u64::MAX);
    let key = Key([0xab; 32]);
    store.put(&key, b"x").unwrap();
    let path = store.dir.join(format!("v1/ab/{}", "ab".repeat(31)));
    assert!(path.is_file(), "{path}");
}

#[test]
fn keys_differ_when_any_field_changes() {
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
    for _ in 0..500 {
        let version = String::from_utf8_lossy(&rng.bytes(8)).into_owned();
        let step = String::from_utf8_lossy(&rng.bytes(8)).into_owned();
        let options = rng.bytes(16);
        let inputs: Vec<[u8; 32]> = (0..1 + rng.next() % 3).map(|_| rng.digest()).collect();
        let key = Key::derive(&version, &step, &options, &inputs);
        assert_eq!(key, Key::derive(&version, &step, &options, &inputs));

        assert_ne!(
            key,
            Key::derive(&format!("{version}x"), &step, &options, &inputs)
        );
        assert_ne!(
            key,
            Key::derive(&version, &format!("{step}x"), &options, &inputs)
        );
        let mut other = options.clone();
        other.push(0);
        assert_ne!(key, Key::derive(&version, &step, &other, &inputs));
        for i in 0..inputs.len() {
            let mut other = inputs.clone();
            other[i][(rng.next() % 32) as usize] ^= 1 << (rng.next() % 8);
            assert_ne!(key, Key::derive(&version, &step, &options, &other));
        }
        let mut longer = inputs.clone();
        longer.push(inputs[0]);
        assert_ne!(key, Key::derive(&version, &step, &options, &longer));
    }
}

#[test]
fn keys_differ_when_a_byte_moves_between_fields() {
    let base = Key::derive("ab", "c", b"", &[]);
    assert_ne!(base, Key::derive("a", "bc", b"", &[]));
    assert_ne!(base, Key::derive("a", "b", b"c", &[]));
    assert_ne!(
        Key::derive("v", "s", b"o", &[]),
        Key::derive("v", "so", b"", &[])
    );
    let (a, b) = ([1; 32], [2; 32]);
    assert_ne!(
        Key::derive("v", "s", b"", &[a, b]),
        Key::derive("v", "s", b"", &[b, a])
    );
}

#[test]
fn truncated_entry_is_a_miss_then_overwritten() {
    let dir = tempfile::tempdir().unwrap();
    let store = store_in(&dir, u64::MAX);
    let key = Key::derive("v", "s", b"", &[]);
    let value = vec![7u8; 300];
    store.put(&key, &value).unwrap();
    let path = store.entry_path(&key);
    let whole = fs::read(&path).unwrap();
    for len in 0..whole.len() {
        fs::write(&path, &whole[..len]).unwrap();
        assert_eq!(store.get(&key), None, "truncated to {len} bytes");
    }
    store.put(&key, b"fresh").unwrap();
    assert_eq!(store.get(&key).as_deref(), Some(&b"fresh"[..]));
}

#[test]
fn altered_entry_is_a_miss_then_overwritten() {
    let dir = tempfile::tempdir().unwrap();
    let store = store_in(&dir, u64::MAX);
    let key = Key::derive("v", "s", b"", &[]);
    store.put(&key, &[7u8; 64]).unwrap();
    let path = store.entry_path(&key);
    let whole = fs::read(&path).unwrap();
    for at in 0..whole.len() {
        let mut altered = whole.clone();
        altered[at] ^= 0x01;
        fs::write(&path, &altered).unwrap();
        assert_eq!(store.get(&key), None, "byte {at} flipped");
    }
    let mut longer = whole;
    longer.push(0);
    fs::write(&path, &longer).unwrap();
    assert_eq!(store.get(&key), None, "trailing byte");

    store.put(&key, b"fresh").unwrap();
    assert_eq!(store.get(&key).as_deref(), Some(&b"fresh"[..]));
}

#[test]
fn eviction_keeps_the_most_recently_written_under_max_bytes() {
    let dir = tempfile::tempdir().unwrap();
    let entry = (HEADER + 100) as u64;
    let store = store_in(&dir, 4 * entry + entry / 2);
    let keys: Vec<Key> = (0..10u8)
        .map(|i| Key::derive("v", "s", &[i], &[]))
        .collect();
    // Written out of key order, so the order of the directory listing cannot pass for the order of writes.
    let order = [3, 7, 0, 9, 1, 5, 2, 8, 4, 6];
    for (rank, &i) in order.iter().enumerate() {
        store.put(&keys[i], &[i as u8; 100]).unwrap();
        set_written(
            &store.entry_path(&keys[i]),
            SystemTime::UNIX_EPOCH + Duration::from_secs(1000 + rank as u64),
        );
    }

    store.evict().unwrap();

    let total: u64 = entry_files(&store).iter().map(|(_, len)| len).sum();
    assert!(total <= store.max_bytes, "{total} > {}", store.max_bytes);
    for (rank, &i) in order.iter().enumerate() {
        assert_eq!(
            store.get(&keys[i]).is_some(),
            rank >= 6,
            "entry written {rank}th"
        );
    }
}

#[test]
fn eviction_below_the_limit_removes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let store = store_in(&dir, u64::MAX);
    let key = Key::derive("v", "s", b"", &[]);
    store.put(&key, b"v").unwrap();
    store.evict().unwrap();
    assert!(store.get(&key).is_some());
    store_in(&tempfile::tempdir().unwrap(), 0).evict().unwrap();
}

#[test]
fn eviction_removes_stale_temporary_files_only() {
    let dir = tempfile::tempdir().unwrap();
    let store = store_in(&dir, u64::MAX);
    let key = Key::derive("v", "s", b"", &[]);
    store.put(&key, b"v").unwrap();
    let shard = store.entry_path(&key).parent().unwrap().to_owned();
    let (stale, fresh) = (shard.join(".tmp-stale"), shard.join(".tmp-fresh"));
    fs::write(&stale, b"crashed").unwrap();
    fs::write(&fresh, b"in flight").unwrap();
    set_written(&stale, SystemTime::now() - 2 * STALE_TEMP);

    store.evict().unwrap();

    assert!(!stale.exists());
    assert!(fresh.exists());
    assert!(store.get(&key).is_some());
}

/// 1..=255, so a value is told from any other writer's by its first byte.
fn pattern(writer: u8) -> Vec<u8> {
    vec![writer; 128 * 1024 + writer as usize * 1000]
}

fn is_whole(value: &[u8]) -> bool {
    value.first().is_some_and(|&w| value == pattern(w))
}

fn hammer(store: &Store, key: &Key, writers: std::ops::Range<u8>, rounds: usize) {
    thread::scope(|s| {
        for writer in writers {
            s.spawn(move || {
                let value = pattern(writer);
                for _ in 0..rounds {
                    store.put(key, &value).unwrap();
                }
            });
        }
    });
}

/// The re-executed test binary of a writer process; a plain run of the suite skips it.
#[test]
fn concurrent_worker() {
    let Some(setup) = std::env::var_os(WORKER_ENV) else {
        return;
    };
    let setup = setup.into_string().unwrap();
    let (dir, first) = setup.rsplit_once('|').unwrap();
    let first: u8 = first.parse().unwrap();
    let store = Store {
        dir: Utf8Path::new(dir).to_owned(),
        max_bytes: u64::MAX,
    };
    hammer(
        &store,
        &Key::derive("v", "shared", b"", &[]),
        first..first + 3,
        30,
    );
}

#[test]
fn concurrent_processes_and_threads_leave_one_whole_entry() {
    let dir = tempfile::tempdir().unwrap();
    let store = store_in(&dir, u64::MAX);
    let key = Key::derive("v", "shared", b"", &[]);
    let done = AtomicBool::new(false);

    thread::scope(|s| {
        let readers: Vec<_> = (0..4)
            .map(|_| {
                s.spawn(|| {
                    let mut seen = false;
                    while !done.load(Ordering::Acquire) {
                        match store.get(&key) {
                            Some(value) => {
                                assert!(
                                    is_whole(&value),
                                    "a partial entry of {} bytes",
                                    value.len()
                                );
                                seen = true;
                            }
                            None => assert!(
                                !seen,
                                "a miss after a hit: the entry was not replaced atomically"
                            ),
                        }
                    }
                })
            })
            .collect();

        let exe = std::env::current_exe().unwrap();
        let children: Vec<_> = (0..3u8)
            .map(|i| {
                Command::new(&exe)
                    .args(["--exact", "tests::concurrent_worker", "--nocapture"])
                    .env(WORKER_ENV, format!("{}|{}", store.dir, 10 + 10 * i))
                    .spawn()
                    .unwrap()
            })
            .collect();
        hammer(&store, &key, 1..4, 30);
        for mut child in children {
            assert!(child.wait().unwrap().success());
        }
        done.store(true, Ordering::Release);
        for reader in readers {
            reader.join().unwrap();
        }
    });

    assert!(is_whole(
        &store.get(&key).expect("an entry after the writers")
    ));
    let shard = store.entry_path(&key).parent().unwrap().to_owned();
    assert_eq!(
        fs::read_dir(shard).unwrap().count(),
        1,
        "a temporary file was left behind"
    );
}

#[test]
fn put_and_evict_race_without_error_or_partial_entry() {
    let dir = tempfile::tempdir().unwrap();
    let entry = (HEADER + pattern(1).len()) as u64;
    let store = store_in(&dir, 3 * entry);
    let keys: Vec<Key> = (0..12u8)
        .map(|i| Key::derive("v", "s", &[i], &[]))
        .collect();
    let done = AtomicBool::new(false);

    thread::scope(|s| {
        let others: Vec<_> = [
            s.spawn(|| {
                while !done.load(Ordering::Acquire) {
                    store.evict().unwrap();
                }
            }),
            s.spawn(|| {
                while !done.load(Ordering::Acquire) {
                    for key in &keys {
                        if let Some(value) = store.get(key) {
                            assert!(is_whole(&value));
                        }
                    }
                }
            }),
        ]
        .into();
        thread::scope(|writers| {
            for t in 0..3 {
                let (store, keys) = (&store, &keys);
                writers.spawn(move || {
                    for round in 0..20 {
                        let i = (round * 3 + t) % keys.len();
                        store.put(&keys[i], &pattern(1 + i as u8)).unwrap();
                    }
                });
            }
        });
        done.store(true, Ordering::Release);
        for other in others {
            other.join().unwrap();
        }
    });

    store.evict().unwrap();
    let total: u64 = entry_files(&store).iter().map(|(_, len)| len).sum();
    assert!(total <= store.max_bytes);
}

#[test]
fn the_override_replaces_the_configured_location() {
    let configured = Utf8Path::new(".packprune-cache");
    let base = Utf8Path::new("/srv/pack");
    assert_eq!(
        resolve_dir(None, configured, base),
        "/srv/pack/.packprune-cache"
    );
    assert_eq!(
        resolve_dir(Some(Utf8Path::new("/tmp/c")), configured, base),
        "/tmp/c"
    );
    assert_eq!(
        resolve_dir(None, Utf8Path::new("/abs/cache"), base),
        "/abs/cache"
    );
}

#[test]
fn an_unset_or_empty_variable_is_no_override() {
    use crate::location::parse_override;
    assert_eq!(parse_override(None).unwrap(), None);
    assert_eq!(parse_override(Some("".into())).unwrap(), None);
    assert_eq!(
        parse_override(Some("/tmp/c".into())).unwrap().as_deref(),
        Some(Utf8Path::new("/tmp/c"))
    );
}

#[test]
fn the_drop_in_base_is_the_executable_directory() {
    let exe = Utf8PathBuf::from_path_buf(std::env::current_exe().unwrap()).unwrap();
    assert_eq!(executable_dir().unwrap(), exe.parent().unwrap());
}
