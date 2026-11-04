use super::JavaPattern;

/// Each case with `Pattern.compile(pattern).asPredicate().test(value)` as Java 25 answers it.
#[test]
fn finds_as_java_does() {
    let cases = [
        ("stone", "block/stone_bricks", true),
        ("^stone", "block/stone", false),
        ("^block/.*_(top|side)$", "block/log_side", true),
        ("^block/.*_(top|side)$", "block/log_bottom", false),
        (r"[a-c]+\d{2,3}\b", "x abc12 y", true),
        ("[^a-z]", "abc", false),
        ("a.c", "abc", true),
        ("a.c", "a\nc", false),
        ("a.c", "a\u{2028}c", false),
        ("x(?:ab)*?y", "xababy", true),
        (r"\Qa.b\E", "a.b", true),
        (r"\Qa.b\E", "axb", false),
        ("", "anything", true),
        ("a$", "a\n", true),
        ("a$", "a\r\n", true),
        ("a$", "a\n\n", false),
        (r"a\b", "a\u{e9}", true),
        (r"\w", "\u{e9}", false),
        (r"\d", "\u{663}", false),
        (r"[\d_]+$", "12_", true),
        (r"[^\s]", "  ", false),
        ("a{2}b", "aab", true),
        ("(a|b)c", "bc", true),
    ];
    for (pattern, value, java) in cases {
        let compiled = JavaPattern::new(pattern).unwrap();
        assert_eq!(compiled.find(value), java, "{pattern:?} on {value:?}");
    }
}

/// Java refuses the first three; the rest compile in Java but are outside the subset: refused with their text.
#[test]
fn a_pattern_outside_the_subset_is_refused_by_name() {
    for pattern in [
        "(", "[", "a{2", "(?i)a", "a(?=b)", r"(a)\1", "[a&&b]", "a++", r"\p{L}", "a$b",
    ] {
        let error = JavaPattern::new(pattern).unwrap_err();
        assert_eq!(error.pattern, pattern);
        assert!(
            error.to_string().contains(&format!("{pattern:?}")),
            "{error}"
        );
    }
}
