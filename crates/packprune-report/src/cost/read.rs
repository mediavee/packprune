//! Typed reading of a parsed JSON tree, each failure naming its file and field path, and resource ids.

use std::fmt;

use packprune_pack::json::{Node, Value};

/// A malformed input the client would refuse: `file > path: why`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Failure(pub String);

impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

pub(super) type Res<T> = Result<T, Failure>;

/// A resource id as the client reads it: `namespace:path`, the namespace defaulting to `minecraft`.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct Key {
    pub namespace: String,
    pub value: String,
}

impl Key {
    pub(super) fn new(namespace: &str, value: &str) -> Self {
        Self {
            namespace: namespace.to_owned(),
            value: value.to_owned(),
        }
    }

    /// Adventure's `Key.key(String)`; `None` when a character is not allowed.
    pub(super) fn parse(text: &str) -> Option<Self> {
        let (namespace, value) = match text.find(':') {
            Some(colon) if colon > 0 => (&text[..colon], &text[colon + 1..]),
            Some(_) => ("minecraft", &text[1..]),
            None => ("minecraft", text),
        };
        (Self::namespace_ok(namespace) && Self::value_ok(value))
            .then(|| Self::new(namespace, value))
    }

    pub(super) fn namespace_ok(namespace: &str) -> bool {
        namespace
            .bytes()
            .all(|byte| matches!(byte, b'a'..=b'z' | b'0'..=b'9' | b'_' | b'.' | b'-'))
    }

    pub(super) fn value_ok(value: &str) -> bool {
        value
            .bytes()
            .all(|byte| matches!(byte, b'a'..=b'z' | b'0'..=b'9' | b'_' | b'.' | b'-' | b'/'))
    }
}

impl fmt::Display for Key {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.namespace, self.value)
    }
}

/// A JSON value that remembers its file and field path, so a failure names where it is.
#[derive(Clone, Debug)]
pub(super) struct Json<'a> {
    file: &'a str,
    path: String,
    value: Option<&'a Node>,
}

impl<'a> Json<'a> {
    pub(super) fn new(file: &'a str, path: &str, value: &'a Node) -> Self {
        Self {
            file,
            path: path.to_owned(),
            value: Some(value),
        }
    }

    pub(super) fn error(&self, why: &str) -> Failure {
        Failure(format!("{} > {}: {why}", self.file, self.path))
    }

    /// `file > path`, to prefix a message that is not about this value's type.
    pub(super) fn location(&self) -> String {
        format!("{} > {}", self.file, self.path)
    }

    pub(super) fn present(&self) -> bool {
        self.value.is_some()
    }

    fn required(&self) -> Res<&'a Value> {
        self.value
            .map(|node| &node.value)
            .ok_or_else(|| self.error("missing"))
    }

    pub(super) fn is_object(&self) -> bool {
        self.value.is_some_and(Node::is_object)
    }

    pub(super) fn is_array(&self) -> bool {
        self.value.is_some_and(Node::is_array)
    }

    /// The same value, once checked to be an object.
    pub(super) fn object(&self) -> Res<&Self> {
        match self.required()? {
            Value::Object(_) => Ok(self),
            _ => Err(self.error("expected an object")),
        }
    }

    pub(super) fn field(&self, name: &str) -> Res<Self> {
        self.object()?;
        Ok(Self {
            file: self.file,
            path: format!("{}.{name}", self.path),
            value: self.value.and_then(|node| node.get(name)),
        })
    }

    pub(super) fn fields(&self) -> Res<Vec<(&'a str, Self)>> {
        self.object()?;
        Ok(self
            .value
            .into_iter()
            .flat_map(Node::members)
            .map(|member| {
                let field = Self {
                    file: self.file,
                    path: format!("{}.{}", self.path, member.name),
                    value: Some(&member.node),
                };
                (&*member.name, field)
            })
            .collect())
    }

    pub(super) fn array(&self) -> Res<Vec<Self>> {
        match self.required()? {
            Value::Array(items) => Ok(items
                .iter()
                .enumerate()
                .map(|(index, node)| Self {
                    file: self.file,
                    path: format!("{}[{index}]", self.path),
                    value: Some(node),
                })
                .collect()),
            _ => Err(self.error("expected an array")),
        }
    }

    pub(super) fn string(&self) -> Res<&'a str> {
        match self.required()? {
            Value::String(text) => Ok(text),
            _ => Err(self.error("expected a string")),
        }
    }

    pub(super) fn bool(&self) -> Res<bool> {
        match self.required()? {
            Value::Bool(flag) => Ok(*flag),
            _ => Err(self.error("expected a boolean")),
        }
    }

    /// A number as a double, as Jackson's `doubleValue()` reads any of its number nodes.
    fn number(&self) -> Res<f64> {
        self.required()?;
        self.value
            .and_then(Node::double_value)
            .ok_or_else(|| self.error("expected a number"))
    }

    /// A number with no fractional part that fits an `int`, as `BigDecimal.intValueExact`.
    pub(super) fn integer(&self) -> Res<i32> {
        Some(self.number()?)
            .filter(|real| real.is_finite() && real.fract() == 0.0)
            .filter(|real| (f64::from(i32::MIN)..=f64::from(i32::MAX)).contains(real))
            .map(|real| real as i32)
            .ok_or_else(|| self.error("expected a 32-bit integer"))
    }

    pub(super) fn decimal(&self) -> Res<f64> {
        Some(self.number()?)
            .filter(|real| real.is_finite())
            .ok_or_else(|| self.error("expected a finite number"))
    }

    pub(super) fn key(&self) -> Res<Key> {
        let text = self.string()?;
        Key::parse(text).ok_or_else(|| self.error(&format!("invalid resource id: {text}")))
    }

    /// `Codec.BOOL`: a JSON boolean or a number.
    pub(super) fn flag(&self) -> Res<()> {
        match self.value.map(|node| &node.value) {
            None | Some(Value::Bool(_)) => Ok(()),
            Some(_) if self.number().is_ok() => Ok(()),
            Some(_) => Err(self.error("expected a boolean")),
        }
    }
}
