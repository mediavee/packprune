#![no_main]

use std::collections::BTreeMap;

use libfuzzer_sys::fuzz_target;
use packprune_pack::json::{self, TokenKind};

// The tokenizer never panics; on any input it accepts, an identity rewrite gives the bytes back and setting every
// string changes those strings' bytes only.
fuzz_target!(|data: &[u8]| {
    let mut strings = BTreeMap::new();
    let mut spans = Vec::new();
    let read = json::visit(data, |token, path| {
        assert!(token.span.start < token.span.end && token.span.end <= data.len());
        if token.kind == TokenKind::String {
            strings.insert(path.pointer(), format!("{}!", token.text));
            spans.push(token.span.clone());
        }
    });
    assert_eq!(read.is_ok(), json::parses(data));
    if json::validate(data).is_ok() {
        assert!(read.is_ok());
    }
    if read.is_err() {
        return;
    }
    let same = json::rewrite_numbers(data, |token, _| Some(token.text.to_string())).unwrap();
    assert_eq!(same, data);
    assert_eq!(json::splice(data, Vec::new()), data);
    if strings.len() != spans.len() {
        return;
    }
    let edited = json::edit(data, &strings, &BTreeMap::new()).unwrap();
    let mut texts = BTreeMap::new();
    let mut edited_spans = Vec::new();
    json::visit(&edited, |token, path| {
        if token.kind == TokenKind::String {
            texts.insert(path.pointer(), token.text.to_string());
            edited_spans.push(token.span.clone());
        }
    })
    .unwrap();
    assert_eq!(texts, strings);
    let outside = |bytes: &[u8], spans: &[std::ops::Range<usize>]| {
        let mut out = Vec::new();
        let mut at = 0;
        for span in spans {
            out.extend_from_slice(&bytes[at..span.start]);
            out.push(0);
            at = span.end;
        }
        out.extend_from_slice(&bytes[at..]);
        out
    };
    assert_eq!(outside(data, &spans), outside(&edited, &edited_spans));
});
