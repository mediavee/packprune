use std::collections::BTreeSet;

use packprune_pack::json::{self, TokenKind};

/// `json` without the members at `removed`; only the containers above a removal are compacted, every other byte kept.
pub(super) fn remove(json: &[u8], removed: &BTreeSet<String>) -> Vec<u8> {
    edit(json, removed, None)
}

pub(super) fn edit(
    json: &[u8],
    removed: &BTreeSet<String>,
    added: Option<(&str, &str)>,
) -> Vec<u8> {
    let mut touched = BTreeSet::new();
    if added.is_some() {
        touched.insert(String::new());
    }
    for pointer in removed {
        let mut ancestor = pointer.as_str();
        while let Some((parent, _)) = ancestor.rsplit_once('/') {
            touched.insert(parent.to_owned());
            ancestor = parent;
        }
    }
    struct Container {
        pointer: String,
        start: usize,
        member: Option<usize>,
        compact: bool,
        kept: usize,
        out: Vec<u8>,
    }
    impl Container {
        fn item(&mut self, source: &[u8], start: usize, bytes: &[u8], drop: bool) {
            let member = self.member.take().unwrap_or(start);
            if !self.compact || drop {
                return;
            }
            if self.kept > 0 {
                self.out.push(b',');
            }
            self.kept += 1;
            self.out.extend_from_slice(&source[member..start]);
            self.out.extend_from_slice(bytes);
        }
    }
    let mut stack: Vec<Container> = Vec::new();
    let mut output = Vec::new();
    json::visit(json, |token, path| {
        let pointer = path.pointer().0.into_string();
        match token.kind {
            TokenKind::Name => {
                stack.last_mut().expect("member container").member = Some(token.span.start)
            }
            TokenKind::StartObject | TokenKind::StartArray => {
                stack.push(Container {
                    compact: touched.contains(&pointer),
                    pointer,
                    start: token.span.start,
                    member: None,
                    kept: 0,
                    out: vec![json[token.span.start]],
                });
            }
            TokenKind::EndObject | TokenKind::EndArray => {
                let mut container = stack.pop().expect("balanced by the tokenizer");
                let bytes = if container.compact {
                    if stack.is_empty()
                        && let Some((name, value)) = added
                    {
                        if container.kept > 0 {
                            container.out.push(b',');
                        }
                        let mut writer = json::Writer::new(&mut container.out);
                        writer.name(name);
                        writer.string(value);
                    }
                    container.out.push(json[token.span.start]);
                    container.out.as_slice()
                } else {
                    &json[container.start..token.span.end]
                };
                if let Some(parent) = stack.last_mut() {
                    parent.item(
                        json,
                        container.start,
                        bytes,
                        removed.contains(&container.pointer),
                    );
                } else {
                    output.extend_from_slice(&json[..container.start]);
                    output.extend_from_slice(bytes);
                    output.extend_from_slice(&json[token.span.end..]);
                }
            }
            _ => {
                if let Some(parent) = stack.last_mut() {
                    parent.item(
                        json,
                        token.span.start,
                        &json[token.span.clone()],
                        removed.contains(&pointer),
                    );
                }
            }
        }
    })
    .expect("model JSON was parsed before editing");
    output
}
