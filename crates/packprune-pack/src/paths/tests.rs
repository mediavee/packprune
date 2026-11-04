use super::*;

#[test]
fn refuses_every_unsafe_path() {
    for path in [
        "",
        "/",
        "/absolute",
        "a\\b",
        "a\0b",
        "C:file",
        "c:/file",
        "../escape",
        "a/../escape",
        "a/./b",
        "a//b",
        "a/",
        ".",
    ] {
        assert_eq!(PhysPath::new(path), Err(PathError), "{path:?}");
        assert_eq!(LogicalPath::new(path), Err(PathError), "{path:?}");
    }
}

#[test]
fn accepts_paths_that_only_look_unsafe() {
    for path in [
        "pack.mcmeta",
        "assets/minecraft/textures/..png",
        "assets/a/b.c/d",
        "été/ü.json",
        "assets/C:file",
        "1:x",
        "ab:c",
        ".hidden",
    ] {
        assert_eq!(PhysPath::new(path).unwrap().as_str(), path);
    }
}
