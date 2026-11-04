use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, LazyLock},
};

use packprune_graph::SourceError;
use packprune_pack::pattern::JavaPattern;
use saphyr_parser::{Event, Parser, ScalarStyle, Tag};

use crate::refused;

const CODE_POINT_LIMIT: usize = 3 * 1024 * 1024;
const MAX_COLLECTION_ALIASES: usize = 50;
// snakeyaml-engine sets no nesting limit: this one keeps a hostile file from exhausting the stack.
const MAX_DEPTH: usize = 4096;

static NUMBER: LazyLock<JavaPattern> = LazyLock::new(|| {
    JavaPattern::new(r"^(?:-?(?:0|[1-9][0-9]*)(?:\.[0-9]*)?(?:[eE][-+]?[0-9]+)?|-?\.inf|\.nan)$")
        .expect("JSON schema number pattern")
});
static ENV: LazyLock<JavaPattern> = LazyLock::new(|| {
    JavaPattern::new(r"^\$\{\s*(?:(\w+)(?:(:?[-?])(\w+)?)?)\s*\}$")
        .expect("JSON schema environment pattern")
});

#[derive(Clone, Debug)]
pub(crate) struct Node {
    pub line: usize,
    pub key_line: usize,
    pub value: Arc<Value>,
    merge_key: bool,
    mapping_error: Option<(usize, &'static str)>,
    depth: usize,
}

#[derive(Clone, Debug)]
pub(crate) enum Value {
    Scalar(String, bool),
    Null(String),
    Map(Vec<(String, Node)>),
    List(Vec<Node>),
}

impl Node {
    fn new(line: usize, value: Value) -> Self {
        Self {
            line,
            key_line: line,
            value: Arc::new(value),
            merge_key: false,
            mapping_error: None,
            depth: 1,
        }
    }

    pub fn text(&self) -> Option<&str> {
        match &*self.value {
            Value::Scalar(s, _) => Some(s),
            _ => None,
        }
    }

    fn key_text(&self) -> Option<&str> {
        match &*self.value {
            Value::Scalar(s, _) | Value::Null(s) => Some(s),
            _ => None,
        }
    }

    pub fn string(&self) -> Option<&str> {
        match &*self.value {
            Value::Scalar(s, true) => Some(s),
            _ => None,
        }
    }

    pub fn mapping(&self) -> bool {
        matches!(&*self.value, Value::Map(_))
    }

    pub fn entries(&self) -> &[(String, Node)] {
        match &*self.value {
            Value::Map(entries) => entries,
            _ => &[],
        }
    }

    pub fn checked_entries(&self, file: &str) -> Result<&[(String, Node)], SourceError> {
        if let Some((line, why)) = self.mapping_error {
            return Err(refused(format!("{file}:{line}"), why));
        }
        Ok(self.entries())
    }

    pub fn get(&self, path: &str) -> Option<&Self> {
        let mut current = self;
        for step in path.split('.') {
            current = &current.entries().iter().rev().find(|(k, _)| k == step)?.1;
        }
        (!matches!(&*current.value, Value::Null(_))).then_some(current)
    }

    pub fn checked_get(&self, file: &str, path: &str) -> Result<Option<&Self>, SourceError> {
        let mut current = self;
        for step in path.split('.') {
            let Some((_, child)) = current
                .checked_entries(file)?
                .iter()
                .rev()
                .find(|(k, _)| k == step)
            else {
                return Ok(None);
            };
            current = child;
        }
        Ok((!matches!(&*current.value, Value::Null(_))).then_some(current))
    }

    pub fn list(&self) -> &[Self] {
        match &*self.value {
            Value::List(list) => list,
            Value::Null(_) => &[],
            _ => std::slice::from_ref(self),
        }
    }

    pub fn checked_find_all<'a>(
        &'a self,
        file: &str,
        name: &str,
        out: &mut Vec<&'a Self>,
    ) -> Result<(), SourceError> {
        let mut pending = vec![(self, false)];
        while let Some((node, found)) = pending.pop() {
            if found {
                out.push(node);
                continue;
            }
            match &*node.value {
                Value::List(list) => pending.extend(list.iter().rev().map(|n| (n, false))),
                Value::Map(_) => pending.extend(
                    node.checked_entries(file)?
                        .iter()
                        .rev()
                        .map(|(key, n)| (n, key == name)),
                ),
                _ => {}
            }
        }
        Ok(())
    }

    pub fn json_duplicates(&mut self) {
        match Arc::make_mut(&mut self.value) {
            Value::Map(entries) => {
                let mut positions = HashMap::new();
                let mut unique: Vec<(String, Node)> = Vec::new();
                for (key, mut node) in entries.drain(..) {
                    node.json_duplicates();
                    if let Some(&index) = positions.get(&key) {
                        unique[index] = (key, node);
                    } else {
                        positions.insert(key.clone(), unique.len());
                        unique.push((key, node));
                    }
                }
                *entries = unique;
            }
            Value::List(list) => {
                for node in list {
                    node.json_duplicates();
                }
            }
            _ => {}
        }
    }
}

