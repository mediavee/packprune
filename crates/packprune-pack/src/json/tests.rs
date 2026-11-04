use std::collections::BTreeMap;
use std::fmt::Write;

use super::*;

/// One file's tokens as `tok <JACKSON_TYPE> <pointer> <start> <hex text>` lines.
fn dump(json: &[u8]) -> String {
    let mut out = format!(
        "parses\t{}\ntree\t{}\n",
        parses(json),
        validate(json).is_ok()
    );
    let result = visit(json, |token, path| {
        let kind = match token.kind {
            TokenKind::StartObject => "START_OBJECT",
            TokenKind::EndObject => "END_OBJECT",
            TokenKind::StartArray => "START_ARRAY",
            TokenKind::EndArray => "END_ARRAY",
            TokenKind::Name => "PROPERTY_NAME",
            TokenKind::String => "VALUE_STRING",
            TokenKind::Int => "VALUE_NUMBER_INT",
            TokenKind::Float => "VALUE_NUMBER_FLOAT",
            TokenKind::True => "VALUE_TRUE",
            TokenKind::False => "VALUE_FALSE",
            TokenKind::Null => "VALUE_NULL",
        };
        let text = match token.kind {
            TokenKind::Name | TokenKind::String => {
                token.text.bytes().fold(String::new(), |mut hex, byte| {
                    write!(hex, "{byte:02x}").unwrap();
                    hex
                })
            }
            _ => String::new(),
        };
        writeln!(
            out,
            "tok\t{kind}\t{}\t{}\t{text}",
            path.pointer().0,
            token.span.start
        )
        .unwrap();
    });
    if result.is_err() {
        out.push_str("err\n");
    }
    out
}

