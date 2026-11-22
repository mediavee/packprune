//! font-trim runs through the harness, whose strict oracle must find every glyph the same; font-downscale runs on its
//! options and reports its measured loss.

use packprune_codec::png::Image;
use packprune_pack::PhysPath;
use packprune_pack::json::{self, Node};
use packprune_pass::{
    Edits, Enabled, Loss, Pass, PassCx, PassError, Proof, RunError, Scope, Stage, prove,
};

use super::downscale::{FontDownscale, resample};
use super::trim::FontTrim;
use crate::textures::no_options;
use crate::textures::tests::support::{FONT, Files, Fixture, decode, pack, png, put, test_pack};

const SHEET: &str = "assets/fx/textures/font/glyphs.png";
const OTHER: &str = "assets/fx/textures/font/other.png";
const WHITE: u32 = 0xFFFF_FFFF;

/// A sheet of `cols` by `rows` cells of `cell_w` by `cell_h`; `argb(col, row, x, y)` is the texel at `(x, y)` of
/// cell `(col, row)`.
fn sheet(
    (cols, rows): (u32, u32),
    (cell_w, cell_h): (u32, u32),
    argb: impl Fn(u32, u32, u32, u32) -> u32,
) -> Vec<u8> {
    png(cols * cell_w, rows * cell_h, |x, y| {
        argb(x / cell_w, y / cell_h, x % cell_w, y % cell_h)
    })
}