// snakeyaml-engine 3.2 JsonScalarResolver: tags affect string selection, never acceptance by Compose.
fn scalar(text: String, style: ScalarStyle, tag: Option<&Tag>) -> Value {
    if let Some(tag) = tag {
        let core = if tag.is_yaml_core_schema() {
            Some(tag.suffix.as_str())
        } else if tag.handle.is_empty() {
            tag.suffix.strip_prefix("tag:yaml.org,2002:")
        } else {
            None
        };
        if core == Some("null") {
            return Value::Null(text);
        }
        let string = core == Some("str") || tag.handle.is_empty() && tag.suffix == "!";
        return Value::Scalar(text, string);
    }
    if style != ScalarStyle::Plain {
        return Value::Scalar(text, true);
    }
    if text.is_empty() || text == "null" {
        return Value::Null(text);
    }
    let string =
        !matches!(text.as_str(), "true" | "false") && !NUMBER.find(&text) && !ENV.find(&text);
    Value::Scalar(text, string)
}

struct Frame {
    node: Node,
    anchor: usize,
    key: Option<Node>,
}

/// The value of a collection still being parsed: an anchor or alias shares it only once it is complete.
fn open_value(value: &mut Arc<Value>) -> &mut Value {
    Arc::get_mut(value).expect("an open collection is not shared yet")
}

pub(crate) fn parse(file: &str, bytes: &[u8], nexo: bool) -> Result<Vec<Node>, SourceError> {
    let text =
        std::str::from_utf8(bytes).map_err(|e| refused(file, format!("not valid UTF-8: {e}")))?;
    parse_text(file, text, nexo, true)
}

pub(crate) fn json(text: &str) -> Option<Node> {
    let mut docs = parse_text("JSON", text, false, false).ok()?;
    if docs.len() != 1 {
        return None;
    }
    let mut node = docs.remove(0);
    node.json_duplicates();
    Some(node)
}

fn parse_text(
    file: &str,
    text: &str,
    nexo: bool,
    yaml_limits: bool,
) -> Result<Vec<Node>, SourceError> {
    let mut docs = Vec::new();
    let mut stack: Vec<Frame> = Vec::new();
    let mut anchors = HashMap::<usize, Node>::new();
    let mut collections = HashSet::new();
    let mut aliases = 0;
    let mut document_start = 0;
    for event in Parser::new_from_str(text) {
        let (event, span) = event.map_err(|e| refused(file, format!("not valid YAML: {e}")))?;
        if matches!(event, Event::DocumentStart(_)) {
            document_start = span.start.index();
            anchors.clear();
            collections.clear();
            aliases = 0;
        }
        if yaml_limits && span.end.index().saturating_sub(document_start) > CODE_POINT_LIMIT {
            return Err(refused(
                file,
                "The incoming YAML document exceeds the limit: 3145728 code points.",
            ));
        }
        let line = span.start.line();
        let (mut node, anchor) = match event {
            Event::SequenceStart(anchor, _) | Event::MappingStart(anchor, _) => {
                if stack.len() >= MAX_DEPTH {
                    return Err(refused(file, "YAML nesting exceeds 4096 levels"));
                }
                let value = if matches!(event, Event::SequenceStart(..)) {
                    Value::List(Vec::new())
                } else {
                    Value::Map(Vec::new())
                };
                if anchor != 0 {
                    collections.insert(anchor);
                }
                stack.push(Frame {
                    node: Node::new(line, value),
                    anchor,
                    key: None,
                });
                continue;
            }
            Event::SequenceEnd | Event::MappingEnd => {
                let mut frame = stack
                    .pop()
                    .ok_or_else(|| refused(file, "unbalanced YAML collection"))?;
                if nexo && let Value::Map(entries) = open_value(&mut frame.node.value) {
                    let mut seen = HashSet::new();
                    entries.reverse();
                    entries.retain(|(key, node)| {
                        !matches!(&*node.value, Value::Null(_)) && seen.insert(key.clone())
                    });
                    entries.reverse();
                }
                (frame.node, frame.anchor)
            }
            Event::Scalar(text, style, anchor, tag) => {
                let mut node = Node::new(line, scalar(text.into_owned(), style, tag.as_deref()));
                node.merge_key = tag
                    .as_ref()
                    .is_some_and(|t| t.is_yaml_core_schema() && t.suffix == "merge");
                (node, anchor)
            }
            Event::Alias(id) => {
                if collections.contains(&id) {
                    aliases += 1;
                    if aliases > MAX_COLLECTION_ALIASES {
                        return Err(refused(
                            file,
                            "Number of aliases for non-scalar nodes exceeds the specified max=50",
                        ));
                    }
                }
                // Shared values preserve alias provenance without expanding the referenced tree.
                (
                    anchors
                        .get(&id)
                        .cloned()
                        .unwrap_or_else(|| Node::new(line, Value::Null(String::new()))),
                    0,
                )
            }
            _ => continue,
        };
        if node.depth + stack.len() > MAX_DEPTH {
            return Err(refused(file, "YAML nesting exceeds 4096 levels"));
        }
        if anchor != 0 {
            anchors.insert(anchor, node.clone());
        }
        if let Some(frame) = stack.last_mut() {
            frame.node.depth = frame.node.depth.max(node.depth + 1);
            match open_value(&mut frame.node.value) {
                Value::List(list) => list.push(node),
                Value::Map(entries) => {
                    if let Some(key) = frame.key.take() {
                        let Some(text) = key.key_text() else {
                            frame
                                .node
                                .mapping_error
                                .get_or_insert((key.line, "only text keys are read"));
                            continue;
                        };
                        if text == "<<" || key.merge_key {
                            frame
                                .node
                                .mapping_error
                                .get_or_insert((key.line, "YAML merge keys (<<) are not read"));
                        }
                        node.key_line = key.line;
                        entries.push((text.to_owned(), node));
                    } else {
                        frame.key = Some(node);
                    }
                }
                _ => unreachable!(),
            }
        } else {
            docs.push(node);
        }
    }
    Ok(docs)
}
