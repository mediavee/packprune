//! The `#moj_import` directives the 26.2 client reads, as `GlslPreprocessor` finds them: `REGEX_MOJ_IMPORT` and
//! `isDirectiveDisabled` ported by hand, over the file decoded as UTF-8 (malformed bytes replaced), with Java's
//! positions (UTF-16 units) where the client compares them.

use std::borrow::Cow;
use std::ops::Range;

use packprune_pack::json::Pointer;

use crate::{RawRef, Role};

/// Every import the client does not disable, in text order: the path between the quotes or the angle brackets.
pub(crate) fn imports(bytes: &[u8]) -> Vec<RawRef> {
    let text = String::from_utf8_lossy(bytes);
    let lossless = matches!(text, Cow::Borrowed(_));
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    let c: Vec<char> = chars.iter().map(|&(_, c)| c).collect();
    // UTF-16 offset of each char index, one past the end included.
    let mut utf16 = Vec::with_capacity(c.len() + 1);
    let mut unit = 0;
    for ch in &c {
        utf16.push(unit);
        unit += ch.len_utf16();
    }
    utf16.push(unit);
    let byte_at = |index: usize| chars.get(index).map_or(text.len(), |&(at, _)| at);

    let mut out = Vec::new();
    let mut previous_end = 0;
    let mut from = 0;
    while let Some(found) = find_import(&c, from) {
        from = found.end;
        if disabled(&c, &utf16, found.start, previous_end) {
            continue;
        }
        previous_end = found.end;
        // A span into lossily decoded text would not name the file's bytes: such a file keeps no span.
        let span = if lossless {
            byte_at(found.group.start) as u32..byte_at(found.group.end) as u32
        } else {
            0..0
        };
        out.push(RawRef {
            pointer: Pointer::new(""),
            span,
            raw: c[found.group.clone()].iter().collect::<String>().into(),
            role: if found.quoted {
                Role::ShaderImportQuoted
            } else {
                Role::ShaderImportAngled
            },
        });
    }
    out
}

struct Found {
    start: usize,
    end: usize,
    group: Range<usize>,
    quoted: bool,
}

/// `\h`: Java's horizontal whitespace.
fn horizontal(c: char) -> bool {
    matches!(
        c,
        ' ' | '\t' | '\u{a0}' | '\u{1680}' | '\u{180e}' | '\u{2000}'
            ..='\u{200a}' | '\u{202f}' | '\u{205f}' | '\u{3000}'
    )
}

/// `\v`: Java's vertical whitespace.
fn vertical(c: char) -> bool {
    matches!(
        c,
        '\n' | '\u{b}' | '\u{c}' | '\r' | '\u{85}' | '\u{2028}' | '\u{2029}'
    )
}

/// `\s` without `UNICODE_CHARACTER_CLASS`.
fn space(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\n' | '\u{b}' | '\u{c}' | '\r')
}

/// What `.` does not match.
fn line_terminator(c: char) -> bool {
    matches!(c, '\n' | '\r' | '\u{85}' | '\u{2028}' | '\u{2029}')
}

/// The end of `/\*(?:[^*]|\*+[^*/])*\*+/` from `at`: the comment closes at its first `*/`.
fn block_comment(c: &[char], at: usize) -> Option<usize> {
    if c.get(at) != Some(&'/') || c.get(at + 1) != Some(&'*') {
        return None;
    }
    (at + 2..c.len().saturating_sub(1))
        .find(|&i| c[i] == '*' && c[i + 1] == '/')
        .map(|i| i + 2)
}

/// `(?:comment|\h)*` from `at`: where it stops. Each iteration starts with `/` or `\h`, so no shorter run could be
/// followed by a letter, a quote or a bracket: the greedy run is the only one that can match.
fn skip_spacing(c: &[char], mut at: usize) -> usize {
    loop {
        if let Some(end) = block_comment(c, at) {
            at = end;
        } else if c.get(at).copied().is_some_and(horizontal) {
            at += 1;
        } else {
            return at;
        }
    }
}

/// `Matcher.find` of `REGEX_MOJ_IMPORT` from `from`.
fn find_import(c: &[char], from: usize) -> Option<Found> {
    const WORD: [char; 10] = ['m', 'o', 'j', '_', 'i', 'm', 'p', 'o', 'r', 't'];
    (from..c.len())
        .filter(|&start| c[start] == '#')
        .find_map(|start| {
            let at = skip_spacing(c, start + 1);
            if c.get(at..at + WORD.len()) != Some(&WORD[..]) {
                return None;
            }
            let at = skip_spacing(c, at + WORD.len());
            let (open, close) = match c.get(at) {
                Some('"') => ('"', '"'),
                Some('<') => ('<', '>'),
                _ => return None,
            };
            let line_end = (at + 1..c.len())
                .find(|&i| line_terminator(c[i]))
                .unwrap_or(c.len());
            // `(.*)` is greedy: the last closing character on the line.
            let close_at = (at + 1..line_end).rev().find(|&i| c[i] == close)?;
            Some(Found {
                start,
                end: close_at + 1,
                group: at + 1..close_at,
                quoted: open == '"',
            })
        })
}

