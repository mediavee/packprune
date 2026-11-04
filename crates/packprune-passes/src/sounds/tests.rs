use packprune_codec::audio;
use packprune_pass::{Enabled, Kept, Pass, Scope, run};

use super::{ogg::Ogg, stream::StreamSounds, transcode::Audio};
use crate::lossy::tests::{Files, Fixture, bytes, files, pack, physical, text};

const SOUNDS: &str = "assets/fx/sounds.json";

#[test]
fn options_validate_at_load() {
    for pass in super::passes() {
        assert!(pass.validate_options(&toml::Table::new()).is_ok());
        assert!(
            pass.validate_options(&"unknown = 1".parse().unwrap())
                .is_err()
        );
        let (valid, invalid): (&str, &[&str]) = if pass.name() == "stream-sounds" {
            (
                "seconds = 1",
                &["seconds = -1", "seconds = nan", "seconds = 'long'"],
            )
        } else {
            (
                "quality = 3\nmax_lsd = 2\nmin_snr = 10",
                &[
                    "quality = 11",
                    "quality = -3",
                    "quality = nan",
                    "cap = 0",
                    "max_lsd = -1",
                    "min_snr = nan",
                    "cap = 'high'",
                ],
            )
        };
        assert!(
            pass.validate_options(&valid.parse().unwrap()).is_ok(),
            "{}",
            pass.name()
        );
        if pass.name() == "sound-rate" {
            assert!(
                pass.validate_options(&"cap = 32000".parse().unwrap())
                    .is_ok()
            );
        } else {
            assert!(
                pass.validate_options(&"cap = 32000".parse().unwrap())
                    .is_err(),
                "{} must reject cap",
                pass.name()
            );
        }
        for invalid in invalid {
            assert!(
                pass.validate_options(&invalid.parse().unwrap()).is_err(),
                "{}: {invalid}",
                pass.name()
            );
        }
    }
}

const LONG: &[u8] = include_bytes!("../../../../tests/fixtures/sounds/stereo_44k_6s.ogg");
const SHORT: &[u8] = include_bytes!("../../../../tests/fixtures/sounds/mono_22k_1s.ogg");
const HIGH: &[u8] = include_bytes!("../../../../tests/fixtures/sounds/stereo_96k_1s.ogg");

fn sounds(json: &str) -> Files {
    let mut files = files();
    text(&mut files, SOUNDS, json);
    files.insert("assets/fx/sounds/long.ogg".into(), LONG.to_vec());
    files.insert("assets/fx/sounds/short.ogg".into(), SHORT.to_vec());
    files
}

#[test]
fn ogg_metadata_matches_1x_and_rejects_truncated_pages() {
    let stereo = Ogg::read(LONG).unwrap();
    assert_eq!(stereo.channels, 2);
    assert_eq!(stereo.rate, 44100);
    assert_eq!(stereo.seconds(), 6.0);
    let mono = Ogg::read(SHORT).unwrap();
    assert_eq!(mono.channels, 1);
    assert_eq!(mono.rate, 22050);
    assert_eq!(mono.seconds(), 1.0);
    for bytes in [&LONG[..LONG.len() - 1], b"not really vorbis", &[]] {
        assert!(Ogg::read(bytes).is_none());
    }
}

#[test]
fn stream_entry_forms_preserve_every_other_byte_and_report_1x_durations() {
    let files = sounds(
        r#"{"a": {"sounds": ["fx:long", "fx:short"]},
 "b": {"sounds": [{"name": "fx:long", "volume": 0.5}, {"name": "fx:short"}, {"name": "fx:long", "stream": false}]},
 "c": {"sounds": [{"name": "fx:long"}]}}"#,
    );
    let out = Fixture::new().execute(&files, &StreamSounds, "", Scope::all(), &[]);
    assert_eq!(bytes(&out, SOUNDS), br#"{"a": {"sounds": [{"name":"fx:long","stream":true}, "fx:short"]},
 "b": {"sounds": [{"name": "fx:long", "volume": 0.5,"stream":true}, {"name": "fx:short"}, {"name": "fx:long", "stream": true}]},
 "c": {"sounds": [{"name": "fx:long","stream":true}]}}"#);
    assert_eq!(
        out.reports[0].changes,
        [
            "stream a: fx:long (6.0 s)",
            "stream b: fx:long (6.0 s)",
            "stream b: fx:long (6.0 s)",
            "stream c: fx:long (6.0 s)"
        ]
    );
    assert_eq!(out.reports[0].measured.len(), 4);
}

