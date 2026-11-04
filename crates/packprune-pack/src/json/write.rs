//! The one JSON writer: Jackson 3.2's compact serialisation (`writeValueAsBytes`). Escapes are Jackson's: quote,
//! backslash, `\b\t\n\f\r`, `\u00XX` in upper case for the other controls, nothing else (`/` and non-ASCII are
//! written as they are); doubles are `Double.toString` and floats `Float.toString`, a non-finite one quoted.

use std::io::Write as _;

use super::{Node, Value, java_double, java_float};

/// Jackson's `JsonGenerator` in compact mode: each value or member name written in order, separators added.
pub struct Writer<'a> {
    out: &'a mut Vec<u8>,
    /// Per open container: whether a value was written in it.
    filled: Vec<bool>,
    /// A member name was written and its value is next.
    named: bool,
}

impl<'a> Writer<'a> {
    pub fn new(out: &'a mut Vec<u8>) -> Self {
        Self {
            out,
            filled: Vec::new(),
            named: false,
        }
    }

    fn separate(&mut self) {
        if std::mem::take(&mut self.named) {
            return;
        }
        if let Some(filled) = self.filled.last_mut() {
            if *filled {
                self.out.push(b',');
            }
            *filled = true;
        }
    }

    pub fn null(&mut self) {
        self.separate();
        self.out.extend_from_slice(b"null");
    }

    pub fn bool(&mut self, value: bool) {
        self.separate();
        self.out
            .extend_from_slice(if value { b"true" } else { b"false" });
    }

    pub fn int(&mut self, value: i64) {
        self.separate();
        write!(self.out, "{value}").expect("writes to a Vec");
    }

    /// A number whose text is already Jackson's, such as a `BigIntegerNode`'s.
    pub fn number(&mut self, text: &str) {
        self.separate();
        self.out.extend_from_slice(text.as_bytes());
    }

    /// A non-finite value is written as a string (`"NaN"`, `"Infinity"`), as Jackson's default `WRITE_NAN_AS_STRINGS`
    /// does: JSON has no such number.
    pub fn float(&mut self, value: f32) {
        if value.is_finite() {
            self.number(&java_float(value));
        } else {
            self.string(&java_float(value));
        }
    }

    /// As [`Self::float`].
    pub fn double(&mut self, value: f64) {
        if value.is_finite() {
            self.number(&java_double(value));
        } else {
            self.string(&java_double(value));
        }
    }

    pub fn string(&mut self, text: &str) {
        self.separate();
        quoted(text, self.out);
    }

    /// A member name of the open object; its value is written next.
    pub fn name(&mut self, name: &str) {
        self.separate();
        quoted(name, self.out);
        self.out.push(b':');
        self.named = true;
    }

    pub fn start_array(&mut self) {
        self.open(b'[');
    }

    pub fn end_array(&mut self) {
        self.close(b']');
    }

    pub fn start_object(&mut self) {
        self.open(b'{');
    }

    pub fn end_object(&mut self) {
        self.close(b'}');
    }

    fn open(&mut self, bracket: u8) {
        self.separate();
        self.out.push(bracket);
        self.filled.push(false);
    }

    fn close(&mut self, bracket: u8) {
        self.filled.pop();
        self.out.push(bracket);
    }

    /// `node` and what it holds. The missing node is written `null`, as Jackson writes an absent value.
    pub fn node(&mut self, node: &Node) {
        match &node.value {
            Value::Object(members) => {
                self.start_object();
                for member in members {
                    self.name(&member.name);
                    self.node(&member.node);
                }
                self.end_object();
            }
            Value::Array(items) => {
                self.start_array();
                for item in items {
                    self.node(item);
                }
                self.end_array();
            }
            Value::String(text) => self.string(text),
            Value::Int(value) => self.int(i64::from(*value)),
            Value::Long(value) => self.int(*value),
            Value::Big(text) => self.number(text),
            Value::Double(value) => self.double(*value),
            Value::Bool(value) => self.bool(*value),
            Value::Null | Value::Missing => self.null(),
        }
    }
}

/// `node` as Jackson's `writeValueAsBytes` writes it.
pub fn write(node: &Node) -> Vec<u8> {
    let mut out = Vec::new();
    Writer::new(&mut out).node(node);
    out
}

fn quoted(text: &str, out: &mut Vec<u8>) {
    out.push(b'"');
    let mut start = 0;
    for (at, c) in text.char_indices() {
        let escape: Option<&[u8]> = match c {
            '"' => Some(b"\\\""),
            '\\' => Some(b"\\\\"),
            '\n' => Some(b"\\n"),
            '\r' => Some(b"\\r"),
            '\t' => Some(b"\\t"),
            '\u{8}' => Some(b"\\b"),
            '\u{c}' => Some(b"\\f"),
            _ => None,
        };
        if escape.is_some() || c < ' ' {
            out.extend_from_slice(&text.as_bytes()[start..at]);
            match escape {
                Some(escape) => out.extend_from_slice(escape),
                None => write!(out, "\\u{:04X}", c as u32).expect("writes to a Vec"),
            }
            start = at + c.len_utf8();
        }
    }
    out.extend_from_slice(&text.as_bytes()[start..]);
    out.push(b'"');
}
