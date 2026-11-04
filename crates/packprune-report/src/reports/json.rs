//! The report files' JSON layout, Jackson's `DefaultPrettyPrinter` style: two-space indents, `"name": value`, an
//! empty array written `[ ]` and an empty object `{ }`.

use std::io;

use serde::Serialize;
use serde_json::ser::{CharEscape, Formatter, Serializer};

#[derive(Default)]
struct Layout {
    depth: usize,
    has_value: bool,
}

impl Layout {
    fn indent<W: ?Sized + io::Write>(&self, writer: &mut W) -> io::Result<()> {
        for _ in 0..self.depth {
            writer.write_all(b"  ")?;
        }
        Ok(())
    }

    fn begin<W: ?Sized + io::Write>(&mut self, writer: &mut W, open: &[u8]) -> io::Result<()> {
        self.depth += 1;
        self.has_value = false;
        writer.write_all(open)
    }

    fn end<W: ?Sized + io::Write>(&mut self, writer: &mut W, close: &[u8]) -> io::Result<()> {
        self.depth -= 1;
        if self.has_value {
            writer.write_all(b"\n")?;
            self.indent(writer)?;
        } else {
            writer.write_all(b" ")?;
        }
        writer.write_all(close)
    }

    fn next<W: ?Sized + io::Write>(&mut self, writer: &mut W, first: bool) -> io::Result<()> {
        writer.write_all(if first { b"\n" } else { b",\n" })?;
        self.indent(writer)
    }
}

#[derive(Default)]
struct Jackson(Layout);

impl Formatter for Jackson {
    fn begin_array<W: ?Sized + io::Write>(&mut self, writer: &mut W) -> io::Result<()> {
        self.0.begin(writer, b"[")
    }

    fn end_array<W: ?Sized + io::Write>(&mut self, writer: &mut W) -> io::Result<()> {
        self.0.end(writer, b"]")
    }

    fn begin_array_value<W: ?Sized + io::Write>(
        &mut self,
        writer: &mut W,
        first: bool,
    ) -> io::Result<()> {
        self.0.next(writer, first)
    }

    fn end_array_value<W: ?Sized + io::Write>(&mut self, _: &mut W) -> io::Result<()> {
        self.0.has_value = true;
        Ok(())
    }

    fn begin_object<W: ?Sized + io::Write>(&mut self, writer: &mut W) -> io::Result<()> {
        self.0.begin(writer, b"{")
    }

    fn end_object<W: ?Sized + io::Write>(&mut self, writer: &mut W) -> io::Result<()> {
        self.0.end(writer, b"}")
    }

    fn begin_object_key<W: ?Sized + io::Write>(
        &mut self,
        writer: &mut W,
        first: bool,
    ) -> io::Result<()> {
        self.0.next(writer, first)
    }

    fn begin_object_value<W: ?Sized + io::Write>(&mut self, writer: &mut W) -> io::Result<()> {
        writer.write_all(b": ")
    }

    fn end_object_value<W: ?Sized + io::Write>(&mut self, _: &mut W) -> io::Result<()> {
        self.0.has_value = true;
        Ok(())
    }

    /// Jackson writes control characters as `\u001F`, upper-case hex.
    fn write_char_escape<W: ?Sized + io::Write>(
        &mut self,
        writer: &mut W,
        escape: CharEscape,
    ) -> io::Result<()> {
        match escape {
            CharEscape::AsciiControl(byte) => write!(writer, "\\u{byte:04X}"),
            other => {
                let text: &[u8] = match other {
                    CharEscape::Quote => b"\\\"",
                    CharEscape::ReverseSolidus => b"\\\\",
                    CharEscape::Solidus => b"\\/",
                    CharEscape::Backspace => b"\\b",
                    CharEscape::FormFeed => b"\\f",
                    CharEscape::LineFeed => b"\\n",
                    CharEscape::CarriageReturn => b"\\r",
                    CharEscape::Tab => b"\\t",
                    CharEscape::AsciiControl(_) => unreachable!("handled above"),
                };
                writer.write_all(text)
            }
        }
    }
}

/// The value as a report file: pretty, then a line feed.
pub fn to_string<T: Serialize>(value: &T) -> String {
    let mut bytes = Vec::new();
    let mut serializer = Serializer::with_formatter(&mut bytes, Jackson::default());
    value
        .serialize(&mut serializer)
        .expect("a report is plain data and a Vec takes every write");
    bytes.push(b'\n');
    String::from_utf8(bytes).expect("serde_json writes UTF-8")
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn matches_jacksons_default_pretty_printer() {
        let value = json!({"a": [], "b": {}, "c": [1, 2], "d": {"e": "x\u{1f}\"y"}});
        // `json!` keeps keys sorted; Jackson's layout is what is under test.
        assert_eq!(
            to_string(&value),
            "{\n  \"a\": [ ],\n  \"b\": { },\n  \"c\": [\n    1,\n    2\n  ],\n  \"d\": {\n    \"e\": \"x\\u001F\\\"y\"\n  }\n}\n"
        );
    }
}
