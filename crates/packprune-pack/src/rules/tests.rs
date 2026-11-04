use super::*;

#[test]
fn rules_of_26_2_are_client_rules_json() {
    let rules = ClientRules::for_version("26.2");
    assert_eq!(rules.version, "26.2");
    assert_eq!(rules.warning, None);
    assert_eq!(rules.missing_sprite, "minecraft:missingno");
    assert_eq!(rules.builtin_model_prefix, "builtin/");
    assert_eq!(rules.model_atlases, ["minecraft:blocks", "minecraft:items"]);
    assert_eq!(rules.particle_atlases, ["minecraft:particles"]);
    assert_eq!(
        rules.waypoint_sprites,
        SpriteRule {
            atlases: vec!["minecraft:gui".into()],
            prefix: "hud/locator_bar_dot/".into(),
            suffix: String::new(),
        }
    );
    assert_eq!(
        rules.equipment_textures.file("x", "humanoid/iron"),
        "assets/x/textures/entity/equipment/humanoid/iron.png"
    );
    assert_eq!(
        rules.shader_includes.file("x", "a.glsl"),
        "assets/x/shaders/include/a.glsl"
    );
    assert_eq!(rules.tooltip_sprites.len(), 2);
    assert_eq!(rules.tooltip_sprites[1].of("x"), "tooltip/x_frame");
    assert_eq!(
        rules.tooltip_sprites[1].unwrap("tooltip/x_frame"),
        Some("x")
    );
    assert_eq!(
        rules.tooltip_sprites[1].unwrap("tooltip/x_background"),
        None
    );
    assert_eq!(
        rules.special_textures.keys().collect::<Vec<_>>(),
        [
            "minecraft:chest",
            "minecraft:copper_golem_statue",
            "minecraft:head",
            "minecraft:shulker_box"
        ]
    );
    let TextureRule::File(statue) = &rules.special_textures["minecraft:copper_golem_statue"] else {
        panic!("a file rule");
    };
    assert_eq!(
        statue.file("minecraft", "textures/entity/golem.png"),
        "assets/minecraft/textures/entity/golem.png"
    );
    assert!(matches!(
        rules.special_textures["minecraft:chest"],
        TextureRule::Sprite(_)
    ));
}

#[test]
fn a_version_without_rules_gets_the_newest_with_a_warning() {
    let rules = ClientRules::for_version("1.21.4");
    assert_eq!(rules.version, "26.2");
    assert_eq!(
        rules.warning.as_deref(),
        Some("client version 1.21.4 has no rules; using the rules of 26.2")
    );
    assert_eq!(
        rules.missing_sprite,
        ClientRules::for_version("26.2").missing_sprite
    );
}

#[test]
fn versions_order_their_numbers_as_numbers() {
    assert_eq!(version_order("26.10", "26.2"), Ordering::Greater);
    assert_eq!(version_order("26.2", "26.2.1"), Ordering::Less);
    assert_eq!(version_order("26.02", "26.2"), Ordering::Equal);
    assert_eq!(version_order("26.2-pre1", "26.2-rc1"), Ordering::Less);
    assert_eq!(version_order("1.21.10", "1.21.9"), Ordering::Greater);
}

/// Every non-comment line of `vanilla-code-loaded.txt` is a code-loaded pattern.
#[test]
fn code_loaded_patterns_are_the_data_file() {
    let rules = ClientRules::for_version("26.2");
    let lines = CODE_LOADED
        .lines()
        .filter(|line| !line.trim().is_empty() && !line.trim().starts_with('#'))
        .count();
    assert_eq!(rules.code_loaded.len(), lines);
    assert_eq!(
        rules.code_loaded[0].pattern,
        "assets/minecraft/textures/entity/**"
    );
    assert!(rules.code_loaded[0].matches("assets/minecraft/textures/entity/chest/normal.png"));
    assert!(!rules.code_loaded[0].matches("assets/minecraft/textures/block/stone.png"));
}

#[test]
fn path_globs_keep_star_within_a_folder() {
    let glob = Glob::path;
    assert!(glob("a/*.png").matches("a/b.png"));
    assert!(!glob("a/*.png").matches("a/b/c.png"));
    assert!(glob("a/**.png").matches("a/b/c.png"));
    assert!(glob("a/**").matches("a/"));
    assert!(glob("a/?.png").matches("a/b.png"));
    assert!(!glob("a/?.png").matches("a//.png"));
    assert!(glob("a/b.png").matches("a/b.png"));
    assert!(!glob("a/b.png").matches("a/bxpng"));
    assert!(glob("*").matches(""));
    assert!(!glob("a").matches("ab"));
}
