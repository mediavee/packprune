//! Java regexes as the client's `IdentifierPattern` applies them: `Pattern.compile(text)` found
//! anywhere in the value. Atlas `filter` sources and `pack.mcmeta`'s `filter.block` use them.
//!
//! The pattern is parsed with Java's grammar for the subset packs write (literals and escapes, classes with ranges,
//! `.`, groups, alternation, greedy and lazy quantifiers, `^`, `$`, `\b`, `\Q...\E`) and translated to the `regex`
//! crate's syntax with Java's meaning: `.` and `$` know Java's line terminators, `\d`, `\w`, `\s` and `\b` are ASCII
//! as in Java 25. Anything else (lookaround, backreferences, flags, possessive quantifiers, class intersections,
//! `\p{...}`) is refused with the pattern's text.

#[cfg(test)]
mod tests;

/// Java's line terminators without `UNIX_LINES`.
const TERMINATORS: &str = r"\n\r\x{85}\x{2028}\x{2029}";

/// Counted repetition past this is refused rather than compiled huge.
const MAX_COUNT: u32 = 1000;

#[derive(Clone, Debug)]
pub struct JavaPattern {
    text: Box<str>,
    regex: regex::Regex,
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("pattern {pattern:?}: {why}")]
pub struct PatternError {
    pub pattern: String,
    pub why: String,
}

impl JavaPattern {
    pub fn new(text: &str) -> Result<Self, PatternError> {
        let refuse = |why: &str| PatternError {
            pattern: text.to_owned(),
            why: why.to_owned(),
        };
        let chars: Vec<char> = text.chars().collect();
        let mut parser = Parser { c: &chars, at: 0 };
        let translated = parser.alternation().map_err(refuse)?;
        if parser.at != chars.len() {
            return Err(refuse("unbalanced `)`"));
        }
        let regex = regex::Regex::new(&translated).map_err(|error| refuse(&error.to_string()))?;
        Ok(Self {
            text: text.into(),
            regex,
        })
    }

    /// `Matcher.find`: the pattern matches somewhere in `value`.
    pub fn find(&self, value: &str) -> bool {
        self.regex.is_match(value)
    }

    pub fn as_str(&self) -> &str {
        &self.text
    }
}

type Parsed = Result<String, &'static str>;

struct Parser<'a> {
    c: &'a [char],
    at: usize,
}

