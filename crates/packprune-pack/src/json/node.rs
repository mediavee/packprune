//! The one token-to-tree builder: Jackson's `JsonNode` (`readTree`) with the accessors the graph, the oracle and the
//! cost calculator call, and each node's byte span. One root value, members in first-seen order, a repeated name
//! replacing the value in place (`ObjectNode.set`, Gson's `JsonObject.add`).

use std::borrow::Cow;
use std::collections::HashMap;
use std::ops::Range;

use super::{JsonError, Path, Token, TokenKind, java_double, lenient, scan};

#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Object(Vec<Member>),
    Array(Vec<Node>),
    String(Box<str>),
    /// `IntNode`.
    Int(i32),
    /// `LongNode`.
    Long(i64),
    /// `BigIntegerNode`, kept as its text.
    Big(Box<str>),
    /// `DoubleNode`.
    Double(f64),
    Bool(bool),
    Null,
    /// Jackson's `MissingNode`: an absent member, or an empty document.
    Missing,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Member {
    pub name: Box<str>,
    pub name_span: Range<u32>,
    pub node: Node,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Node {
    pub value: Value,
    /// Empty for a node that was not parsed.
    pub span: Range<u32>,
}

static MISSING: Node = Node {
    value: Value::Missing,
    span: 0..0,
};

/// A child's place in its container: a member name or an array index.
#[derive(Clone, Copy)]
pub enum Seg<'a> {
    Name(&'a str),
    Index(usize),
}

/// Jackson `readTree` with the strict grammar: at most one root value, an empty file is the missing node.
pub fn parse(json: &[u8]) -> Result<Node, JsonError> {
    build(|visitor| scan::visit(json, visitor))
}

/// [`parse`] with the grammar of [`super::visit_lenient`], the one Gson's `JsonParser` reads a font with. An empty
/// file is the missing node, as Gson's `null`.
pub fn parse_lenient(json: &[u8]) -> Result<Node, JsonError> {
    build(|visitor| lenient::visit(json, visitor))
}

fn build<'a>(
    walk: impl FnOnce(&mut dyn FnMut(&Token<'a>, &Path)) -> Result<(), JsonError>,
) -> Result<Node, JsonError> {
    let span = |range: &Range<usize>| range.start as u32..range.end as u32;
    // Each open container with the member name it is the value of, and an object's members by name once it has
    // enough of them that looking a repeated name up member by member would cost more.
    type Open = (
        Option<(Box<str>, Range<u32>)>,
        Node,
        HashMap<Box<str>, usize>,
    );
    const INDEXED: usize = 16;
    let mut stack: Vec<Open> = Vec::new();
    let mut name: Option<(Box<str>, Range<u32>)> = None;
    let mut root: Option<Node> = None;
    let mut extra = None;
    walk(&mut |token, _| {
        let node = match token.kind {
            TokenKind::Name => {
                name = Some((token.text.as_ref().into(), span(&token.span)));
                return;
            }
            TokenKind::StartObject | TokenKind::StartArray => {
                let value = if token.kind == TokenKind::StartObject {
                    Value::Object(Vec::new())
                } else {
                    Value::Array(Vec::new())
                };
                stack.push((
                    name.take(),
                    Node {
                        value,
                        span: span(&token.span),
                    },
                    HashMap::new(),
                ));
                return;
            }
            TokenKind::EndObject | TokenKind::EndArray => {
                let (own, mut node, _) = stack.pop().expect("balanced by the grammar");
                node.span.end = token.span.end as u32;
                name = own;
                node
            }
            kind => Node {
                value: match kind {
                    TokenKind::String => Value::String(token.text.as_ref().into()),
                    TokenKind::Int => integer(&token.text),
                    TokenKind::Float => Value::Double(token.text.parse().unwrap_or(f64::NAN)),
                    TokenKind::True => Value::Bool(true),
                    TokenKind::False => Value::Bool(false),
                    _ => Value::Null,
                },
                span: span(&token.span),
            },
        };
        match stack.last_mut() {
            Some((
                _,
                Node {
                    value: Value::Object(members),
                    ..
                },
                names,
            )) => {
                let (key, name_span) = name.take().expect("a member has a name");
                if names.is_empty() && members.len() >= INDEXED {
                    names.extend(
                        members
                            .iter()
                            .enumerate()
                            .map(|(at, member)| (member.name.clone(), at)),
                    );
                }
                let found = if names.is_empty() {
                    members.iter().position(|member| member.name == key)
                } else {
                    names.get(&key).copied()
                };
                match found {
                    Some(at) => members[at].node = node,
                    None => {
                        if !names.is_empty() {
                            names.insert(key.clone(), members.len());
                        }
                        members.push(Member {
                            name: key,
                            name_span,
                            node,
                        });
                    }
                }
            }
            Some((
                _,
                Node {
                    value: Value::Array(items),
                    ..
                },
                _,
            )) => items.push(node),
            _ if root.is_none() => root = Some(node),
            _ => extra = extra.or(Some(token.span.start)),
        }
    })?;
    if let Some(offset) = extra {
        return Err(JsonError::new(
            offset,
            "trailing token after the root value",
        ));
    }
    Ok(root.unwrap_or(MISSING.clone()))
}

fn integer(text: &str) -> Value {
    if let Ok(value) = text.parse::<i32>() {
        Value::Int(value)
    } else if let Ok(value) = text.parse::<i64>() {
        Value::Long(value)
    } else {
        Value::Big(text.into())
    }
}

/// `Integer.parseInt` over what `NumberInput.looksLikeValidNumber` lets through.
fn java_int(text: &str) -> Option<i32> {
    let digits = text.strip_prefix(['+', '-']).unwrap_or(text);
    if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    text.strip_prefix('+').unwrap_or(text).parse().ok()
}

/// `NumberInput.parseDouble` over what `looksLikeValidNumber` lets through: a sign, digits, a fraction, an exponent.
fn java_double_of(text: &str) -> Option<f64> {
    let body = text.strip_prefix(['+', '-']).unwrap_or(text);
    let (mantissa, exponent) = match body.find(['e', 'E']) {
        Some(at) => (&body[..at], Some(&body[at + 1..])),
        None => (body, None),
    };
    let digits = |part: &str| part.bytes().all(|byte| byte.is_ascii_digit());
    let (int, fraction) = mantissa.split_once('.').unwrap_or((mantissa, ""));
    let valid = (!int.is_empty() || !fraction.is_empty())
        && digits(int)
        && digits(fraction)
        && exponent.is_none_or(|exponent| {
            let exponent = exponent.strip_prefix(['+', '-']).unwrap_or(exponent);
            !exponent.is_empty() && digits(exponent)
        });
    if !valid {
        return None;
    }
    text.strip_prefix('+').unwrap_or(text).parse().ok()
}

impl Member {
    /// A member that was not parsed, with empty spans.
    pub fn new(name: &str, node: Node) -> Self {
        Self {
            name: name.into(),
            name_span: 0..0,
            node,
        }
    }
}

impl Node {
    /// A node that was not parsed, with an empty span.
    pub fn new(value: Value) -> Self {
        Self { value, span: 0..0 }
    }

    pub fn get(&self, name: &str) -> Option<&Node> {
        match &self.value {
            Value::Object(members) => members
                .iter()
                .find(|member| &*member.name == name)
                .map(|member| &member.node),
            _ => None,
        }
    }

    /// `path(name)`: the member, or the missing node.
    pub fn path(&self, name: &str) -> &Node {
        self.get(name).unwrap_or(&MISSING)
    }

    pub fn has(&self, name: &str) -> bool {
        self.get(name).is_some()
    }

    /// The member when it is a string.
    pub fn string(&self, name: &str) -> Option<&Node> {
        self.get(name).filter(|node| node.as_str().is_some())
    }

    /// `get(index)` of an array.
    pub fn item(&self, index: usize) -> Option<&Node> {
        self.items().get(index)
    }

    /// `stringValue()` of a `StringNode`, `None` for anything else.
    pub fn as_str(&self) -> Option<&str> {
        match &self.value {
            Value::String(text) => Some(text),
            _ => None,
        }
    }

    pub fn is_object(&self) -> bool {
        matches!(self.value, Value::Object(_))
    }

    pub fn is_array(&self) -> bool {
        matches!(self.value, Value::Array(_))
    }

    pub fn size(&self) -> usize {
        match &self.value {
            Value::Array(items) => items.len(),
            Value::Object(members) => members.len(),
            _ => 0,
        }
    }

    /// An array's items, nothing for anything else.
    pub fn items(&self) -> &[Node] {
        match &self.value {
            Value::Array(items) => items,
            _ => &[],
        }
    }

    /// `properties()`: an object's members in order, nothing for anything else.
    pub fn members(&self) -> &[Member] {
        match &self.value {
            Value::Object(members) => members,
            _ => &[],
        }
    }

    /// What `for (JsonNode child : node)` walks: array items or object values.
    pub fn children(&self) -> Box<dyn Iterator<Item = &Node> + '_> {
        match &self.value {
            Value::Array(items) => Box::new(items.iter()),
            Value::Object(members) => Box::new(members.iter().map(|member| &member.node)),
            _ => Box::new(std::iter::empty()),
        }
    }

    /// [`Self::children`] with each child's place.
    pub fn children_at(&self) -> Vec<(Seg<'_>, &Node)> {
        match &self.value {
            Value::Array(items) => items
                .iter()
                .enumerate()
                .map(|(index, node)| (Seg::Index(index), node))
                .collect(),
            Value::Object(members) => members
                .iter()
                .map(|member| (Seg::Name(&member.name), &member.node))
                .collect(),
            _ => Vec::new(),
        }
    }

    /// `asInt(default)`.
    pub fn as_int(&self, default: i32) -> i32 {
        match &self.value {
            Value::Int(value) => *value,
            Value::Long(value) => i32::try_from(*value).unwrap_or(default),
            Value::Double(value) if (f64::from(i32::MIN)..=f64::from(i32::MAX)).contains(value) => {
                *value as i32
            }
            Value::String(text) => java_int(text).unwrap_or(default),
            _ => default,
        }
    }

    /// `isInt()`: an integer that fits `int`.
    pub fn int(&self) -> Option<i32> {
        match self.value {
            Value::Int(value) => Some(value),
            _ => None,
        }
    }

    /// `asInt()` of Jackson 3, which has no default: integers in range, doubles truncated when in range, strings
    /// `[+-]?[0-9]+` in range, 0 for `null` or missing; anything else throws, here `None`.
    pub fn try_as_int(&self) -> Option<i32> {
        match &self.value {
            Value::Int(value) => Some(*value),
            Value::Double(value) if (f64::from(i32::MIN)..=f64::from(i32::MAX)).contains(value) => {
                Some(*value as i32)
            }
            Value::String(text) => text.parse().ok(),
            Value::Null | Value::Missing => Some(0),
            _ => None,
        }
    }

    /// `asBoolean(default)`.
    pub fn as_bool(&self, default: bool) -> bool {
        match &self.value {
            Value::Bool(value) => *value,
            Value::Int(value) => *value != 0,
            Value::Long(value) => *value != 0,
            Value::Big(text) => text.bytes().any(|byte| (b'1'..=b'9').contains(&byte)),
            Value::String(text) if &**text == "true" => true,
            Value::String(text) if &**text == "false" => false,
            _ => default,
        }
    }

    /// `asString(default)`: a scalar's text, `default` for null, missing and containers.
    pub fn as_string<'a>(&'a self, default: &'a str) -> Cow<'a, str> {
        match &self.value {
            Value::String(text) => (&**text).into(),
            Value::Int(value) => value.to_string().into(),
            Value::Long(value) => value.to_string().into(),
            Value::Big(text) => (&**text).into(),
            Value::Double(value) => java_double(*value).into(),
            Value::Bool(value) => value.to_string().into(),
            _ => default.into(),
        }
    }

    /// `asDouble(default)`.
    pub fn as_double(&self, default: f64) -> f64 {
        match &self.value {
            Value::String(text) => java_double_of(text).unwrap_or(default),
            _ => self.double_value().unwrap_or(default),
        }
    }

    /// `doubleValue()` of a number.
    pub fn double_value(&self) -> Option<f64> {
        match &self.value {
            Value::Int(value) => Some(f64::from(*value)),
            Value::Long(value) => Some(*value as f64),
            Value::Big(text) => text.parse().ok(),
            Value::Double(value) => Some(*value),
            _ => None,
        }
    }

    /// `floatValue()`, 0 for anything but a finite number (Jackson 2's value; Jackson 3 throws).
    pub fn float_value(&self) -> f32 {
        self.double_value()
            .map(|value| value as f32)
            .filter(|value| value.is_finite())
            .unwrap_or(0.0)
    }
}