#[test]
fn stream_duplicate_event_and_sounds_keys_are_all_spliced_like_1x() {
    let files =
        sounds(r#"{"a":{"sounds":["fx:long"],"sounds":["fx:long"]},"a":{"sounds":["fx:long"]}}"#);
    let out = Fixture::new().execute(&files, &StreamSounds, "", Scope::all(), &[]);
    assert_eq!(bytes(&out, SOUNDS), br#"{"a":{"sounds":[{"name":"fx:long","stream":true}],"sounds":[{"name":"fx:long","stream":true}]},"a":{"sounds":[{"name":"fx:long","stream":true}]}}"#);
    assert_eq!(out.reports[0].changes.len(), 3);
}

#[test]
fn stream_preloaded_event_missing_invalid_and_already_streamed_stay() {
    for json in [
        r#"{"a":{"sounds":[{"name":"fx:long","stream":true},{"name":"fx:long","preload":true},{"name":"fx:long","type":"event"},"fx:missing",{"volume":1},3]}}"#,
        r#"{"a":{"sounds":["fx:long""#,
    ] {
        let files = sounds(json);
        let out = Fixture::new().execute(&files, &StreamSounds, "", Scope::all(), &[]);
        assert_eq!(bytes(&out, SOUNDS), files[SOUNDS]);
        assert!(out.reports[0].changed.is_empty());
    }
}

#[test]
fn stream_threshold_is_strict_and_scope_is_the_sounds_json() {
    let files = sounds(r#"{"a":{"sounds":["fx:long"]}}"#);
    let fixture = Fixture::new();
    let equal = fixture.execute(&files, &StreamSounds, "seconds = 6", Scope::all(), &[]);
    assert_eq!(bytes(&equal, SOUNDS), files[SOUNDS]);
    let below = fixture.execute(
        &files,
        &StreamSounds,
        "seconds = 5.99",
        Scope::matching(|p| p.as_str() == SOUNDS),
        &[],
    );
    assert_eq!(below.reports[0].changed, [physical(SOUNDS)]);
    let outside = fixture.execute(
        &files,
        &StreamSounds,
        "",
        Scope::matching(|p| p.as_str().ends_with(".ogg")),
        &[],
    );
    assert_eq!(bytes(&outside, SOUNDS), files[SOUNDS]);
    let proof = fixture.prove(&files, &StreamSounds, "");
    assert_eq!(proof.loss.unwrap().differing_keys, 1);
}

#[test]
fn stream_overlay_uses_its_own_layer_then_the_base_not_another_overlay() {
    let mut files = sounds(r#"{"a":{"sounds":["fx:long"]}}"#);
    text(
        &mut files,
        "pack.mcmeta",
        r#"{"pack":{"pack_format":88,"description":"test","supported_formats":[88,89]},"overlays":{"entries":[{"formats":89,"directory":"v89"}]}}"#,
    );
    text(
        &mut files,
        "v89/assets/fx/sounds.json",
        r#"{"a":{"sounds":["fx:long"]}}"#,
    );
    files.insert("v89/assets/fx/sounds/long.ogg".into(), SHORT.to_vec());
    let out = Fixture::new().execute(&files, &StreamSounds, "", Scope::all(), &[]);
    assert!(String::from_utf8_lossy(bytes(&out, SOUNDS)).contains("stream"));
    assert_eq!(
        bytes(&out, "v89/assets/fx/sounds.json"),
        files["v89/assets/fx/sounds.json"]
    );
    files.remove("v89/assets/fx/sounds/long.ogg");
    let out = Fixture::new().execute(&files, &StreamSounds, "", Scope::all(), &[]);
    assert!(String::from_utf8_lossy(bytes(&out, "v89/assets/fx/sounds.json")).contains("stream"));
}

#[test]
fn mono_only_stereo_and_rate_only_above_the_cap_are_transcoded() {
    let mut files = sounds(r#"{"a":{"sounds":["fx:long","fx:short","fx:high"]}}"#);
    files.insert("assets/fx/sounds/high.ogg".into(), HIGH.to_vec());
    let fixture = Fixture::new();
    let mono = fixture.execute(&files, &Audio::Mono, "", Scope::all(), &[]);
    assert_eq!(
        audio::decode(bytes(&mono, "assets/fx/sounds/long.ogg"))
            .unwrap()
            .channels
            .len(),
        1
    );
    assert_eq!(bytes(&mono, "assets/fx/sounds/short.ogg"), SHORT);
    let rate = fixture.execute(&files, &Audio::Rate, "", Scope::all(), &[]);
    assert_eq!(
        audio::decode(bytes(&rate, "assets/fx/sounds/high.ogg"))
            .unwrap()
            .rate,
        48000
    );
    assert_eq!(bytes(&rate, "assets/fx/sounds/long.ogg"), LONG);
    assert_eq!(bytes(&rate, "assets/fx/sounds/short.ogg"), SHORT);
    let proof = fixture.prove(&files, &Audio::Rate, "");
    assert!(proof.loss.unwrap().differing_keys > 0);
    for result in [&mono, &rate] {
        assert!(
            result.reports[0]
                .measured
                .iter()
                .any(|loss| loss.metric == "lsd" && loss.value.is_finite())
        );
        assert!(
            result.reports[0]
                .measured
                .iter()
                .any(|loss| loss.metric == "pcm_after" && loss.value > 0.0)
        );
    }
}

#[test]
fn mono_scope_threshold_and_determinism_are_enforced() {
    let files = sounds(r#"{"a":{"sounds":["fx:long"]}}"#);
    let fixture = Fixture::new();
    let outside = fixture.execute(&files, &Audio::Mono, "", Scope::matching(|_| false), &[]);
    assert_eq!(bytes(&outside, "assets/fx/sounds/long.ogg"), LONG);
    let refused = fixture.execute(&files, &Audio::Mono, "max_lsd = 0", Scope::all(), &[]);
    assert_eq!(bytes(&refused, "assets/fx/sounds/long.ogg"), LONG);
    let first = fixture.prove(&files, &Audio::Mono, "");
    let second = fixture.prove(&files, &Audio::Mono, "");
    assert_eq!(first.output.pack.digest, second.output.pack.digest);
    assert_eq!(first.reports, second.reports);
    assert!(first.loss.unwrap().differing_keys > 0);
}

#[test]
fn sound_options_are_validated_before_any_edit() {
    let fixture = Fixture::new();
    let cx = fixture.context();
    let state = cx.analyse(pack(&sounds(r#"{}"#))).unwrap();
    for (pass, options) in [
        (&Audio::Mono as &dyn Pass, "quality = 11"),
        (&Audio::Rate, "cap = 0"),
        (&Audio::Ogg, "max_lsd = -1"),
        (&Audio::Convert, "quality = nan"),
        (&StreamSounds, "seconds = -1"),
        (&StreamSounds, "seconds = nan"),
    ] {
        assert!(
            run(
                &cx,
                &Kept::default(),
                &state,
                &[Enabled {
                    pass,
                    scope: Scope::all(),
                    options: options.parse().unwrap()
                }]
            )
            .is_err()
        );
    }
}

fn wav() -> Vec<u8> {
    let frames = 4410u32;
    let mut bytes = b"RIFF".to_vec();
    bytes.extend((36 + frames * 2).to_le_bytes());
    bytes.extend(b"WAVEfmt ");
    bytes.extend(16u32.to_le_bytes());
    bytes.extend(1u16.to_le_bytes());
    bytes.extend(1u16.to_le_bytes());
    bytes.extend(22050u32.to_le_bytes());
    bytes.extend(44100u32.to_le_bytes());
    bytes.extend(2u16.to_le_bytes());
    bytes.extend(16u16.to_le_bytes());
    bytes.extend(b"data");
    bytes.extend((frames * 2).to_le_bytes());
    for i in 0..frames {
        bytes.extend(
            (((i as f32 * 440.0 * std::f32::consts::TAU / 22050.0).sin() * 16000.0) as i16)
                .to_le_bytes(),
        );
    }
    bytes
}

#[test]
fn audio_conversion_moves_only_supported_sound_assets_and_preserves_ogg_collision() {
    let mut files = files();
    let source = "assets/fx/sounds/tone.wav";
    let destination = "assets/fx/sounds/tone.ogg";
    files.insert(source.into(), wav());
    files.insert("outside.wav".into(), wav());
    text(&mut files, SOUNDS, r#"{"tone":{"sounds":["fx:tone"]}}"#);
    let fixture = Fixture::new();
    let out = fixture.execute(&files, &Audio::Convert, "", Scope::all(), &[]);
    assert!(!out.state.pack.files.contains_key(&physical(source)));
    let pcm = audio::decode(bytes(&out, destination)).unwrap();
    assert_eq!(pcm.rate, 22050);
    assert_eq!(pcm.channels.len(), 1);
    assert_eq!(bytes(&out, "outside.wav"), files["outside.wav"]);
    assert!(!out.reports[0].measured.is_empty());
    let proof = fixture.prove(&files, &Audio::Convert, "");
    assert!(proof.output.pack.files.contains_key(&physical(destination)));
    assert!(proof.loss.unwrap().differing_keys > 0);
    let outside = fixture.execute(
        &files,
        &Audio::Convert,
        "",
        Scope::matching(move |p| p.as_str() == source),
        &[],
    );
    assert_eq!(bytes(&outside, source), files[source]);
    files.insert(destination.into(), SHORT.to_vec());
    let collision = fixture.execute(&files, &Audio::Convert, "", Scope::all(), &[]);
    assert_eq!(bytes(&collision, source), files[source]);
    assert_eq!(bytes(&collision, destination), SHORT);
    assert!(collision.reports[0].changes[0].contains("already exists"));
}

#[test]
fn ogg_transcode_keeps_unprofitable_or_unmeasurable_files() {
    let mut files = sounds(r#"{"a":{"sounds":["fx:long"]}}"#);
    let fixture = Fixture::new();
    let out = fixture.execute(&files, &Audio::Ogg, "quality = -2", Scope::all(), &[]);
    assert!(bytes(&out, "assets/fx/sounds/long.ogg").len() <= LONG.len());
    assert!(!out.reports[0].changed.is_empty());
    let proof = fixture.prove(&files, &Audio::Ogg, "quality = -2");
    assert!(proof.loss.unwrap().differing_keys > 0);
    let silence = audio::Pcm {
        rate: 22050,
        channels: vec![vec![0.0; 100]],
    };
    files.insert(
        "assets/fx/sounds/short.ogg".into(),
        audio::encode(&silence, 0.0).unwrap(),
    );
    let out = fixture.execute(&files, &Audio::Ogg, "", Scope::all(), &[]);
    assert_eq!(
        bytes(&out, "assets/fx/sounds/short.ogg"),
        files["assets/fx/sounds/short.ogg"]
    );
    assert!(
        out.reports[0]
            .changes
            .iter()
            .any(|line| line.contains("loss cannot be measured"))
    );
}

#[test]
fn registry_lists_only_lossy_d5_passes_and_png_optimisers() {
    let passes = super::passes();
    assert_eq!(
        passes.iter().map(|pass| pass.name()).collect::<Vec<_>>(),
        [
            "stream-sounds",
            "sound-mono",
            "sound-rate",
            "ogg-transcode",
            "audio-to-ogg"
        ]
    );
    assert!(
        passes
            .iter()
            .all(|pass| pass.loss() == packprune_pass::Loss::Lossy)
    );
    for pass in passes {
        assert!(pass.options_schema().as_object().is_some());
    }
}