impl Parser<'_> {
    fn peek(&self) -> Option<char> {
        self.c.get(self.at).copied()
    }

    fn eat(&mut self, c: char) -> bool {
        let found = self.peek() == Some(c);
        if found {
            self.at += 1;
        }
        found
    }

    fn alternation(&mut self) -> Parsed {
        let mut out = self.concat()?;
        while self.eat('|') {
            out.push('|');
            out.push_str(&self.concat()?);
        }
        Ok(out)
    }

    fn concat(&mut self) -> Parsed {
        let mut out = String::new();
        while let Some(c) = self.peek() {
            if c == '|' || c == ')' {
                break;
            }
            if c == '\\' && self.c.get(self.at + 1) == Some(&'Q') {
                self.at += 2;
                while self.at < self.c.len() {
                    if self.c[self.at] == '\\' && self.c.get(self.at + 1) == Some(&'E') {
                        self.at += 2;
                        break;
                    }
                    out.push_str(&literal(self.c[self.at]));
                    self.at += 1;
                }
                continue;
            }
            let (atom, quantifiable) = self.atom()?;
            out.push_str(&self.quantified(atom, quantifiable)?);
        }
        Ok(out)
    }

    fn quantified(&mut self, atom: String, quantifiable: bool) -> Parsed {
        let (min, max) = match self.peek() {
            Some('*') => (0, None),
            Some('+') => (1, None),
            Some('?') => (0, Some(1)),
            Some('{') => {
                self.at += 1;
                let min = self.number()?;
                let max = if self.eat(',') {
                    if self.peek() == Some('}') {
                        None
                    } else {
                        Some(self.number()?)
                    }
                } else {
                    Some(min)
                };
                if self.peek() != Some('}') || max.is_some_and(|max| max < min) {
                    return Err("invalid repetition");
                }
                (min, max)
            }
            _ => return Ok(atom),
        };
        self.at += 1;
        if !quantifiable {
            return Err("a quantifier with nothing to repeat");
        }
        if min > MAX_COUNT || max.is_some_and(|max| max > MAX_COUNT) {
            return Err("repetition count too large");
        }
        let lazy = self.eat('?');
        if matches!(self.peek(), Some('+' | '*' | '?' | '{')) {
            return Err("possessive or stacked quantifiers are not supported");
        }
        let count = match (min, max) {
            (0, None) => "*".to_owned(),
            (1, None) => "+".to_owned(),
            (0, Some(1)) => "?".to_owned(),
            (min, None) => format!("{{{min},}}"),
            (min, Some(max)) if min == max => format!("{{{min}}}"),
            (min, Some(max)) => format!("{{{min},{max}}}"),
        };
        Ok(format!("(?:{atom}){count}{}", if lazy { "?" } else { "" }))
    }

    fn number(&mut self) -> Result<u32, &'static str> {
        let start = self.at;
        while self.peek().is_some_and(|c| c.is_ascii_digit()) {
            self.at += 1;
        }
        self.c[start..self.at]
            .iter()
            .collect::<String>()
            .parse()
            .map_err(|_| "invalid repetition")
    }

    /// One atom in `regex` syntax, and whether a quantifier may follow it.
    fn atom(&mut self) -> Result<(String, bool), &'static str> {
        let c = self.peek().ok_or("unexpected end")?;
        self.at += 1;
        Ok(match c {
            '(' => {
                if self.eat('?') && !self.eat(':') {
                    return Err("group flags and lookaround are not supported");
                }
                let inner = self.alternation()?;
                if !self.eat(')') {
                    return Err("unclosed group");
                }
                (format!("(?:{inner})"), true)
            }
            '[' => (self.class()?, true),
            '.' => (format!("[^{TERMINATORS}]"), true),
            '^' => (r"\A".to_owned(), false),
            '$' => {
                // A `$` followed by more pattern needs lookahead to keep the terminator unconsumed.
                if !matches!(self.peek(), None | Some('|' | ')')) {
                    return Err("`$` before more pattern is not supported");
                }
                (format!(r"(?:\r\n|[{TERMINATORS}])?\z"), false)
            }
            '\\' => self.escape(false)?,
            '*' | '+' | '?' | '{' => return Err("a quantifier with nothing to repeat"),
            ')' => return Err("unbalanced `)`"),
            c => (literal(c), true),
        })
    }

    /// After a backslash. Inside a class only what stands for characters is allowed.
    fn escape(&mut self, in_class: bool) -> Result<(String, bool), &'static str> {
        let c = self.peek().ok_or("trailing backslash")?;
        self.at += 1;
        let set = |set: &str| Ok((set.to_owned(), true));
        match c {
            'd' => set("[0-9]"),
            'D' => set("[^0-9]"),
            'w' => set("[a-zA-Z_0-9]"),
            'W' => set("[^a-zA-Z_0-9]"),
            's' => set(r"[ \t\n\x0B\f\r]"),
            'S' => set(r"[^ \t\n\x0B\f\r]"),
            'h' => set(r"[ \t\x{a0}\x{1680}\x{180e}\x{2000}-\x{200a}\x{202f}\x{205f}\x{3000}]"),
            'H' => set(r"[^ \t\x{a0}\x{1680}\x{180e}\x{2000}-\x{200a}\x{202f}\x{205f}\x{3000}]"),
            'v' => set(r"[\n\x0B\f\r\x{85}\x{2028}\x{2029}]"),
            'V' => set(r"[^\n\x0B\f\r\x{85}\x{2028}\x{2029}]"),
            'b' if !in_class => Ok((r"(?-u:\b)".to_owned(), false)),
            'B' if !in_class => Ok((r"(?-u:\B)".to_owned(), false)),
            'A' if !in_class => Ok((r"\A".to_owned(), false)),
            'z' if !in_class => Ok((r"\z".to_owned(), false)),
            't' => Ok((literal('\t'), true)),
            'n' => Ok((literal('\n'), true)),
            'r' => Ok((literal('\r'), true)),
            'f' => Ok((literal('\u{c}'), true)),
            'a' => Ok((literal('\u{7}'), true)),
            'e' => Ok((literal('\u{1b}'), true)),
            'x' | 'u' => {
                let digits = if c == 'x' { 2 } else { 4 };
                let hex: String = self
                    .c
                    .get(self.at..self.at + digits)
                    .ok_or("truncated escape")?
                    .iter()
                    .collect();
                self.at += digits;
                let code = u32::from_str_radix(&hex, 16).map_err(|_| "invalid escape")?;
                Ok((literal(char::from_u32(code).ok_or("invalid escape")?), true))
            }
            c if c.is_ascii_alphanumeric() => Err("escape not supported"),
            c => Ok((literal(c), true)),
        }
    }

    fn class(&mut self) -> Parsed {
        let mut out = String::from("[");
        if self.eat('^') {
            out.push('^');
        }
        let mut first = true;
        loop {
            let c = self.peek().ok_or("unclosed character class")?;
            if c == ']' && !first {
                self.at += 1;
                break;
            }
            first = false;
            if c == '[' || c == '&' && self.c.get(self.at + 1) == Some(&'&') {
                return Err("nested classes and intersections are not supported");
            }
            self.at += 1;
            let low = if c == '\\' {
                let (escaped, _) = self.escape(true)?;
                match single(&escaped) {
                    Some(low) => low,
                    None => {
                        // A predefined class: `regex` nests it as a set inside the class.
                        out.push_str(&escaped);
                        continue;
                    }
                }
            } else {
                c
            };
            if self.peek() == Some('-') && self.c.get(self.at + 1).is_some_and(|&next| next != ']')
            {
                self.at += 1;
                let high = match self.peek().ok_or("unclosed character class")? {
                    '\\' => {
                        self.at += 1;
                        single(&self.escape(true)?.0).ok_or("invalid class range")?
                    }
                    '[' => return Err("nested classes and intersections are not supported"),
                    high => {
                        self.at += 1;
                        high
                    }
                };
                if high < low {
                    return Err("invalid class range");
                }
                out.push_str(&format!("{}-{}", hex(low), hex(high)));
            } else {
                out.push_str(&hex(low));
            }
        }
        out.push(']');
        Ok(out)
    }
}

fn hex(c: char) -> String {
    format!(r"\x{{{:x}}}", u32::from(c))
}

fn literal(c: char) -> String {
    hex(c)
}

/// The character a translated literal stands for.
fn single(translated: &str) -> Option<char> {
    let code = translated.strip_prefix(r"\x{")?.strip_suffix('}')?;
    char::from_u32(u32::from_str_radix(code, 16).ok()?)
}