/// Grammar verdicts of Jackson 3.2.3 with its default features: as a token stream and as one tree.
#[test]
fn grammar_is_jackson_s() {
    let deep = |depth: usize| [b"[".repeat(depth), b"]".repeat(depth)].concat();
    let number = |text: String| format!("[{text}]").into_bytes();
    #[rustfmt::skip]
    let cases: Vec<(&str, Vec<u8>, bool, bool)> = vec![
        ("empty", b"".to_vec(), true, true),
        ("ws", b"  \n".to_vec(), true, true),
        ("obj", br#"{"a":1}"#.to_vec(), true, true),
        ("two_roots", b"{} {}".to_vec(), true, false),
        ("two_roots_nows", b"{}{}".to_vec(), true, false),
        ("scalar", b"12".to_vec(), true, true),
        ("str_root", br#""x""#.to_vec(), true, true),
        ("trailing_comma_obj", br#"{"a":1,}"#.to_vec(), false, false),
        ("trailing_comma_arr", b"[1,]".to_vec(), false, false),
        ("comment", br#"{"a":1}//x"#.to_vec(), false, false),
        ("hash", b"#x\n{}".to_vec(), false, false),
        ("bom", b"\xef\xbb\xbf{\"a\":\"b\"}".to_vec(), true, true),
        ("only_bom", b"\xef\xbb\xbf".to_vec(), false, false),
        ("ctrl_in_str", b"{\"a\":\"x\ty\"}".to_vec(), false, false),
        ("bad_escape", br#"{"a":"\q"}"#.to_vec(), false, false),
        ("sq_escape", br#"{"a":"\'"}"#.to_vec(), false, false),
        ("u_escape", br#"{"a":"\u00e9\ud83d\ude00"}"#.to_vec(), true, true),
        ("lone_surrogate", br#"{"a":"\ud800"}"#.to_vec(), true, true),
        ("esc_upper_u", br#"{"a":"\U0041"}"#.to_vec(), false, false),
        ("esc_short_u", br#"{"a":"\u00g1"}"#.to_vec(), false, false),
        ("esc_slash", br#"{"a":"\/"}"#.to_vec(), true, true),
        ("del_char", b"{\"a\":\"\x7f\"}".to_vec(), true, true),
        ("c1_ctrl", b"{\"a\":\"\xc2\x80\"}".to_vec(), true, true),
        ("bad_utf8_str", b"{\"a\":\"\xff\"}".to_vec(), false, false),
        ("bad_utf8_key", b"{\"\xff\":1}".to_vec(), false, false),
        ("overlong", b"{\"a\":\"\xc0\xaf\"}".to_vec(), true, true),
        ("e0_low", b"{\"a\":\"\xe0\x80\x80\"}".to_vec(), true, true),
        ("cesu", b"{\"a\":\"\xed\xa0\x80\"}".to_vec(), false, false),
        ("four_byte", b"{\"a\":\"\xf0\x9f\x98\x80\"}".to_vec(), true, true),
        ("f5", b"{\"a\":\"\xf5\x80\x80\x80\"}".to_vec(), true, true),
        ("f8", b"{\"a\":\"\xf8\x80\x80\x80\x80\"}".to_vec(), false, false),
        ("cont_lead", b"{\"a\":\"\x80\"}".to_vec(), false, false),
        ("trunc2", b"{\"a\":\"\xc3\"}".to_vec(), false, false),
        ("trunc3", b"{\"a\":\"\xe2\x82\"}".to_vec(), false, false),
        ("utf8_ok", "{\"é/~\":[\"ü\"]}".as_bytes().to_vec(), true, true),
        ("name_ctrl", b"{\"a\tb\":1}".to_vec(), false, false),
        ("name_esc", br#"{"a\u0041":1}"#.to_vec(), true, true),
        ("name_utf8", "{\"é\":1}".as_bytes().to_vec(), true, true),
        ("name_overlong", b"{\"\xc0\xaf\":1}".to_vec(), true, true),
        ("name_f5", b"{\"\xf5\x80\x80\x80\":1}".to_vec(), true, true),
        ("name_cesu", b"{\"\xed\xa0\x80\":1}".to_vec(), false, false),
        ("name_cont_lead", b"{\"\x80\":1}".to_vec(), false, false),
        ("name_ff", b"{\"\xff\":1}".to_vec(), false, false),
        ("empty_key", br#"{"":""}"#.to_vec(), true, true),
        ("lead_zero", b"[01]".to_vec(), false, false),
        ("minus_zero_lead", b"[-01]".to_vec(), false, false),
        ("neg_zero", b"[-0]".to_vec(), true, true),
        ("minus", b"[-]".to_vec(), false, false),
        ("neg_letter", b"[-a]".to_vec(), false, false),
        ("dot", b"[1.]".to_vec(), false, false),
        ("dot_lead", b"[.5]".to_vec(), false, false),
        ("zero_dot_e", b"[0.e1]".to_vec(), false, false),
        ("zero_e", b"[0e1]".to_vec(), true, true),
        ("plus", b"[+1]".to_vec(), false, false),
        ("exp", b"[1e5,1E+5,1e-5,1.5e3]".to_vec(), true, true),
        ("e_capital_plus", b"[1E+2]".to_vec(), true, true),
        ("exp_empty", b"[1e]".to_vec(), false, false),
        ("big", b"[1e400, 123456789012345678901234567890]".to_vec(), true, true),
        ("nan", b"[NaN]".to_vec(), false, false),
        ("inf", b"[Infinity]".to_vec(), false, false),
        ("num_then_letter", b"[1x]".to_vec(), false, false),
        ("num_minus", b"[1-2]".to_vec(), false, false),
        ("numletter_root", b"1x".to_vec(), false, false),
        ("num_root_space", b"1 ".to_vec(), true, true),
        ("num_root_nl", b"1\n".to_vec(), true, true),
        ("num_root_brace", b"1}".to_vec(), false, false),
        ("num_root_comma", b"1,".to_vec(), false, false),
        ("root_neg", b"-1".to_vec(), true, true),
        ("long_num_ok", number("1".repeat(1000)), true, true),
        ("long_num", number("1".repeat(1001)), false, false),
        ("neg_long", number(format!("-{}", "1".repeat(1000))), true, true),
        ("neg_long_1001", number(format!("-{}", "1".repeat(1001))), false, false),
        ("frac_999", number(format!("0.{}", "1".repeat(997))), true, true),
        ("frac_1000", number(format!("0.{}", "1".repeat(998))), true, true),
        ("frac_long", number(format!("0.{}", "1".repeat(1000))), false, false),
        ("frac_int_1001", number(format!("{}.{}", "1".repeat(500), "1".repeat(500))), true, true),
        ("exp_long", number(format!("1{}e{}", "0".repeat(500), "1".repeat(499))), true, true),
        ("exp_long2", number(format!("1{}e{}", "0".repeat(500), "1".repeat(500))), false, false),
        ("true", b"[true,false,null]".to_vec(), true, true),
        ("tru", b"[tru]".to_vec(), false, false),
        ("truex", b"truex".to_vec(), false, false),
        ("true_space_x", b"true x".to_vec(), false, false),
        ("true_bracket", b"[true]x".to_vec(), false, false),
        ("true_comma", b"[true,]".to_vec(), false, false),
        ("nul_root", b"null".to_vec(), true, true),
        ("false_root", b"false ".to_vec(), true, true),
        ("unclosed", br#"{"a":1"#.to_vec(), false, false),
        ("obj_open", b"{".to_vec(), false, false),
        ("arr_open", b"[".to_vec(), false, false),
        ("key_unclosed", br#"{"a"#.to_vec(), false, false),
        ("extra_close", b"{}}".to_vec(), false, false),
        ("nul_ws", b"{}\x00".to_vec(), false, false),
        ("ff_ws", b"{\x0c}".to_vec(), false, false),
        ("vt_ws", b"\x0b{}".to_vec(), false, false),
        ("utf8_in_ws", b"{\xc2\xa0}".to_vec(), false, false),
        ("cr", b"{\r\n\"a\"\r:1}".to_vec(), true, true),
        ("dup", br#"{"a":1,"a":2}"#.to_vec(), true, true),
        ("key_num", b"{1:2}".to_vec(), false, false),
        ("colon_missing", br#"{"a" 1}"#.to_vec(), false, false),
        ("space_before_colon", br#"{ "a" : [ 1 , 2 ] }"#.to_vec(), true, true),
        ("obj_in_arr_close", br#"[{"a":[]},{}]"#.to_vec(), true, true),
        ("deep_ok", deep(400), true, true),
        ("deep_500", deep(500), true, true),
        ("deep_501", deep(501), false, false),
        ("deep_1000", deep(1000), false, false),
    ];
    for (name, json, splice, tree) in cases {
        assert_eq!(
            (parses(&json), validate(&json).is_ok()),
            (splice, tree),
            "{name}"
        );
    }
}

/// An unterminated string, UTF-16 and UTF-32 are invalid: the client reads UTF-8 only.
#[test]
fn unterminated_strings_and_wide_encodings_are_refused() {
    let utf16: Vec<u8> = "\u{feff}{\"a\":1}"
        .encode_utf16()
        .flat_map(u16::to_be_bytes)
        .collect();
    for json in [
        br#"{"a":"x"#.to_vec(),
        br#"{"a":"\"#.to_vec(),
        br#""abc"#.to_vec(),
        utf16,
        b"{\x00\"\x00".to_vec(),
    ] {
        assert!(!parses(&json), "{json:?}");
    }
}

/// Jackson's tokens for this file.
#[test]
fn pointers_and_offsets_are_jackson_s() {
    let expected = "parses\ttrue\ntree\ttrue\n\
        tok\tSTART_OBJECT\t\t0\t\n\
        tok\tPROPERTY_NAME\t/a~1b\t1\t612f62\n\
        tok\tSTART_OBJECT\t/a~1b\t7\t\n\
        tok\tPROPERTY_NAME\t/a~1b/c~0d\t8\t637e64\n\
        tok\tSTART_ARRAY\t/a~1b/c~0d\t14\t\n\
        tok\tVALUE_NUMBER_INT\t/a~1b/c~0d/0\t15\t\n\
        tok\tSTART_OBJECT\t/a~1b/c~0d/1\t17\t\n\
        tok\tPROPERTY_NAME\t/a~1b/c~0d/1/\t18\t\n\
        tok\tVALUE_STRING\t/a~1b/c~0d/1/\t21\t78\n\
        tok\tEND_OBJECT\t/a~1b/c~0d/1\t24\t\n\
        tok\tEND_ARRAY\t/a~1b/c~0d\t25\t\n\
        tok\tEND_OBJECT\t/a~1b\t26\t\n\
        tok\tEND_OBJECT\t\t27\t\n";
    assert_eq!(dump(br#"{"a/b":{"c~d":[1,{"":"x"}]}}"#), expected);
    let bom = "parses\ttrue\ntree\ttrue\n\
        tok\tSTART_OBJECT\t\t3\t\n\
        tok\tPROPERTY_NAME\t/a\t4\t61\n\
        tok\tVALUE_STRING\t/a\t8\t62\n\
        tok\tEND_OBJECT\t\t11\t\n";
    assert_eq!(dump(b"\xef\xbb\xbf{\"a\":\"b\"}"), bom);
    let escapes = "parses\ttrue\ntree\ttrue\n\
        tok\tSTART_OBJECT\t\t0\t\n\
        tok\tPROPERTY_NAME\t/a\t1\t61\n\
        tok\tVALUE_STRING\t/a\t5\tc3a9f09f9880\n\
        tok\tEND_OBJECT\t\t25\t\n";
    assert_eq!(dump(br#"{"a":"\u00e9\ud83d\ude00"}"#), escapes);
}

#[test]
fn spans_cover_each_token() {
    let json = br#" {"k" : [ -1.5e3 , "v\"" , true ] } "#;
    let mut spans = Vec::new();
    visit(json, |token, _| {
        spans.push((token.kind, &json[token.span.clone()]))
    })
    .unwrap();
    assert_eq!(
        spans,
        [
            (TokenKind::StartObject, &b"{"[..]),
            (TokenKind::Name, br#""k""#),
            (TokenKind::StartArray, b"["),
            (TokenKind::Float, b"-1.5e3"),
            (TokenKind::String, br#""v\"""#),
            (TokenKind::True, b"true"),
            (TokenKind::EndArray, b"]"),
            (TokenKind::EndObject, b"}"),
        ]
    );
}

#[test]
fn edit_changes_only_the_named_strings() {
    let json = b"{ \"a\" : \"x\",\n  \"b\":[1.50, \"y\"] }";
    let strings = BTreeMap::from([
        (Pointer::new("/a"), "new \"q\"\u{1}".to_owned()),
        (Pointer::new("/b/1"), "z".to_owned()),
    ]);
    let out = edit(json, &strings, &BTreeMap::new()).unwrap();
    assert_eq!(
        String::from_utf8(out).unwrap(),
        "{ \"a\" : \"new \\\"q\\\"\\u0001\",\n  \"b\":[1.50, \"z\"] }"
    );
}

#[test]
fn edit_appends_fields_at_the_end_of_an_object() {
    let json = br#"{"a":{"x":1},"b":{}}"#;
    let field = |name: &str| Field {
        name: name.into(),
        value: "v".into(),
    };
    let fields = BTreeMap::from([
        (Pointer::new("/a"), vec![field("y"), field("z")]),
        (Pointer::new("/b"), vec![field("y"), field("z")]),
    ]);
    let out = edit(json, &BTreeMap::new(), &fields).unwrap();
    // An empty object gets no leading comma, so the result stays JSON.
    assert_eq!(
        String::from_utf8(out.clone()).unwrap(),
        r#"{"a":{"x":1,"y":"v","z":"v"},"b":{"y":"v","z":"v"}}"#
    );
    assert!(parses(&out));
}

#[test]
fn edit_refuses_a_pointer_that_names_nothing_of_its_kind() {
    let json = br#"{"a":1,"o":{}}"#;
    let strings = BTreeMap::from([(Pointer::new("/a"), "x".to_owned())]);
    assert!(matches!(
        edit(json, &strings, &BTreeMap::new()),
        Err(EditError::Unmatched { .. })
    ));
    let fields = BTreeMap::from([(Pointer::new("/a"), vec![])]);
    assert!(edit(json, &BTreeMap::new(), &fields).is_err());
    assert!(matches!(
        edit(b"{", &BTreeMap::new(), &BTreeMap::new()),
        Err(EditError::Json(_))
    ));
}

#[test]
fn rewrite_numbers_touches_numbers_only() {
    let json = b"{\"n\": 1.000, \"s\": \"1.000\", \"i\": [7, 1e2]}";
    let out = rewrite_numbers(json, |token, path| {
        (token.kind == TokenKind::Float)
            .then(|| format!("{}{}", path.pointer().0.len(), token.text.len()))
    })
    .unwrap();
    assert_eq!(out, b"{\"n\": 25, \"s\": \"1.000\", \"i\": [7, 43]}");
    let same = rewrite_numbers(json, |token, _| Some(token.text.to_string())).unwrap();
    assert_eq!(same, json);
}

#[test]
fn splice_keeps_every_byte_outside_the_ranges() {
    let json = b"0123456789";
    let out = splice(
        json,
        vec![
            Splice {
                range: 7..8,
                bytes: b"x".to_vec(),
            },
            Splice {
                range: 2..2,
                bytes: b"ab".to_vec(),
            },
            Splice {
                range: 4..6,
                bytes: Vec::new(),
            },
        ],
    );
    assert_eq!(out, b"01ab236x89");
    assert_eq!(splice(json, Vec::new()), json);
}

/// The splice round trip on every JSON and mcmeta of corpus packs: an identity rewrite gives the file back byte for
/// byte, and setting every string to its own text keeps every byte outside the strings.
/// `PACKPRUNE_JSON_CORPUS` lists pack zips, `;`-separated.
#[test]
#[ignore = "requires PACKPRUNE_JSON_CORPUS (`;`-separated pack zips)"]
fn corpus_round_trips_keep_every_byte() {
    let zips = std::env::var("PACKPRUNE_JSON_CORPUS").expect("PACKPRUNE_JSON_CORPUS");
    for zip in zips.split(';') {
        let pack = crate::PackInput::open(zip.into(), &crate::Limits::default()).unwrap();
        let mut files = 0;
        for (path, entry) in &pack.files {
            let name = path.as_str();
            if !(name.ends_with(".json") || name.ends_with(".mcmeta")) {
                continue;
            }
            files += 1;
            let json = &entry.blob[..];
            assert_eq!(
                rewrite_numbers(json, |token, _| Some(token.text.to_string())).unwrap(),
                json,
                "{name}"
            );
            let mut strings = BTreeMap::new();
            let mut spans = Vec::new();
            visit(json, |token, path| {
                if token.kind == TokenKind::String {
                    strings.insert(path.pointer(), token.text.to_string());
                    spans.push(token.span.clone());
                }
            })
            .unwrap();
            // A duplicate key names one pointer twice, which `edit` refuses.
            if strings.len() != spans.len() {
                continue;
            }
            let edited = edit(json, &strings, &BTreeMap::new()).unwrap();
            let mut outside = Vec::new();
            let mut at = 0;
            for span in &spans {
                outside.extend_from_slice(&json[at..span.start]);
                outside.push(b'"');
                at = span.end;
            }
            outside.extend_from_slice(&json[at..]);
            let mut edited_outside = Vec::new();
            let mut at = 0;
            visit(&edited, |token, _| {
                if token.kind == TokenKind::String {
                    edited_outside.extend_from_slice(&edited[at..token.span.start]);
                    edited_outside.push(b'"');
                    at = token.span.end;
                }
            })
            .unwrap();
            edited_outside.extend_from_slice(&edited[at..]);
            assert_eq!(outside, edited_outside, "{name}");
            let mut texts = BTreeMap::new();
            visit(&edited, |token, path| {
                if token.kind == TokenKind::String {
                    texts.insert(path.pointer(), token.text.to_string());
                }
            })
            .unwrap();
            assert_eq!(texts, strings, "{name}");
        }
        eprintln!("{zip}: {files} files");
    }
}

/// The lenient tree of `json` in a compact notation: strings Debug-quoted, numbers as written, `<empty>` for no value.
fn lenient(json: &str) -> Result<String, JsonError> {
    fn show(node: &Node, out: &mut String) {
        match &node.value {
            Value::Object(members) => {
                out.push('{');
                for (index, member) in members.iter().enumerate() {
                    if index > 0 {
                        out.push(',');
                    }
                    out.push_str(&format!("{:?}:", member.name));
                    show(&member.node, out);
                }
                out.push('}');
            }
            Value::Array(items) => {
                out.push('[');
                for (index, item) in items.iter().enumerate() {
                    if index > 0 {
                        out.push(',');
                    }
                    show(item, out);
                }
                out.push(']');
            }
            Value::String(text) => out.push_str(&format!("{text:?}")),
            Value::Int(number) => out.push_str(&number.to_string()),
            Value::Long(number) => out.push_str(&number.to_string()),
            Value::Big(text) => out.push_str(text),
            Value::Double(number) => out.push_str(&java_double(*number)),
            Value::Bool(flag) => out.push_str(&flag.to_string()),
            Value::Null => out.push_str("null"),
            Value::Missing => out.push_str("<empty>"),
        }
    }
    let mut out = String::new();
    show(&parse_lenient(json.as_bytes())?, &mut out);
    Ok(out)
}

/// Every expectation below is what Gson 2.14's `fromJson(Reader, JsonElement.class)` returned for the input on a JVM
/// (the grammar `FontManager.GSON` reads fonts with).
#[test]
fn the_lenient_grammar_takes_what_gson_lenient_takes() {
    for (input, tree) in [
        // A font as a hand-writer would, with a comment in it.
        (
            "// header\n{ /* why */ \"providers\": [ # first\n {\"type\":\"space\"} ] }",
            r#"{"providers":[{"type":"space"}]}"#,
        ),
        ("{a:1}", r#"{"a":1}"#),
        ("{'a':'b'}", r#"{"a":"b"}"#),
        (r#"{"a":x}"#, r#"{"a":"x"}"#),
        (
            "{type: bitmap, ascent: 7}",
            r#"{"type":"bitmap","ascent":7}"#,
        ),
        (r#"{"a"=1,"b"=>2}"#, r#"{"a":1,"b":2}"#),
        (r#"{"a":1;"b":2}"#, r#"{"a":1,"b":2}"#),
        (r#"["it\'s"]"#, r#"["it's"]"#),
        ("[TRUE,nuLL,False]", "[true,null,false]"),
        // A number that does not end a literal is a string.
        ("[01,1.,1a,-,.5]", r#"["01","1.","1a","-",".5"]"#),
        (
            "[1e5,-0,0.5e-3,12345678901,99999999999999999999]",
            "[100000.0,0,5.0E-4,12345678901,99999999999999999999]",
        ),
        ("{\"a\":\"x\ny\"}", r#"{"a":"x\ny"}"#),
        ("\u{feff}{\"a\":1}", r#"{"a":1}"#),
        (")]}'\n{\"a\":1}", r#"{"a":1}"#),
        ("[\"a\\\nb\"]", r#"["a\nb"]"#),
        ("5", "5"),
        ("abc", r#""abc""#),
        ("", "<empty>"),
        ("// only a comment", "<empty>"),
        ("{\"a\":1}\n", r#"{"a":1}"#),
    ] {
        assert_eq!(lenient(input).as_deref(), Ok(tree), "{input:?}");
    }
}

/// Gson keeps a lone surrogate in its `String`; ours cannot, and U+FFFD in its place would make two names one.
#[test]
fn a_lone_surrogate_escape_is_refused_by_the_lenient_grammar_and_a_pair_is_kept() {
    for lone in [
        r#"{"\ud800":1}"#,
        r#"["\udc00"]"#,
        r#"["\ud800A"]"#,
        r#"["\ude00\ud83d"]"#,
        r#"{"a":'\ud800'}"#,
    ] {
        assert!(lenient(lone).is_err(), "{lone}");
    }
    assert_eq!(
        lenient(r#"["😀","a😀b"]"#).as_deref(),
        Ok(r#"["😀","a😀b"]"#)
    );
    assert!(parse(br#"{"a":"\ud800"}"#).is_ok(), "Jackson reads it");
}

/// An omitted array element is `null` to Gson, so `[1,]` has two elements; the client's font codec sees the `null`.
#[test]
fn an_omitted_array_element_reads_as_null() {
    for (input, tree) in [
        ("[1,]", "[1,null]"),
        ("[1,,2]", "[1,null,2]"),
        ("[,]", "[null,null]"),
        ("[;]", "[null,null]"),
        (r#"{"x":[{},]}"#, r#"{"x":[{},null]}"#),
    ] {
        assert_eq!(lenient(input).as_deref(), Ok(tree), "{input}");
    }
    let mut kinds = Vec::new();
    visit_lenient(b"[1,]", |token, path| {
        kinds.push((token.kind, path.pointer()))
    })
    .unwrap();
    assert_eq!(kinds[2], (TokenKind::Null, Pointer::new("/1")));
}

#[test]
fn the_lenient_grammar_still_refuses_what_gson_lenient_refuses() {
    for broken in [
        r#"{"a":1,}"#,
        "{a:1,}",
        "{,}",
        "{;}",
        r#"{"a":}"#,
        r#"{"a":,}"#,
        "{a:1 b:2}",
        "{a b:1}",
        r#"{"a" 1}"#,
        r#"{"a"::1}"#,
        "[1 2]",
        "[a:b]",
        "/* open",
        "[1 /*",
        "[1 //x]",
        "{a/b:1}",
        r#"["\q"]"#,
        r#"["\u00zz"]"#,
        "'open",
        // Only whitespace may follow the root value: Gson checks it after its leniency is restored.
        r#"{"a":1} {"b":2}"#,
        r#"{"a":1}//c"#,
        r#"{"a":1}x"#,
        "\u{c}{}",
    ] {
        assert!(lenient(broken).is_err(), "{broken}");
    }
}

/// Gson 2.14's nesting limit is 255 containers (Jackson's, in the strict grammar, is 500).
#[test]
fn the_lenient_grammar_nests_255_deep() {
    let nested = |depth: usize| format!("{}{}", "[".repeat(depth), "]".repeat(depth));
    assert!(lenient(&nested(255)).is_ok());
    assert!(lenient(&nested(256)).is_err());
}

#[test]
fn the_tree_keeps_first_seen_order_with_spans_and_takes_one_root() {
    let json = br#"{"a":1,"b":[2],"a":3}"#;
    for node in [parse(json).unwrap(), parse_lenient(json).unwrap()] {
        let names: Vec<_> = node.members().iter().map(|m| &*m.name).collect();
        assert_eq!(names, ["a", "b"]);
        assert_eq!(node.get("a").unwrap().value, Value::Int(3));
        assert_eq!(node.span, 0..json.len() as u32);
        assert_eq!(node.members()[1].name_span, 7..10);
        assert_eq!(node.get("b").unwrap().span, 11..14);
    }
    assert!(matches!(parse(b"").unwrap().value, Value::Missing));
    assert_eq!(parse(b"1.5").unwrap().value, Value::Double(1.5));
    assert!(parse(b"{} {}").is_err());
    assert!(parse(b"1 2").is_err());
    assert!(parse(br#"{"a":1,}"#).is_err());
    assert!(parse_lenient(b"{} {}").is_err());
}

/// Jackson 3 `readTree`'s number nodes and the coercions the graph and the oracle call on them.
#[test]
fn numbers_are_jackson_nodes_and_coerce_as_jackson_does() {
    let node = parse(
        br#"{"i": 7, "l": 3000000000, "big": 99999999999999999999, "d": 2.9, "s": "12", "f": "1.5", "b": true, "n": null}"#,
    )
    .unwrap();
    assert_eq!(node.path("i").value, Value::Int(7));
    assert_eq!(node.path("l").value, Value::Long(3_000_000_000));
    assert_eq!(
        node.path("big").value,
        Value::Big("99999999999999999999".into())
    );
    assert_eq!(node.path("i").as_int(0), 7);
    assert_eq!(node.path("d").as_int(0), 2);
    assert_eq!(node.path("s").as_int(0), 12);
    assert_eq!(node.path("l").as_int(-1), -1);
    assert_eq!(node.path("big").as_int(-1), -1);
    assert_eq!(node.path("f").as_int(-1), -1);
    assert_eq!(node.path("b").as_int(-1), -1);
    assert_eq!(node.path("n").as_int(-1), -1);
    assert_eq!(node.path("absent").as_int(-1), -1);
    assert!(node.path("i").as_bool(false));
    assert!(!node.path("d").as_bool(false));
    assert!(node.path("b").as_bool(false));
    assert_eq!(node.path("d").as_string(""), "2.9");
    assert_eq!(node.path("i").as_string(""), "7");
    assert_eq!(node.path("l").as_string(""), "3000000000");
    assert_eq!(node.path("big").as_string(""), "99999999999999999999");
    assert_eq!(node.path("n").as_string("x"), "x");
    assert_eq!(node.path("f").as_double(0.0), 1.5);
    assert_eq!(node.path("s").float_value(), 0.0);
    assert_eq!(parse(b"1e7").unwrap().as_string(""), "1.0E7");
}

/// Jackson 3.2.3 `asInt()` (which throws where this is `None`) and `isInt()` on a JVM, one value per line.
#[test]
fn try_as_int_and_int_follow_jackson_3_2_3() {
    for (json, as_int, is_int) in [
        ("7", Some(7), true),
        ("3000000000", None, false),
        ("99999999999999999999", None, false),
        ("2.9", Some(2), false),
        ("1e5", Some(100_000), false),
        ("-0", Some(0), true),
        ("-0.0", Some(0), false),
        ("true", None, false),
        ("null", Some(0), false),
        (r#""12""#, Some(12), false),
        (r#""+5""#, Some(5), false),
        (r#""1.5""#, None, false),
        (r#"" 7""#, None, false),
        (r#""""#, None, false),
        ("[1]", None, false),
        ("{}", None, false),
        ("1e400", None, false),
    ] {
        let node = parse(json.as_bytes()).unwrap();
        assert_eq!(node.try_as_int(), as_int, "asInt() of {json}");
        assert_eq!(node.int().is_some(), is_int, "isInt() of {json}");
    }
    assert_eq!(parse(b"{}").unwrap().path("absent").try_as_int(), Some(0));
}

/// What Jackson 3.2.3 writes for these trees, `writeValueAsBytes(readTree(input))` on a JVM:
/// a repeated member replaced in place, `/` and non-ASCII as they are, `\u00XX` in upper case, doubles as
/// `Double.toString`, a non-finite one as a string (`1e999` reads as infinity).
#[test]
fn written_as_jackson_3_2_3_writes_it() {
    let cases: &[(&str, &str)] = &[
        (
            "{\"a\":\"x/y\\u0001\\u001f\\n\\t\\u00e9\\u2028<>&'\",\"b\":1.0,\"c\":1e2,\"d\":12345678901,\
             \"e\":123456789012345678901234,\"f\":-0.0,\"g\":1.5E-7,\"h\":0.1, \"i\": true, \"j\": null}",
            "{\"a\":\"x/y\\u0001\\u001F\\n\\t\u{e9}\u{2028}<>&'\",\"b\":1.0,\"c\":100.0,\"d\":12345678901,\
             \"e\":123456789012345678901234,\"f\":-0.0,\"g\":1.5E-7,\"h\":0.1,\"i\":true,\"j\":null}",
        ),
        (
            r#"{"p":1e999,"n":-1e999}"#,
            r#"{"p":"Infinity","n":"-Infinity"}"#,
        ),
        ("{\"a\": 1, \"b\": [], \"a\": 5}", "{\"a\":5,\"b\":[]}"),
        (
            r#"{"animation":{"frametime":2,"frames":[0,{"time":3}]}}"#,
            r#"{"animation":{"frametime":2,"frames":[0,{"time":3}]}}"#,
        ),
    ];
    for (input, expected) in cases {
        let written = write(&parse(input.as_bytes()).unwrap());
        assert_eq!(String::from_utf8(written).unwrap(), *expected, "{input}");
    }
}

/// A large object (a language file) looks repeated names up by name: each still replaces its member in place, before
/// and after the object grew large, in either grammar.
#[test]
fn a_repeated_name_in_a_large_object_replaces_its_member_in_place() {
    let mut input = String::from("{");
    for i in 0..40 {
        write!(input, "\"k{i}\": {i}, ").unwrap();
    }
    input.push_str("\"k0\": \"first\", \"k39\": \"late\", \"new\": 1, \"new\": 2}");
    let mut expected = String::from("{");
    for i in 0..40 {
        match i {
            0 => expected.push_str("\"k0\":\"first\","),
            39 => expected.push_str("\"k39\":\"late\","),
            _ => write!(expected, "\"k{i}\":{i},").unwrap(),
        }
    }
    expected.push_str("\"new\":2}");
    for node in [
        parse(input.as_bytes()).unwrap(),
        parse_lenient(input.as_bytes()).unwrap(),
    ] {
        assert_eq!(String::from_utf8(write(&node)).unwrap(), expected);
    }
}

#[test]
fn the_writer_separates_values_and_members_as_a_generator() {
    let mut out = Vec::new();
    let mut writer = Writer::new(&mut out);
    writer.start_object();
    writer.name("n");
    writer.float(0.1);
    writer.name("x");
    writer.float(f32::NAN);
    writer.name("l");
    writer.start_array();
    writer.start_object();
    writer.end_object();
    writer.null();
    writer.end_array();
    writer.end_object();
    assert_eq!(
        String::from_utf8(out).unwrap(),
        r#"{"n":0.1,"x":"NaN","l":[{},null]}"#
    );
}