/// [`test_pack`] with `providers` as the font and `sheet` as `fx:font/glyphs.png`.
fn with_font(providers: &str, sheet: Vec<u8>) -> Files {
    let mut files = test_pack();
    put(
        &mut files,
        FONT,
        &format!(r#"{{"providers": [{providers}]}}"#),
    );
    files.insert(SHEET.into(), sheet);
    files
}

fn bitmap(file: &str, extra: &str, chars: &str) -> String {
    format!(r#"{{"type": "bitmap", "file": "{file}", {extra}"chars": {chars}}}"#)
}

fn tree(files: &Files, path: &str) -> Node {
    json::parse(&files[path]).unwrap()
}

fn provider(files: &Files, index: usize) -> Node {
    tree(files, FONT).path("providers").items()[index].clone()
}

fn size(files: &Files, path: &str) -> (u32, u32) {
    let image = decode(&files[path]);
    (image.width, image.height)
}

fn chars(node: &Node) -> Vec<String> {
    node.path("chars")
        .items()
        .iter()
        .map(|row| row.as_str().unwrap().to_owned())
        .collect()
}

// font-trim: rows and columns of \0

#[test]
fn rows_and_columns_of_nul_go_and_every_glyph_stays_the_same() {
    let fx = Fixture::new();
    // A and B fill their cells; the \0 cells hold texels no glyph draws.
    let files = with_font(
        &bitmap(
            "fx:font/glyphs.png",
            r#""ascent": 7, "#,
            r#"["AB\u0000\u0000", "\u0000\u0000\u0000\u0000"]"#,
        ),
        sheet((4, 2), (8, 8), |col, row, _, _| match (col, row) {
            (0, 0) => WHITE,
            (1, 0) => 0xFF00_FF00,
            _ => 0xFF12_3456,
        }),
    );

    let ran = fx.same(&files, &[], &FontTrim);

    assert_eq!(size(&ran.out, SHEET), (16, 8));
    assert_eq!(chars(&provider(&ran.out, 0)), ["AB"]);
    assert_eq!(
        ran.changes(),
        [
            &format!(
                "{SHEET}: 32x16 to 16x8 (cells 8x8 to 8x8; 1 row and 2 columns of \\0 removed)"
            ),
            "1 sheet trimmed, 384 texels fewer"
        ]
    );
}

#[test]
fn chars_are_written_back_as_valid_json_with_their_codepoints() {
    let fx = Fixture::new();
    let files = with_font(
        &bitmap(
            "fx:font/glyphs.png",
            r#""ascent": 7, "#,
            r#"["\u0000", "\u0000\u0000\u0000", "a\u0000\""]"#,
        ),
        sheet((3, 3), (4, 4), |col, row, _, _| {
            if (col, row) == (1, 0) || row == 1 {
                0
            } else {
                WHITE
            }
        }),
    );

    let ran = fx.same(&files, &[], &FontTrim);

    let written = &ran.out[FONT];
    assert!(std::str::from_utf8(written).is_ok());
    assert!(json::parse_lenient(written).is_ok());
    assert_eq!(
        chars(&provider(&ran.out, 0)),
        ["\u{E000}\u{E001}", "a\""],
        "column 1 and row 1 were only \\0"
    );
    assert_eq!(size(&ran.out, SHEET), (8, 8));
}

// font-trim: transparent margins

#[test]
fn the_right_margin_goes_and_the_advances_stay() {
    let fx = Fixture::new();
    let files = with_font(
        &bitmap("fx:font/glyphs.png", r#""ascent": 7, "#, r#"["AB"]"#),
        sheet((2, 1), (8, 8), |col, _, x, _| {
            if x < [3, 5][col as usize] { WHITE } else { 0 }
        }),
    );

    let ran = fx.same(&files, &[], &FontTrim);

    assert_eq!(size(&ran.out, SHEET), (10, 8));
    assert_eq!(ran.out[FONT], files[FONT], "height and ascent stay");
}

#[test]
fn a_transparent_texel_with_a_colour_is_never_cut() {
    let fx = Fixture::new();
    // Alpha 0 with red under it at x = 6: a shader could read it, so the margin ends after it.
    let files = with_font(
        &bitmap("fx:font/glyphs.png", r#""ascent": 7, "#, r#"["A"]"#),
        sheet((1, 1), (8, 8), |_, _, x, _| match x {
            0..3 => WHITE,
            6 => 0x00FF_0000,
            _ => 0,
        }),
    );

    let ran = fx.same(&files, &[], &FontTrim);

    assert_eq!(size(&ran.out, SHEET), (7, 8));
    let image = decode(&ran.out[SHEET]);
    assert_eq!(&image.rgba[6 * 4..7 * 4], &[0xFF, 0, 0, 0]);
}

#[test]
fn top_and_bottom_margins_go_with_height_and_ascent_moved() {
    let fx = Fixture::new();
    let mut files = with_font(
        "",
        sheet((1, 1), (8, 8), |_, _, x, y| {
            if x < 4 && (2..6).contains(&y) {
                WHITE
            } else {
                0
            }
        }),
    );
    put(
        &mut files,
        FONT,
        "{\"providers\": [\n  {\"type\": \"bitmap\", \"file\": \"fx:font/glyphs.png\", \"ascent\": 6, \"chars\": [\"A\"]}\n]}",
    );

    let ran = fx.same(&files, &[], &FontTrim);

    assert_eq!(size(&ran.out, SHEET), (4, 4));
    assert_eq!(
        ran.text(FONT),
        "{\"providers\": [\n  {\"type\": \"bitmap\", \"file\": \"fx:font/glyphs.png\", \"ascent\": 4, \"chars\": [\"A\"],\"height\":4}\n]}",
        "every other byte stays"
    );
    assert_eq!(
        ran.changes()[0],
        format!("{SHEET}: 8x8 to 4x4 (cells 8x8 to 4x4; height 8 to 4, ascent 6 to 4)")
    );
}

#[test]
fn a_cut_keeps_the_scale_exact_and_the_height_an_integer() {
    let fx = Fixture::new();
    // Half a GUI pixel per texel: a cut keeps an even cell height and moves the top by whole GUI pixels, so of the
    // 3 free rows on each side only 2 go.
    let files = with_font(
        &bitmap(
            "fx:font/glyphs.png",
            r#""height": 4, "ascent": 2, "#,
            r#"["A"]"#,
        ),
        sheet((1, 1), (8, 8), |_, _, x, y| {
            if x < 2 && (3..5).contains(&y) {
                WHITE
            } else {
                0
            }
        }),
    );

    let ran = fx.same(&files, &[], &FontTrim);

    assert_eq!(size(&ran.out, SHEET), (2, 4));
    let provider = provider(&ran.out, 0);
    assert_eq!(provider.path("height").int(), Some(2));
    assert_eq!(provider.path("ascent").int(), Some(1));
}

#[test]
fn a_two_thirds_scale_cuts_only_where_every_texel_keeps_its_rectangle() {
    let fx = Fixture::new();
    // 12 texels for 8 GUI pixels: whatever the cut, the oracle must find the glyph the same.
    let files = with_font(
        &bitmap("fx:font/glyphs.png", r#""ascent": 7, "#, r#"["A"]"#),
        sheet((1, 1), (12, 12), |_, _, x, y| {
            if x < 5 && (4..8).contains(&y) {
                WHITE
            } else {
                0
            }
        }),
    );

    let ran = fx.same(&files, &[], &FontTrim);

    let provider = provider(&ran.out, 0);
    let height = provider.path("height").as_int(8);
    let (_, cell_h) = size(&ran.out, SHEET);
    assert_eq!(
        (height as f32 / cell_h as f32).to_bits(),
        (8.0f32 / 12.0).to_bits()
    );
    assert!(provider.path("ascent").as_int(0) <= height);
}

#[test]
fn ascent_never_goes_above_height() {
    let fx = Fixture::new();
    // The glyph fills the top 3 rows: 5 free rows below, but height may not drop below the ascent of 7.
    let files = with_font(
        &bitmap("fx:font/glyphs.png", r#""ascent": 7, "#, r#"["A"]"#),
        sheet(
            (1, 1),
            (8, 8),
            |_, _, x, y| if x < 8 && y < 3 { WHITE } else { 0 },
        ),
    );

    let ran = fx.same(&files, &[], &FontTrim);

    assert_eq!(size(&ran.out, SHEET), (8, 7));
    let provider = provider(&ran.out, 0);
    assert_eq!(provider.path("height").int(), Some(7));
    assert_eq!(provider.path("ascent").int(), Some(7));
}

#[test]
fn a_tight_sheet_is_left_as_it_is() {
    let fx = Fixture::new();
    let files = with_font(
        &bitmap("fx:font/glyphs.png", r#""ascent": 7, "#, r#"["AB"]"#),
        sheet((2, 1), (8, 8), |_, _, _, _| WHITE),
    );

    let ran = fx.same(&files, &[], &FontTrim);

    assert_eq!(ran.changed(), 0);
    assert_eq!(ran.changes(), ["kept 1 sheet: no texel to trim"]);
}

// font-trim: providers no glyph comes from

#[test]
fn a_provider_of_only_nul_goes_and_its_sheet_with_it() {
    let fx = Fixture::new();
    let mut files = with_font(
        &[
            r#"{"type": "space", "advances": {" ": 4.0}}"#.to_owned(),
            bitmap("fx:font/glyphs.png", r#""ascent": 7, "#, r#"["AB"]"#),
            bitmap(
                "fx:font/other.png",
                r#""ascent": 7, "#,
                r#"["\u0000\u0000"]"#,
            ),
        ]
        .join(", "),
        sheet((2, 1), (8, 8), |_, _, _, _| WHITE),
    );
    files.insert(OTHER.into(), sheet((2, 1), (8, 8), |_, _, _, _| WHITE));

    let ran = fx.same(&files, &[], &FontTrim);

    assert_eq!(tree(&ran.out, FONT).path("providers").size(), 2);
    assert!(!ran.out.contains_key(OTHER), "the re-prune drops the sheet");
    assert_eq!(ran.out[SHEET], files[SHEET]);
    assert_eq!(
        ran.changes(),
        [
            &format!("{FONT}: provider 2 removed: its grid is only \\0"),
            "1 provider removed",
            "kept 1 sheet: no texel to trim"
        ]
    );
}

#[test]
fn a_provider_whose_every_glyph_an_earlier_one_draws_goes() {
    let fx = Fixture::new();
    let mut files = with_font(
        &[
            bitmap("fx:font/glyphs.png", r#""ascent": 7, "#, r#"["AB"]"#),
            r#"{"type": "space", "advances": {"C": 5}}"#.to_owned(),
            bitmap("fx:font/other.png", r#""ascent": 7, "#, r#"["AC"]"#),
        ]
        .join(", "),
        sheet((2, 1), (8, 8), |_, _, _, _| WHITE),
    );
    files.insert(
        OTHER.into(),
        sheet((2, 1), (8, 8), |_, _, _, _| 0xFF00_00FF),
    );

    let ran = fx.same(&files, &[], &FontTrim);

    assert_eq!(
        ran.text(FONT),
        format!(
            r#"{{"providers": [{},{{"type": "space", "advances": {{"C": 5}}}}]}}"#,
            bitmap("fx:font/glyphs.png", r#""ascent": 7, "#, r#"["AB"]"#)
        )
    );
    assert!(!ran.out.contains_key(OTHER));
}

#[test]
fn a_provider_behind_a_filter_another_type_or_a_wide_glyph_stays() {
    let fx = Fixture::new();
    let shadowed = bitmap("fx:font/other.png", r#""ascent": 7, "#, r#"["A"]"#);
    let cases = [
        // The earlier provider is not always active.
        format!(
            r#"{}, {shadowed}"#,
            bitmap(
                "fx:font/glyphs.png",
                r#""ascent": 7, "filter": {"uniform": true}, "#,
                r#"["A"]"#
            )
        ),
        // A provider of another type between them.
        format!(
            r#"{}, {{"type": "ttf", "file": "fx:hud.ttf"}}, {shadowed}"#,
            bitmap("fx:font/glyphs.png", r#""ascent": 7, "#, r#"["A"]"#)
        ),
        // The later provider is not always active.
        format!(
            r#"{}, {}"#,
            bitmap("fx:font/glyphs.png", r#""ascent": 7, "#, r#"["A"]"#),
            bitmap(
                "fx:font/other.png",
                r#""ascent": 7, "filter": {"jp": false}, "#,
                r#"["A"]"#
            )
        ),
        // The earlier glyph's advance is past 32.
        format!(r#"{{"type": "space", "advances": {{"A": 40}}}}, {shadowed}"#),
    ];
    for providers in cases {
        let mut files = with_font(&providers, sheet((1, 1), (8, 8), |_, _, _, _| WHITE));
        files.insert(OTHER.into(), sheet((1, 1), (8, 8), |_, _, _, _| WHITE));

        let ran = fx.same(&files, &[], &FontTrim);

        assert_eq!(ran.out[FONT], files[FONT], "{providers}");
        assert!(ran.out.contains_key(OTHER), "{providers}");
    }
}

#[test]
fn a_font_a_reference_provider_inlines_keeps_its_shadowed_providers() {
    let fx = Fixture::new();
    let mut files = with_font(
        &[
            bitmap("fx:font/glyphs.png", r#""ascent": 7, "#, r#"["A"]"#),
            bitmap("fx:font/other.png", r#""ascent": 7, "#, r#"["A"]"#),
        ]
        .join(", "),
        sheet((1, 1), (8, 8), |_, _, _, _| WHITE),
    );
    files.insert(OTHER.into(), sheet((1, 1), (8, 8), |_, _, _, _| WHITE));
    // Under the reference's filter, the oracle reads every provider of the font that draws a glyph.
    put(
        &mut files,
        "assets/fx/font/outer.json",
        r#"{"providers": [{"type": "reference", "id": "fx:hud", "filter": {"uniform": true}}]}"#,
    );

    let ran = fx.same(&files, &[], &FontTrim);

    assert_eq!(ran.out[FONT], files[FONT]);
}

#[test]
fn the_last_provider_of_a_font_stays() {
    let fx = Fixture::new();
    let files = with_font(
        &bitmap("fx:font/glyphs.png", r#""ascent": 7, "#, r#"["\u0000"]"#),
        sheet((1, 1), (8, 8), |_, _, _, _| 0),
    );

    let ran = fx.same(&files, &[], &FontTrim);

    assert_eq!(tree(&ran.out, FONT).path("providers").size(), 1);
}

// font-trim: sheets something else reads

#[test]
fn a_sheet_two_providers_load_stays() {
    let fx = Fixture::new();
    let mut files = with_font(
        &bitmap("fx:font/glyphs.png", r#""ascent": 7, "#, r#"["A"]"#),
        sheet((1, 1), (8, 8), |_, _, x, _| if x < 3 { WHITE } else { 0 }),
    );
    put(
        &mut files,
        "assets/fx/font/other.json",
        &format!(
            r#"{{"providers": [{}]}}"#,
            bitmap("fx:font/glyphs.png", r#""ascent": 7, "#, r#"["B"]"#)
        ),
    );

    let ran = fx.same(&files, &[], &FontTrim);

    assert_eq!(ran.out[SHEET], files[SHEET]);
    assert_eq!(
        ran.changes(),
        ["kept 2 sheets: another provider or file loads the sheet"]
    );
}

#[test]
fn a_sheet_a_source_names_or_an_atlas_stitches_stays() {
    let fx = Fixture::new();
    let files = with_font(
        &bitmap("fx:font/glyphs.png", r#""ascent": 7, "#, r#"["A"]"#),
        sheet((1, 1), (8, 8), |_, _, x, _| if x < 3 { WHITE } else { 0 }),
    );

    let named = fx.same(&files, &[SHEET], &FontTrim);

    assert_eq!(named.out[SHEET], files[SHEET]);

    let mut stitched = files.clone();
    put(
        &mut stitched,
        "assets/minecraft/atlases/blocks.json",
        r#"{"sources": [{"type": "directory", "source": "font", "prefix": "font/"}]}"#,
    );

    let ran = fx.same(&stitched, &[], &FontTrim);

    assert_eq!(ran.out[SHEET], files[SHEET]);
    assert_eq!(ran.changes(), ["kept 1 sheet: an atlas stitches the sheet"]);
}

#[test]
fn a_pack_with_a_text_shader_is_left_as_it_is() {
    let fx = Fixture::new();
    let mut files = with_font(
        &[
            bitmap("fx:font/glyphs.png", r#""ascent": 7, "#, r#"["A\u0000"]"#),
            bitmap("fx:font/other.png", r#""ascent": 7, "#, r#"["\u0000"]"#),
        ]
        .join(", "),
        sheet((2, 1), (8, 8), |_, _, x, _| if x < 3 { WHITE } else { 0 }),
    );
    files.insert(OTHER.into(), sheet((1, 1), (8, 8), |_, _, _, _| 0));
    put(
        &mut files,
        "assets/minecraft/shaders/core/rendertype_text.fsh",
        "#version 150\nvoid main() {}\n",
    );

    let ran = fx.same(&files, &[], &FontTrim);

    assert_eq!(ran.changed(), 0);
    assert_eq!(
        ran.changes(),
        ["skipped: the pack replaces a text core shader, which may read any texel of a sheet"]
    );
}

// font-trim: the sheet as the oracle reads it

#[test]
fn trimming_keeps_every_glyph_of_a_mixed_sheet() {
    let fx = Fixture::new();
    // Glyphs of different widths and heights, one empty, texels of every alpha: the strict oracle decides.
    let files = with_font(
        &bitmap(
            "fx:font/glyphs.png",
            r#""height": 9, "ascent": 8, "#,
            r#"["AB\u0000C", "D E\u0000"]"#,
        ),
        sheet((4, 2), (9, 9), |col, row, x, y| {
            let shade = 0x1000_0000 * (1 + col + 4 * row) + x * 0x0101 + y * 0x01_0000;
            match (col, row) {
                (0, 0) if x < 4 && (1..7).contains(&y) => shade,
                (1, 0) if x < 6 && (2..8).contains(&y) => shade,
                (3, 0) if x == 0 && y == 4 => shade,
                (0, 1) if x < 2 && (3..6).contains(&y) => shade,
                (2, 1) if x < 7 && (x + y) % 2 == 0 && (1..8).contains(&y) => shade,
                _ => 0,
            }
        }),
    );

    let ran = fx.same(&files, &[], &FontTrim);

    let (width, height) = size(&ran.out, SHEET);
    assert!(width < 36 && height <= 18, "{width}x{height}");
}

// The oracle the proofs above rely on

/// Writes the given files as they are: a trim done wrong.
struct Replace(Vec<(&'static str, Vec<u8>)>);

impl Pass for Replace {
    fn name(&self) -> &'static str {
        "replace"
    }

    fn stage(&self) -> Stage {
        Stage::Fonts
    }

    fn loss(&self) -> Loss {
        Loss::Lossless
    }

    fn options_schema(&self) -> schemars::Schema {
        no_options()
    }

    fn run(&self, _: &PassCx<'_>) -> Result<Edits, PassError> {
        let mut edits = Edits::default();
        for (path, bytes) in &self.0 {
            edits
                .replaced
                .insert(PhysPath::new(path).unwrap(), bytes.clone().into());
        }
        Ok(edits)
    }
}

#[test]
fn the_oracle_refuses_a_glyph_moved_or_a_coloured_texel_cut() {
    let fx = Fixture::new();
    let files = with_font(
        &bitmap("fx:font/glyphs.png", r#""ascent": 6, "#, r#"["A"]"#),
        sheet((1, 1), (8, 8), |_, _, x, y| match (x, y) {
            (0..=3, 2..=5) => WHITE,
            (5, 3) => 0x00FF_0000,
            _ => 0,
        }),
    );
    let cropped = |width: u32, height: u32| {
        png(width, height, |x, y| match (x, y) {
            (0..=3, 0..=3) => WHITE,
            (5, 1) => 0x00FF_0000,
            _ => 0,
        })
    };
    let moved = format!(
        r#"{{"providers": [{}]}}"#,
        bitmap(
            "fx:font/glyphs.png",
            r#""height": 4, "ascent": 4, "#,
            r#"["A"]"#
        )
    );
    let cases = [
        // The rows go, but `ascent` and `height` stay: the glyph is drawn twice as tall, two pixels higher.
        Replace(vec![(SHEET, cropped(6, 4))]),
        // `ascent` and `height` move, but the transparent red texel at x = 5 goes with the margin.
        Replace(vec![
            (SHEET, cropped(5, 4)),
            (FONT, moved.clone().into_bytes()),
        ]),
    ];
    for case in &cases {
        let refused = fx.prove(&files, &[], &[case], Scope::all());

        assert!(
            matches!(refused, Err(RunError::NotEquivalent { .. })),
            "{refused:?}"
        );
    }
    let right = Replace(vec![(SHEET, cropped(6, 4)), (FONT, moved.into_bytes())]);

    fx.prove(&files, &[], &[&right], Scope::all()).unwrap();

    let ran = fx.same(&files, &[], &FontTrim);

    assert_eq!(ran.out[SHEET], cropped(6, 4), "what font-trim writes");
}

// font-downscale

fn downscale(fx: &Fixture, files: &Files, options: &str) -> Result<Proof, RunError> {
    let pass = FontDownscale;
    prove(
        &fx.cx(&[]),
        &fx.client,
        pack(files),
        vec![Enabled {
            pass: &pass,
            scope: Scope::all(),
            options: toml::from_str(options).unwrap(),
        }],
    )
}

/// `height` 8 drawn from 32-texel cells: 4 texels per GUI pixel.
fn oversampled(argb: impl Fn(u32, u32, u32, u32) -> u32) -> Files {
    with_font(
        &bitmap("fx:font/glyphs.png", r#""ascent": 7, "#, r#"["AB"]"#),
        sheet((2, 1), (32, 32), argb),
    )
}

#[test]
fn an_oversampled_sheet_is_downscaled_and_its_loss_measured() {
    let fx = Fixture::new();
    let files = oversampled(|col, _, x, _| if x < [16, 8][col as usize] { WHITE } else { 0 });

    let proof = downscale(&fx, &files, "").unwrap();

    let out = crate::textures::tests::support::files_of(&proof.output.pack);
    assert_eq!(size(&out, SHEET), (32, 16));
    assert_eq!(
        out[FONT], files[FONT],
        "height stays: the glyphs keep their size"
    );
    let report = &proof.reports[0];
    assert_eq!(
        report
            .measured
            .iter()
            .map(|record| (record.key.as_str(), record.metric.as_str(), record.value))
            .collect::<Vec<_>>(),
        [(SHEET, "ssim", 1.0), (SHEET, "max_delta", 0.0)]
    );
    assert!(proof.loss.is_some());
    assert!(
        report.changes[0].starts_with(&format!(
            "{SHEET}: 64x32 to 32x16 (cells 32x32 to 16x16, height 8)"
        )),
        "{}",
        report.changes[0]
    );
}

#[test]
fn a_sheet_whose_advance_would_change_stays() {
    let fx = Fixture::new();
    // 21 texels at a quarter GUI pixel each give advance 6; half of output column 10 would give 7.
    let files = oversampled(|_, _, x, _| if x < 21 { WHITE } else { 0 });

    let proof = downscale(&fx, &files, "").unwrap();

    assert!(proof.loss.is_none());
    assert_eq!(
        proof.reports[0].changes,
        ["kept 1 sheet: a glyph's advance would change"]
    );
}

#[test]
fn a_sheet_over_its_loss_thresholds_stays() {
    let fx = Fixture::new();
    let files = oversampled(
        |_, _, x, y| {
            if x < 16 && (x + y) % 2 == 0 { WHITE } else { 0 }
        },
    );

    let kept = downscale(&fx, &files, "max_delta = 10").unwrap();

    assert!(kept.loss.is_none());
    assert_eq!(
        kept.reports[0].changes,
        ["kept 1 sheet: measured loss exceeds threshold"]
    );

    let shrunk = downscale(&fx, &files, "max_delta = 255").unwrap();

    assert!(shrunk.loss.is_some());
}

#[test]
fn a_sheet_within_max_oversample_stays() {
    let fx = Fixture::new();
    let files = oversampled(|_, _, x, _| if x < 16 { WHITE } else { 0 });

    let proof = downscale(&fx, &files, "max_oversample = 4.0").unwrap();

    assert!(proof.loss.is_none());
    assert_eq!(
        proof.reports[0].changes,
        ["kept 1 sheet: at most max_oversample texels per GUI pixel"]
    );
}

#[test]
fn a_sheet_another_font_loads_is_not_downscaled() {
    let fx = Fixture::new();
    let mut files = oversampled(|_, _, x, _| if x < 16 { WHITE } else { 0 });
    put(
        &mut files,
        "assets/fx/font/other.json",
        &format!(
            r#"{{"providers": [{}]}}"#,
            bitmap("fx:font/glyphs.png", r#""ascent": 7, "#, r#"["CD"]"#)
        ),
    );

    let proof = downscale(&fx, &files, "").unwrap();

    assert!(proof.loss.is_none());
    assert_eq!(
        proof.reports[0].changes,
        ["kept 2 sheets: another provider or file loads the sheet"]
    );
}

#[test]
fn options_are_checked() {
    let check = |text: &str| FontDownscale.validate_options(&toml::from_str(text).unwrap());
    assert!(check("").is_ok());
    assert!(check("max_oversample = 1.5\nmin_ssim = 0.9\nmax_delta = 8").is_ok());
    for bad in [
        "max_oversample = 0.0",
        "max_oversample = -1.0",
        "max_oversample = nan",
        "max_oversample = inf",
        "min_ssim = 2.0",
        "max_delta = 300",
        "ceiling = 4.0",
    ] {
        assert!(check(bad).is_err(), "{bad}");
    }
    assert!(
        FontTrim
            .validate_options(&toml::from_str("x = 1").unwrap())
            .is_err()
    );
}

fn texels(width: u32, height: u32, rgba: &[[u8; 4]]) -> Image {
    Image {
        width,
        height,
        rgba: rgba.concat(),
    }
}

#[test]
fn resampling_averages_light_weighted_by_alpha() {
    let cases = [
        // Linear light: black and white average to 50 % light, sRGB 188, not 128.
        ([[0, 0, 0, 255], [255, 255, 255, 255]], [188, 188, 188, 255]),
        // The colour under a transparent texel adds nothing; alpha halves.
        ([[255, 0, 0, 0], [0, 0, 255, 255]], [0, 0, 255, 128]),
        ([[255, 255, 255, 255], [0, 0, 0, 0]], [255, 255, 255, 128]),
        // Nothing opaque: fully transparent black.
        ([[9, 9, 9, 0], [200, 1, 1, 0]], [0, 0, 0, 0]),
    ];
    for (input, expected) in cases {
        let out = resample(
            &texels(2, 1, &input),
            &super::Grid {
                cols: 1,
                rows: 1,
                cell_w: 2,
                cell_h: 1,
            },
            1,
            1,
        );
        assert_eq!(out.rgba, expected, "{input:?}");
    }
}

#[test]
fn resampling_covers_fractional_texels_by_area() {
    // 3 texels to 2: each output texel covers one and a half, so the middle one counts half in each.
    let input = texels(
        3,
        1,
        &[[255, 255, 255, 255], [255, 255, 255, 255], [0, 0, 0, 0]],
    );
    let grid = super::Grid {
        cols: 1,
        rows: 1,
        cell_w: 3,
        cell_h: 1,
    };

    let out = resample(&input, &grid, 2, 1);

    assert_eq!(out.rgba, [255, 255, 255, 255, 255, 255, 255, 85]);
}

#[test]
fn resampling_keeps_each_cell_apart() {
    // Two cells side by side, one white and one transparent: no texel bleeds across the cell edge.
    let input = texels(4, 1, &[[255; 4], [255; 4], [0; 4], [0; 4]]);
    let grid = super::Grid {
        cols: 2,
        rows: 1,
        cell_w: 2,
        cell_h: 1,
    };

    let out = resample(&input, &grid, 1, 1);

    assert_eq!(out.rgba, [255, 255, 255, 255, 0, 0, 0, 0]);
}

#[test]
fn both_passes_run_together() {
    let fx = Fixture::new();
    let files = with_font(
        &bitmap("fx:font/glyphs.png", r#""ascent": 7, "#, r#"["AB\u0000"]"#),
        sheet(
            (3, 1),
            (32, 32),
            |col, _, x, _| {
                if col < 2 && x < 16 { WHITE } else { 0 }
            },
        ),
    );
    let passes: [&dyn Pass; 2] = [&FontTrim, &FontDownscale];

    let proof = fx.prove(&files, &[], &passes, Scope::all()).unwrap();

    let out = crate::textures::tests::support::files_of(&proof.output.pack);
    assert!(
        proof.proof.is_some(),
        "font-trim changed the pack and the oracle proved it"
    );
    assert_eq!(chars(&provider(&out, 0)), ["AB"]);
    let (width, height) = size(&out, SHEET);
    assert!(width <= 32 && height <= 32, "{width}x{height}");
}

#[test]
fn font_dedupe_merges_identical_font_sheets_even_with_text_shader_and_rgb_under_alpha0() {
    let fx = Fixture::new();
    let mut files = Files::new();
    put(
        &mut files,
        "pack.mcmeta",
        r#"{"pack": {"min_format": 88, "max_format": 88, "description": "t"}}"#,
    );
    // Presence of a custom text core shader must NOT block font-dedupe, and RGB under alpha=0 stays intact.
    put(
        &mut files,
        "assets/minecraft/shaders/core/rendertype_text.fsh",
        "void main() {}",
    );
    let glyph_png = png(16, 16, |x, y| {
        if (x, y) == (0, 0) {
            0x00FC_FEFF // shader marker with alpha = 0
        } else if x < 8 && y < 8 {
            WHITE
        } else {
            0
        }
    });
    files.insert("assets/fx/textures/font/a.png".into(), glyph_png.clone());
    files.insert("assets/fx/textures/font/b.png".into(), glyph_png);
    put(
        &mut files,
        FONT,
        r#"{"providers": [
            {"type": "bitmap", "file": "fx:font/a.png", "ascent": 7, "chars": ["A"]},
            {"type": "bitmap", "file": "fx:font/b.png", "ascent": 7, "chars": ["B"]}
        ]}"#,
    );

    let ran = fx.same(&files, &[], &super::dedupe::FontDedupe);
    assert!(ran.out.contains_key("assets/fx/textures/font/a.png"));
    assert!(!ran.out.contains_key("assets/fx/textures/font/b.png"));
    assert!(ran.text(FONT).contains("fx:font/a.png"));
    assert!(!ran.text(FONT).contains("fx:font/b.png"));
}

#[test]
fn font_pack_packs_single_glyph_sheets_even_with_text_shader_and_proves_equivalence() {
    let fx = Fixture::new();
    let mut files = Files::new();
    put(
        &mut files,
        "pack.mcmeta",
        r#"{"pack": {"min_format": 88, "max_format": 88, "description": "t"}}"#,
    );
    put(
        &mut files,
        "assets/minecraft/shaders/core/rendertype_text.fsh",
        "void main() {}",
    );
    let mut provs = Vec::new();
    for i in 0..4u32 {
        let path = format!("assets/fx/textures/font/g{i}.png");
        let ch = char::from_u32(0xE000 + i).unwrap();
        let img = png(16, 16, |x, y| {
            if (x, y) == (15, 15) {
                0x00FC_FEFF // RGB under alpha=0 shader marker preserved verbatim
            } else if x < 4 + i && y < 8 {
                WHITE
            } else {
                0
            }
        });
        files.insert(path, img);
        provs.push(format!(
            r#"{{"type": "bitmap", "file": "fx:font/g{i}.png", "height": 12, "ascent": 10, "chars": ["{ch}"]}}"#
        ));
    }
    put(
        &mut files,
        FONT,
        &format!(r#"{{"providers": [{}]}}"#, provs.join(",")),
    );

    let ran = fx.same(&files, &[], &super::pack::FontPack);
    for i in 0..4 {
        assert!(
            !ran.out
                .contains_key(&format!("assets/fx/textures/font/g{i}.png")),
            "individual sheet g{i} should be packed"
        );
    }
    let packed_key = ran
        .out
        .keys()
        .find(|k| k.starts_with("assets/packprune/textures/f/"))
        .expect("expected packed grid sheet");
    assert_eq!(size(&ran.out, packed_key), (32, 32));
}
