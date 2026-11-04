use std::mem;

use packprune_graph::UsageKind;
use packprune_pack::{ResourceId, RunCx};

use crate::rules::{BLOCKS_ATLAS, Reference, Rule};

pub(super) fn references(text: &str, cx: &RunCx) -> Vec<Reference> {
    let mut out = Vec::new();
    for (open, _) in text.match_indices('<') {
        if text[..open]
            .bytes()
            .rev()
            .take_while(|b| *b == b'\\')
            .count()
            % 2
            == 1
        {
            continue;
        }
        let mut quote = None;
        let mut escape = false;
        let close = text[open + 1..].char_indices().find_map(|(i, c)| {
            if escape {
                escape = false;
                return None;
            }
            if c == '\\' && quote.is_some() {
                escape = true;
            } else if let Some(q) = quote {
                if c == q {
                    quote = None;
                }
            } else if matches!(c, '\'' | '"') {
                quote = Some(c);
            } else if c == '>' {
                return Some(open + 1 + i);
            }
            None
        });
        let Some(close) = close else {
            continue;
        };
        let body = &text[open + 1..close];
        let body = body.strip_suffix('/').unwrap_or(body);
        let mut args = Vec::new();
        let mut arg = String::new();
        let mut quote = None;
        let mut chars = body.chars();
        while let Some(c) = chars.next() {
            if c == '\\' && quote.is_some() {
                if let Some(next) = chars.next() {
                    arg.push(next);
                }
            } else if let Some(q) = quote {
                if c == q {
                    quote = None;
                } else {
                    arg.push(c);
                }
            } else if matches!(c, '\'' | '"') {
                quote = Some(c);
            } else if c == ':' {
                args.push(mem::take(&mut arg));
            } else {
                arg.push(c);
            }
        }
        args.push(arg);
        if args.len() < 2 {
            continue;
        }
        let (rule, value) = match args[0].to_ascii_lowercase().as_str() {
            "font" => (
                Rule::simple(UsageKind::Font),
                if args.len() == 2 {
                    args[1].clone()
                } else {
                    format!("{}:{}", args[1], args[2])
                },
            ),
            "sprite" => {
                let atlas = if args.len() == 2 {
                    BLOCKS_ATLAS
                } else {
                    &args[1]
                };
                let Some(atlas) = ResourceId::parse(&cx.interner, atlas) else {
                    continue;
                };
                let mut rule = Rule::simple(UsageKind::Sprite);
                rule.atlas = Some(atlas);
                (rule, args[if args.len() == 2 { 1 } else { 2 }].clone())
            }
            _ => continue,
        };
        out.push(Reference {
            rule,
            value,
            at: text[open..=close].into(),
        });
    }
    out
}
