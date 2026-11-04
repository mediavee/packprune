//! The cached reference tokens: a pure function of a file's bytes and kind, so keyed on its content digest and kind,
//! never on its path. One memo serves every index a run builds or updates, and what the run asked is saved
//! for the next one.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use packprune_pack::{Digest, Entry, FileKind, Pointer};

use crate::{Computed, RawRef, Role, TokenSource, Tokens};

const MAGIC: &[u8; 8] = b"PPTOKEN1";

type Key = (Digest, FileKind);

/// What earlier runs saved, and what this run asked, which alone is saved.
#[derive(Default)]
pub struct TokenMemo {
    earlier: BTreeMap<Key, Tokens>,
    asked: Mutex<BTreeMap<Key, Tokens>>,
}

impl TokenSource for TokenMemo {
    fn tokens(&self, entry: &Entry, kind: FileKind) -> Tokens {
        let key = (entry.digest, kind);
        if let Some(tokens) = self.asked().get(&key) {
            return Arc::clone(tokens);
        }
        let tokens = match self.earlier.get(&key) {
            Some(tokens) => Arc::clone(tokens),
            None => Computed.tokens(entry, kind),
        };
        Arc::clone(self.asked().entry(key).or_insert(tokens))
    }
}

impl TokenMemo {
    fn asked(&self) -> MutexGuard<'_, BTreeMap<Key, Tokens>> {
        self.asked.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The tokens this run asked for.
    pub fn to_bytes(&self) -> Vec<u8> {
        let asked = self.asked();
        let mut out = Writer(MAGIC.to_vec());
        out.u32(asked.len() as u32);
        for ((digest, kind), tokens) in asked.iter() {
            out.0.extend_from_slice(&digest.0);
            out.0.push(kind_byte(*kind));
            match &**tokens {
                Ok(refs) => {
                    out.0.push(0);
                    out.u32(refs.len() as u32);
                    for token in refs {
                        out.str(&token.pointer.0);
                        out.u32(token.span.start);
                        out.u32(token.span.end);
                        out.str(&token.raw);
                        out.0.push(role_byte(token.role));
                    }
                }
                Err(why) => {
                    out.0.push(1);
                    out.str(why);
                }
            }
        }
        out.0
    }

    /// `None` for bytes this build did not write: the run then starts from an empty memo.
    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        let mut input = Reader(bytes.strip_prefix(MAGIC)?);
        let mut earlier = BTreeMap::new();
        for _ in 0..input.u32()? {
            let digest = Digest(input.take(32)?.try_into().ok()?);
            let kind = *KINDS.get(usize::from(input.u8()?))?;
            let tokens = match input.u8()? {
                0 => {
                    let count = input.u32()? as usize;
                    let mut refs = Vec::with_capacity(count.min(input.0.len()));
                    for _ in 0..count {
                        refs.push(RawRef {
                            pointer: Pointer(input.str()?.into()),
                            span: input.u32()?..input.u32()?,
                            raw: input.str()?.into(),
                            role: role_of(input.u8()?)?,
                        });
                    }
                    Ok(refs)
                }
                1 => Err(input.str()?.into()),
                _ => return None,
            };
            earlier.insert((digest, kind), Arc::new(tokens));
        }
        input.0.is_empty().then(|| Self {
            earlier,
            asked: Mutex::default(),
        })
    }
}

struct Writer(Vec<u8>);

impl Writer {
    fn u32(&mut self, value: u32) {
        self.0.extend_from_slice(&value.to_le_bytes());
    }

    fn str(&mut self, text: &str) {
        self.u32(text.len() as u32);
        self.0.extend_from_slice(text.as_bytes());
    }
}

struct Reader<'a>(&'a [u8]);

impl<'a> Reader<'a> {
    fn take(&mut self, count: usize) -> Option<&'a [u8]> {
        let (taken, rest) = self.0.split_at_checked(count)?;
        self.0 = rest;
        Some(taken)
    }

    fn u8(&mut self) -> Option<u8> {
        Some(self.take(1)?[0])
    }

    fn u32(&mut self) -> Option<u32> {
        Some(u32::from_le_bytes(self.take(4)?.try_into().ok()?))
    }

    fn str(&mut self) -> Option<&'a str> {
        let count = self.u32()? as usize;
        str::from_utf8(self.take(count)?).ok()
    }
}

/// Every kind, at its [`kind_byte`].
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

fn kind_byte(kind: FileKind) -> u8 {
    match kind {
        FileKind::PackMeta => 0,
        FileKind::ItemDefinition => 1,
        FileKind::Model => 2,
        FileKind::Texture => 3,
        FileKind::TextureMeta => 4,
        FileKind::Blockstate => 5,
        FileKind::Atlas => 6,
        FileKind::Equipment => 7,
        FileKind::Font => 8,
        FileKind::FontFile => 9,
        FileKind::SoundsJson => 10,
        FileKind::Sound => 11,
        FileKind::Lang => 12,
        FileKind::Shader => 13,
        FileKind::ShaderInclude => 14,
        FileKind::PostEffect => 15,
        FileKind::Particle => 16,
        FileKind::WaypointStyle => 17,
        FileKind::Text => 18,
        FileKind::Other => 19,
        FileKind::Unknown => 20,
    }
}

fn role_byte(role: Role) -> u8 {
    match role {
        Role::ModelParent => 0,
        Role::TextureSlot => 1,
        Role::Sprite => 2,
        Role::SoundName => 3,
        Role::ShaderImportQuoted => 4,
        Role::ShaderImportAngled => 5,
        Role::Other => 6,
    }
}

fn role_of(byte: u8) -> Option<Role> {
    Some(match byte {
        0 => Role::ModelParent,
        1 => Role::TextureSlot,
        2 => Role::Sprite,
        3 => Role::SoundName,
        4 => Role::ShaderImportQuoted,
        5 => Role::ShaderImportAngled,
        6 => Role::Other,
        _ => return None,
    })
}
