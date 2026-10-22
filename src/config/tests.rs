use std::io::Write as _;

use super::*;

fn ring(config: &Config) -> &RingIndicator {
    config
        .widgets
        .iter()
        .find_map(|w| match w {
            Widget::RingIndicator(r) => Some(r),
            _ => None,
        })
        .expect("a ring-indicator")
}

fn background(config: &Config) -> &Background {
    config
        .widgets
        .iter()
        .find_map(|w| match w {
            Widget::Background(b) => Some(b),
            _ => None,
        })
        .expect("a background")
}

#[test]
fn empty_file_gives_the_default_look() {
    let config = Config::parse_mem("").unwrap();
    assert_eq!(config, Config::default());
}

#[test]
fn any_widget_replaces_the_default_set() {
    let config = Config::parse_mem(r##"background { color "#000000"; }"##).unwrap();
    assert_eq!(config.widgets.len(), 1);
    assert_eq!(
        background(&config).color,
        Color::from_rgba8_unpremul(0, 0, 0, 255)
    );
}

#[test]
fn general_and_auth() {
    let config = Config::parse_mem(
        r#"
        general {
            hide-cursor false
            ignore-empty-input
            fail-timeout-ms 1500
        }
        auth { pam service="login"; }
        "#,
    )
    .unwrap();
    assert!(!config.general.hide_cursor);
    assert!(config.general.ignore_empty_input);
    assert_eq!(config.general.fail_timeout_ms, 1500);
    assert_eq!(config.auth.pam_service.as_deref(), Some("login"));

    let off = Config::parse_mem("auth { pam \"off\"; }").unwrap();
    assert_eq!(off.auth.pam_service, None);
}

#[test]
fn background_node() {
    let config = Config::parse_mem(
        r##"
        background output="desc:Dell U27" {
            color "#102030"
            path "/tmp/wall.png"
            fit "tile"
            blur { passes 2; }
        }
        "##,
    )
    .unwrap();
    let bg = background(&config);
    assert_eq!(
        bg.common.output,
        Some(OutputMatch::DescriptionPrefix("Dell U27".into()))
    );
    assert_eq!(bg.common.z_index, -1);
    assert_eq!(
        bg.path.as_deref(),
        Some(std::path::Path::new("/tmp/wall.png"))
    );
    assert_eq!(bg.fit, Fit::Tile);
    assert!(!bg.blur.off);
    assert_eq!(bg.blur.passes, 2);
    // Unset blur fields keep swayward's defaults.
    assert_eq!(bg.blur.offset, Blur::default().offset);
}

#[test]
fn ring_node_and_colors() {
    let config = Config::parse_mem(
        r##"
        ring-indicator output="DP-1" {
            radius 80
            thickness 7.5
            position 0 "-25%"
            valign "bottom"
            show-failed-attempts
            show-keyboard-layout "always"
            line-uses "inside"
            colors {
                state "wrong" inside="#ff0000" text="#ffffff80"
                key-highlight "#00ff00"
                layout bg="#000000" text="#ffffff"
            }
        }
        "##,
    )
    .unwrap();
    let r = ring(&config);
    assert_eq!(r.common.output, Some(OutputMatch::Name("DP-1".into())));
    assert_eq!(r.radius, 80.);
    assert_eq!(r.thickness, 7.5);
    assert_eq!(r.common.position, (Length::Px(0.), Length::Fraction(-0.25)));
    assert_eq!(r.common.valign, VAlign::Bottom);
    assert!(r.show_failed_attempts);
    assert_eq!(r.show_keyboard_layout, ShowLayout::Always);
    assert_eq!(r.line_uses, LineUses::Inside);
    assert_eq!(
        r.colors.inside.wrong,
        Color::from_rgba8_unpremul(255, 0, 0, 255)
    );
    assert_eq!(
        r.colors.text.wrong,
        Color::from_rgba8_unpremul(255, 255, 255, 0x80)
    );
    // Untouched state colours keep swaylock's defaults.
    assert_eq!(r.colors.inside.input, RingColors::default().inside.input);
    assert_eq!(
        r.colors.key_highlight,
        Color::from_rgba8_unpremul(0, 255, 0, 255)
    );
    assert_eq!(r.colors.layout_bg, Color::from_rgba8_unpremul(0, 0, 0, 255));
}

#[test]
fn gradient_syntax_is_swaywards() {
    // The copied Gradient decodes swayward's own `active-gradient` syntax; spot-check `in`.
    use super::copied::color::{GradientColorSpace, GradientInterpolation, HueInterpolation};
    let i: GradientInterpolation = "oklch longer hue".parse().unwrap();
    assert_eq!(i.color_space, GradientColorSpace::Oklch);
    assert_eq!(i.hue_interpolation, HueInterpolation::Longer);
}

#[test]
fn errors() {
    for (text, why) in [
        ("frobnicate", "unknown root node"),
        ("general {}\ngeneral {}", "duplicate general"),
        ("ring-indicator { radius 0; }", "radius below 1"),
        (
            "ring-indicator { colors { state \"idle\" ring=\"#fff\"; } }",
            "unknown state",
        ),
        ("background { fit \"zoom\"; }", "unknown fit"),
        (
            "ring-indicator { position 1; }",
            "position needs two values",
        ),
        ("background { position \"x%\" 0; }", "bad percentage"),
        // A clear timer shorter than typing makes the password impossible to enter.
        ("general { clear-input-after-ms 0; }", "zero clear timer"),
        (
            "general { clear-input-after-ms 999; }",
            "sub-second clear timer",
        ),
    ] {
        assert!(Config::parse_mem(text).is_err(), "accepted {why}: {text}");
    }
}

#[test]
fn include_merges_in_order() {
    let dir = tempfile::tempdir().unwrap();
    let mut inc = std::fs::File::create(dir.path().join("colors.kdl")).unwrap();
    writeln!(inc, r##"background {{ color "#010203"; }}"##).unwrap();
    let main = dir.path().join("config.kdl");
    std::fs::write(
        &main,
        "general { fail-timeout-ms 10; }\ninclude \"colors.kdl\"\nring-indicator {}\n",
    )
    .unwrap();

    let config = Config::load(Some(&main)).unwrap();
    assert_eq!(config.general.fail_timeout_ms, 10);
    assert!(matches!(config.widgets[0], Widget::Background(_)));
    assert!(matches!(config.widgets[1], Widget::RingIndicator(_)));

    std::fs::write(&main, "include \"missing.kdl\"").unwrap();
    assert!(Config::load(Some(&main)).is_err());
    std::fs::write(&main, "include optional=true \"missing.kdl\"").unwrap();
    assert_eq!(Config::load(Some(&main)).unwrap(), Config::default());
}

#[test]
fn output_match() {
    let name: OutputMatch = "DP-1".parse().unwrap();
    assert!(name.matches(Some("DP-1"), None));
    assert!(!name.matches(Some("DP-2"), Some("DP-1")));
    let desc: OutputMatch = "desc:Dell".parse().unwrap();
    assert!(desc.matches(None, Some("Dell Inc. U2723QE")));
    assert!(!desc.matches(Some("Dell"), None));
}
