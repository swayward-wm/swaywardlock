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
    assert_eq!(off.auth.fingerprint, None, "fingerprint is off by default");

    let fp =
        Config::parse_mem("auth { fingerprint { ready-message \"Touch\"; retry-delay-ms 100; }; }")
            .unwrap()
            .auth
            .fingerprint
            .unwrap();
    assert_eq!(fp.ready_message, "Touch");
    assert_eq!(fp.retry_delay_ms, 100);
    assert_eq!(fp.present_message, Fingerprint::default().present_message);
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
            screenshot
            contrast 0.9
            brightness 0.8
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
    assert!(bg.screenshot);
    assert_eq!((bg.contrast, bg.brightness), (0.9, 0.8));
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
fn shape_node() {
    let config = Config::parse_mem(
        r##"
        shape {
            size "50%" 60
            color "#11111180"
            corner-radius "full"
            border { width 3; gradient from="#80c8ff" to="#c7ff7f" angle=45 in="oklch longer hue"; }
            shadow { softness 20; spread 2; offset x=0 y=4; color "#00000080"; }
            z-index 2
        }
        shape { xray; corner-radius 1 2 3 4; border { color "#fff"; }; }
        "##,
    )
    .unwrap();
    let shapes: Vec<&Shape> = config
        .widgets
        .iter()
        .filter_map(|w| match w {
            Widget::Shape(s) => Some(s),
            _ => None,
        })
        .collect();
    let a = shapes[0];
    assert_eq!(a.size, (Length::Fraction(0.5), Length::Px(60.)));
    assert_eq!(a.corner_radius, None);
    let b = a.border.unwrap();
    assert_eq!(b.width, 3.);
    assert_eq!(b.gradient.angle, 45);
    assert!(a.shadow.on);
    assert_eq!(a.shadow.softness, 20.);
    assert_eq!(a.common.z_index, 2);

    let x = shapes[1];
    assert!(x.xray);
    assert_eq!(x.corner_radius.unwrap().bottom_left, 4.);
    assert_eq!(x.border.unwrap().width, 2.);
    assert!(!x.shadow.on);
}

#[test]
fn label_and_image_nodes() {
    let config = Config::parse_mem(
        r##"
        label { text "cmd[update:1000] date"; font "Sans Bold 40"; align "left"; markup false; }
        image { path "~/face.png"; size 120; corner-radius 8; border { width 3; color "#fff"; }; }
        "##,
    )
    .unwrap();
    let Widget::Label(l) = &config.widgets[0] else {
        panic!("label")
    };
    assert_eq!(l.text, "cmd[update:1000] date");
    assert_eq!(l.font, "Sans Bold 40");
    assert_eq!(l.align, TextAlign::Left);
    assert!(!l.markup);
    let Widget::Image(i) = &config.widgets[1] else {
        panic!("image")
    };
    assert!(i.path.is_absolute(), "~ expanded: {:?}", i.path);
    assert_eq!(i.size, 120.);
    assert_eq!(i.corner_radius.unwrap().top_left, 8.);
    assert_eq!(i.border.unwrap().width, 3.);
}

/// hyprlock's dots_rounding sentinels: -1 round, -2 follow the field's rounding.
#[test]
fn dot_corner_radius_sentinels() {
    for (kdl, want) in [
        ("dots corner-radius=-1", DotRadius::Round),
        ("dots corner-radius=-2", DotRadius::Inherit),
        ("dots corner-radius=0", DotRadius::Px(0.)),
        ("dots", DotRadius::Round),
    ] {
        let config = Config::parse_mem(&format!("input-field {{ {kdl}; }}")).unwrap();
        let Widget::InputField(f) = &config.widgets[0] else {
            panic!()
        };
        assert_eq!(f.dots.corner_radius, want, "{kdl}");
    }
    assert!(Config::parse_mem("input-field { dots corner-radius=-3; }").is_err());
}

#[test]
fn input_field_node() {
    let config = Config::parse_mem(
        r##"
        input-field {
            size 300 60
            outline { width 2; gradient from="#80c8ff" to="#c7ff7f" angle=45; }
            dots size=0.33 spacing=0.15 center=false corner-radius=3 text="*"
            hide-input "#ff0000"
            fade-on-empty timeout-ms=500
            check { color "#00ff00"; text "Checking"; }
            fail { gradient from="#f00" to="#800"; text "$FAIL ($ATTEMPTS)"; }
            capslock-color "#ffaa00"
            numlock-gradient from="#00f" to="#008"
            invert-numlock
        }
        input-field { fade-on-empty false; }
        "##,
    )
    .unwrap();
    let Widget::InputField(f) = &config.widgets[0] else {
        panic!()
    };
    assert_eq!(f.outline.width, 2.);
    assert_eq!(
        (f.dots.size, f.dots.spacing, f.dots.center),
        (0.33, 0.15, false)
    );
    assert_eq!(f.dots.text.as_deref(), Some("*"));
    assert_eq!(f.dots.corner_radius, DotRadius::Px(3.));
    assert_eq!(
        f.hide_input,
        Some(Color::from_rgba8_unpremul(255, 0, 0, 255))
    );
    assert_eq!(f.fade_on_empty, Some(500));
    assert_eq!(f.check.text, "Checking");
    assert_eq!(f.fail.text, "$FAIL ($ATTEMPTS)");
    assert!(f.capslock_color.is_some() && f.numlock_color.is_some() && f.invert_numlock);
    let Widget::InputField(g) = &config.widgets[1] else {
        panic!()
    };
    assert_eq!(g.fade_on_empty, None);
}

#[test]
fn animations_use_swaywards_syntax() {
    let config = Config::parse_mem(
        r#"
        animations {
            fade-in { duration-ms 400; curve "cubic-bezier" 0.05 0.7 0.1 1; }
            fade-out { spring damping-ratio=1.0 stiffness=800 epsilon=0.001; }
            input-field-fade { off; }
        }
        "#,
    )
    .unwrap();
    let a = config.animations;
    assert!(!a.off);
    match a.fade_in.kind {
        AnimationKind::Easing(e) => {
            assert_eq!(e.duration_ms, 400);
            assert_eq!(e.curve, AnimationCurve::CubicBezier(0.05, 0.7, 0.1, 1.));
        }
        _ => panic!("easing"),
    }
    assert!(matches!(a.fade_out.kind, AnimationKind::Spring(_)));
    assert!(a.input_field_fade.off);
    assert!(
        Config::parse_mem("animations { off; }")
            .unwrap()
            .animations
            .off
    );
    assert!(Config::parse_mem("animations { window-open { off; }; }").is_err());
}

#[test]
fn shipped_examples_parse() {
    for entry in std::fs::read_dir(concat!(env!("CARGO_MANIFEST_DIR"), "/examples")).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_some_and(|e| e == "kdl") {
            let text = std::fs::read_to_string(&path).unwrap();
            if let Err(err) = Config::parse(&path, &text).config {
                panic!("{path:?}: {:?}", miette::Report::new(err));
            }
        }
    }
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
        ("shape { border { width 2; } }", "border without colour"),
        ("label { font \"Sans\"; }", "label without text"),
        ("input-field { dots size=2; }", "dot size over 1"),
        ("input-field { dots wobble=1; }", "unknown dots property"),
        ("image { size 10; }", "image without path"),
        (
            "label { text \"x\"; shadow { softness 1; }; }",
            "label shadow (not supported)",
        ),
        (
            "shape { corner-radius \"round\"; }",
            "bad corner radius word",
        ),
        (
            "shape { shadow { draw-behind-window true; } }",
            "window-only shadow option",
        ),
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