/// `GlslPreprocessor.isDirectiveDisabled`, quirk kept: the end of the last line comment, relative to `previous`, is
/// compared with the directive's absolute start.
fn disabled(c: &[char], utf16: &[usize], start: usize, previous: usize) -> bool {
    if start == previous {
        return false;
    }
    let sub = &c[previous..start];
    match ends_with_whitespace(sub) {
        None => true,
        Some(None) => false,
        Some(Some(end)) => utf16[previous + end] - utf16[previous] == utf16[start],
    }
}

/// `(?:^|\v)(?:\s|/\*...\*+/|(//[^\v]*))*\z` found in `sub`: `None` when it does not match, else the end of
/// group 1 on the path Java's backtracking takes (`Some(None)` when no line comment is on it).
fn ends_with_whitespace(sub: &[char]) -> Option<Option<usize>> {
    let m = sub.len();
    // tail[i]: the loop from i reaches the end, with the end of the last line comment on the path taken.
    let mut tail: Vec<Option<Option<usize>>> = vec![None; m + 1];
    tail[m] = Some(None);
    for i in (0..m).rev() {
        tail[i] = if space(sub[i]) && tail[i + 1].is_some() {
            tail[i + 1]
        } else if let Some(end) = block_comment(sub, i).filter(|&end| tail[end].is_some()) {
            tail[end]
        } else if sub[i] == '/' && sub.get(i + 1) == Some(&'/') {
            let line_end = (i + 2..m).find(|&k| vertical(sub[k])).unwrap_or(m);
            (i + 2..=line_end)
                .rev()
                .find_map(|k| tail[k].map(|last| Some(last.unwrap_or(k))))
        } else {
            None
        };
    }
    if let Some(found) = tail[0] {
        return Some(found);
    }
    (0..m)
        .filter(|&p| vertical(sub[p]))
        .find_map(|p| tail[p + 1])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raws(text: &str) -> Vec<(String, Role)> {
        imports(text.as_bytes())
            .into_iter()
            .map(|token| (token.raw.to_string(), token.role))
            .collect()
    }

    #[test]
    fn quoted_and_angled_imports_with_their_spans() {
        let text = "#version 150\n#moj_import <fx:e.glsl>\n# /* c */ moj_import \"parts/c.glsl\"\n";
        let tokens = imports(text.as_bytes());
        assert_eq!(
            raws(text),
            [
                ("fx:e.glsl".into(), Role::ShaderImportAngled),
                ("parts/c.glsl".into(), Role::ShaderImportQuoted)
            ]
        );
        assert_eq!(
            &text[tokens[0].span.start as usize..tokens[0].span.end as usize],
            "fx:e.glsl"
        );
    }

    /// The client's quirk: only before the first directive does a line comment disable an import.
    #[test]
    fn a_commented_import_counts_only_after_the_first_directive() {
        assert_eq!(raws("// #moj_import <a.glsl>\n"), []);
        assert_eq!(
            raws("#moj_import <a.glsl>\n// #moj_import <b.glsl>\n"),
            [
                ("a.glsl".into(), Role::ShaderImportAngled),
                ("b.glsl".into(), Role::ShaderImportAngled)
            ]
        );
        // Code on earlier lines does not disable a directive; code before it on its own line does.
        assert_eq!(
            raws("#moj_import <a.glsl>\nvoid main() {}\n#moj_import <b.glsl>\n"),
            [
                ("a.glsl".into(), Role::ShaderImportAngled),
                ("b.glsl".into(), Role::ShaderImportAngled)
            ]
        );
        assert_eq!(
            raws("#moj_import <a.glsl>\nx #moj_import <b.glsl>\n"),
            [("a.glsl".into(), Role::ShaderImportAngled)]
        );
        // Java backtracks a line comment to let a block comment span the next line.
        assert_eq!(
            raws("#moj_import <a.glsl>\n// a /* b \n c */ #moj_import <b.glsl>\n").len(),
            2
        );
        assert_eq!(
            raws("x\n/* c */ #moj_import <a>\n// q\n#moj_import <b>").len(),
            2
        );
    }

    #[test]
    fn greedy_group_takes_the_last_quote_of_the_line() {
        assert_eq!(
            raws("#moj_import \"a\" \"b\"\n"),
            [("a\" \"b".into(), Role::ShaderImportQuoted)]
        );
        assert_eq!(raws("#moj_import \"a\n\"\n"), []);
    }
}
