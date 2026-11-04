use rayon::iter::{IntoParallelRefIterator, ParallelIterator};

use super::*;

fn parse(value: &str) -> Option<String> {
    let interner = Interner::default();
    ResourceId::parse(&interner, value).map(|id| id.display(&interner).to_string())
}

#[test]
fn interning_is_idempotent_and_resolves_across_chunk_boundaries() {
    let interner = Interner::default();
    let texts: Vec<String> = (0..5_000).map(|n| format!("path/{n}")).collect();
    let syms: Vec<Sym> = texts.iter().map(|text| interner.intern(text)).collect();
    for (text, sym) in texts.iter().zip(&syms) {
        assert_eq!(interner.resolve(*sym), text);
        assert_eq!(interner.intern(text), *sym);
    }
    let mut distinct = syms;
    distinct.sort();
    distinct.dedup();
    assert_eq!(distinct.len(), texts.len());
    assert_eq!(interner.resolve(interner.intern("")), "");
}

#[test]
fn threads_interning_the_same_texts_agree_on_every_sym() {
    let interner = Interner::default();
    let texts: Vec<String> = (0..2_000).map(|n| format!("ns:{}", n % 700)).collect();
    let syms: Vec<Sym> = texts.par_iter().map(|text| interner.intern(text)).collect();
    for (text, sym) in texts.iter().zip(&syms) {
        assert_eq!(interner.resolve(*sym), text);
        assert_eq!(interner.intern(text), *sym);
    }
    let mut distinct = syms;
    distinct.sort();
    distinct.dedup();
    assert_eq!(distinct.len(), 700);
}

#[test]
#[should_panic(expected = "was not issued by this interner")]
fn resolving_a_sym_of_another_interner_panics() {
    Interner::default().resolve(Sym(7));
}

#[test]
fn parse_defaults_the_namespace_and_refuses_bad_characters() {
    for (value, expected) in [
        ("stone", Some("minecraft:stone")),
        ("block/stone", Some("minecraft:block/stone")),
        ("fx:custom/a", Some("fx:custom/a")),
        (":stone", Some("minecraft:stone")),
        ("a.b-c_d9:e/f.g-h_i", Some("a.b-c_d9:e/f.g-h_i")),
        ("", Some("minecraft:")),
        ("fx:", Some("fx:")),
        (":", Some("minecraft:")),
        ("Stone", None),
        ("fx:Stone", None),
        ("Fx:stone", None),
        ("a:b:c", None),
        ("::", None),
        ("a b", None),
        ("a/b:c", None),
        ("fx:a\\b", None),
        ("fx:a:", None),
        ("é", None),
        ("fx:é", None),
        ("fx:a\n", None),
        ("fx:a\0", None),
    ] {
        assert_eq!(parse(value).as_deref(), expected, "{value:?}");
    }
}

#[test]
fn ids_of_one_interner_compare_by_value() {
    let interner = Interner::default();
    let short = ResourceId::parse(&interner, "stone").unwrap();
    assert_eq!(
        short,
        ResourceId::parse(&interner, "minecraft:stone").unwrap()
    );
    assert_eq!(short, ResourceId::parse(&interner, ":stone").unwrap());
    assert_ne!(short, ResourceId::parse(&interner, "fx:stone").unwrap());
}
