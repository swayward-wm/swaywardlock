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
    // `off` as a child node, as in `animations { off }` and `blur { off; }`.
    for text in ["auth { pam { off; }; }", "auth {\n pam {\n off\n }\n}"] {
        let config = Config::parse_mem(text).unwrap();
        assert_eq!(config.auth.pam_service, None, "{text}");
    }
    assert!(Config::parse_mem("auth { pam { of; }; }").is_err());
    // Any other string is a mistake, not `off`: PAM off leaves only SIGUSR1 to unlock.
    for text in ["auth { pam \"of\"; }", "auth { pam \"swaywardlock\"; }"] {
        assert!(Config::parse_mem(text).is_err(), "accepted {text}");
    }
    assert!(!config.auth.pam_reuse_password, "off by default");
    let reuse = Config::parse_mem("auth { pam service=\"login\" { reuse-password; }; }").unwrap();
    assert!(reuse.auth.pam_reuse_password);
    assert_eq!(reuse.auth.pam_service.as_deref(), Some("login"));
    // A flag: no arguments.
    assert!(Config::parse_mem("auth { pam { reuse-password 1; }; }").is_err());
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
fn background_vibrancy() {
    let config = Config::parse_mem(
        r##"
        background {
            blur { passes 2; noise 0.1; saturation 0.5; }
            vibrancy 0.1686
            vibrancy-darkness 1
        }
        "##,
    )
    .unwrap();
    let bg = background(&config);
    assert_eq!((bg.vibrancy, bg.vibrancy_darkness), (0.1686, 1.));
    assert_eq!((bg.blur.noise, bg.blur.saturation), (0.1, 0.5));
    // Off by default, like contrast and brightness.
    let bg = Background::default();
    assert_eq!((bg.vibrancy, bg.vibrancy_darkness), (0., 0.));
    // Without blur they decode (with a warning) and do nothing.
    assert!(Config::parse_mem("background { vibrancy 1; vibrancy-darkness 0.5; }").is_ok());
    for (text, why) in [
        ("background { vibrancy -0.1; }", "negative vibrancy"),
        ("background { vibrancy 10.5; }", "vibrancy over 10"),
        (
            "background { vibrancy-darkness -0.1; }",
            "negative darkness",
        ),
        ("background { vibrancy-darkness 1.5; }", "darkness over 1"),
        ("background { vibrancy \"a\"; }", "not a number"),
    ] {
        assert!(Config::parse_mem(text).is_err(), "accepted {why}: {text}");
    }
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
            font-size 18
            show-caps-lock-text false
            show-caps-lock-indicator
            colors {
                state "wrong" inside="#ff0000" text="#ffffff80"
                key-highlight "#00ff00"
                caps-lock-key-highlight "#0000ff"
                caps-lock-bs-highlight "#ff00ff"
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
    assert_eq!(r.font_size, 18.);
    assert!(!r.show_caps_lock_text && r.show_caps_lock_indicator);
    assert_eq!(
        (
            r.colors.caps_lock_key_highlight,
            r.colors.caps_lock_bs_highlight
        ),
        (
            Color::from_rgba8_unpremul(0, 0, 255, 255),
            Color::from_rgba8_unpremul(255, 0, 255, 255)
        )
    );
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
fn ring_style_and_glow() {
    // Default: swaylock's ring, no glow.
    let r = RingIndicator::default();
    assert_eq!(r.style, RingStyle::Swaylock);
    assert_eq!(r.glow, 0.);
    let config = Config::parse_mem("ring-indicator { style \"swaylock\"; }").unwrap();
    assert_eq!(ring(&config).style, RingStyle::Swaylock);
    assert_eq!(ring(&config).glow, 0.);
    // swayward: glow 12 when absent, any value in 0..64 otherwise, in either order.
    let config = Config::parse_mem("ring-indicator { style \"swayward\"; }").unwrap();
    assert_eq!(ring(&config).style, RingStyle::Swayward);
    assert_eq!(ring(&config).glow, 12.);
    for (text, want) in [
        ("style \"swayward\"; glow 0", 0.),
        ("glow 64; style \"swayward\"", 64.),
        ("style \"swayward\"; glow 2.5", 2.5),
    ] {
        let config = Config::parse_mem(&format!("ring-indicator {{ {text}; }}")).unwrap();
        assert_eq!(ring(&config).glow, want, "{text}");
    }
    for (text, why) in [
        ("style \"swayward\"; glow 65", "glow over 64"),
        ("style \"swayward\"; glow -1", "negative glow"),
        ("style \"fancy\"", "unknown style"),
        ("glow 4", "glow with the default swaylock style"),
        (
            "style \"swaylock\"; glow 0",
            "any glow with swaylock, even 0",
        ),
        ("glow 4; style \"swaylock\"", "glow before style swaylock"),
    ] {
        let text = format!("ring-indicator {{ {text}; }}");
        assert!(Config::parse_mem(&text).is_err(), "accepted {why}: {text}");
    }
}

#[test]
fn ring_animations_parse_with_defaults() {
    let d = Config::parse_mem("").unwrap().animations;
    let easing = |a: Animation| match a.kind {
        AnimationKind::Easing(e) if !a.off => (e.duration_ms, e.curve),
        _ => panic!("easing"),
    };
    assert_eq!(easing(d.ring_colors), (250, AnimationCurve::EaseOutCubic));
    assert_eq!(
        easing(d.ring_highlight),
        (180, AnimationCurve::EaseOutCubic)
    );
    assert_eq!(easing(d.ring_appear), (250, AnimationCurve::EaseOutCubic));
    assert_eq!(easing(d.ring_wrong), (450, AnimationCurve::Linear));
    let a = Config::parse_mem(
        r#"
        animations {
            ring-colors { duration-ms 100; curve "linear"; }
            ring-highlight { off; }
            ring-appear { spring damping-ratio=1.0 stiffness=800 epsilon=0.001; }
            ring-wrong { duration-ms 900; }
        }
        "#,
    )
    .unwrap()
    .animations;
    assert_eq!(easing(a.ring_colors), (100, AnimationCurve::Linear));
    assert!(a.ring_highlight.off);
    assert!(matches!(a.ring_appear.kind, AnimationKind::Spring(_)));
    assert_eq!(easing(a.ring_wrong).0, 900);
    assert!(Config::parse_mem("animations { ring-spin { off; }; }").is_err());
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

/// hyprlock's `rotate`: degrees counter-clockwise, on labels, images and shapes only.
#[test]
fn rotate_decodes_on_label_image_and_shape() {
    let config = Config::parse_mem(
        r##"
        label { text "x"; rotate 30; }
        image { path "/face.png"; rotate 30; }
        shape { rotate 30; }
        shape { rotate -90; }
        shape { rotate 720.5; }
        shape { }
        shape { rotate -0.00000000000000001; }
        shape { rotate -360; }
        "##,
    )
    .unwrap();
    let angles: Vec<f64> = config
        .widgets
        .iter()
        .map(|w| match w {
            Widget::Label(l) => l.rotate,
            Widget::Image(i) => i.rotate,
            Widget::Shape(s) => s.rotate,
            _ => panic!("unexpected widget"),
        })
        .collect();
    // A tiny negative angle must not normalise to 360 (which would take the rotated path).
    assert_eq!(angles, [30., 30., 30., 270., 0.5, 0., 0., 0.]);
    for (text, why) in [
        ("input-field { rotate 30; }", "input-field"),
        ("ring-indicator { rotate 30; }", "ring-indicator"),
        ("background { rotate 30; }", "background"),
        ("shape { rotate 3601; }", "over the limit"),
    ] {
        assert!(
            Config::parse_mem(text).is_err(),
            "accepted rotate on {why}: {text}"
        );
    }
}

/// hyprlock's label `shadow_*`: `text-shadow` on labels only, and `passes` is required.
#[test]
fn text_shadow_decodes_with_defaults() {
    let config = Config::parse_mem(
        r##"
        label { text "x"; text-shadow { passes 2; }; }
        label { text "x"; text-shadow { passes 3; size 5; color "#ff000080"; boost 2; offset x=4 y=-6; }; }
        label { text "x"; text-shadow { passes 0; }; }
        label { text "x"; }
        label { text "x"; text-shadow { passes 4; size 20; boost 10; }; }
        "##,
    )
    .unwrap();
    let shadows: Vec<_> = config
        .widgets
        .iter()
        .map(|w| match w {
            Widget::Label(l) => l.text_shadow,
            _ => panic!("label"),
        })
        .collect();
    let d = shadows[0].unwrap();
    assert_eq!(d.passes, 2);
    assert_eq!(d.size, 3.);
    assert_eq!(d.boost, 1.2);
    assert_eq!(d.color, Color::from_rgba8_unpremul(0, 0, 0, 255));
    assert_eq!(d.offset, (0., 0.));
    let s = shadows[1].unwrap();
    assert_eq!((s.passes, s.size, s.boost), (3, 5., 2.));
    assert_eq!(s.color, Color::from_rgba8_unpremul(255, 0, 0, 0x80));
    assert_eq!(s.offset, (4., -6.));
    // passes 0 is accepted and means off, like an absent block (hyprlock's default).
    assert_eq!(shadows[2], None);
    assert_eq!(shadows[3], None);
    assert!(shadows[4].is_some());
}

#[test]
fn text_shadow_needs_passes_and_stays_in_range() {
    for (text, why) in [
        (
            r#"label { text "x"; text-shadow { size 3; }; }"#,
            "no passes",
        ),
        (r#"label { text "x"; text-shadow { }; }"#, "empty block"),
        (
            r#"label { text "x"; text-shadow { passes 5; }; }"#,
            "passes over 4",
        ),
        (
            r#"label { text "x"; text-shadow { passes 2; size 21; }; }"#,
            "size over 20",
        ),
        (
            r#"label { text "x"; text-shadow { passes -1; }; }"#,
            "negative passes",
        ),
        (
            r#"label { text "x"; text-shadow { passes 1.5; }; }"#,
            "fractional passes",
        ),
        (
            r#"label { text "x"; text-shadow { passes 2; size 101; }; }"#,
            "size over 100",
        ),
        (
            r#"label { text "x"; text-shadow { passes 2; size -1; }; }"#,
            "negative size",
        ),
        (
            r#"label { text "x"; text-shadow { passes 2; boost 11; }; }"#,
            "boost over 10",
        ),
        (
            r#"label { text "x"; text-shadow { passes 2; spread 3; }; }"#,
            "unknown child",
        ),
        (
            r#"label { text "x"; text-shadow { passes 2; offset x=100000; }; }"#,
            "offset range",
        ),
        // The box shadow of shape, image and input-field is another thing: not on labels.
        (
            r#"label { text "x"; shadow { softness 3; }; }"#,
            "box shadow on a label",
        ),
        (r#"shape { text-shadow { passes 2; }; }"#, "shape"),
        (
            r#"image { path "/face.png"; text-shadow { passes 2; }; }"#,
            "image",
        ),
        (
            r#"input-field { text-shadow { passes 2; }; }"#,
            "input-field",
        ),
        (
            r#"ring-indicator { text-shadow { passes 2; }; }"#,
            "ring-indicator",
        ),
        (r#"background { text-shadow { passes 2; }; }"#, "background"),
    ] {
        assert!(Config::parse_mem(text).is_err(), "accepted ({why}): {text}");
    }
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
            bothlock-color "#123456"
            invert-numlock
            swap-font-color
            shadow { softness 7; }
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
    assert!(f.bothlock_color.is_some() && f.swap_font_color);
    assert!(f.shadow.on && f.shadow.softness == 7., "{:?}", f.shadow);
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

/// The fade-out runs before every unlock; validate once accepted 49.7 days of it. The input
/// field's animations keep the redraw ticker running, and once had no cap at all.
/// Every animation name, ring-* included: the caps apply to all of them.
const ANIMATION_NAMES: [&str; 10] = [
    "fade-in",
    "fade-out",
    "input-field-fade",
    "input-field-width",
    "input-field-colors",
    "input-field-dots",
    "ring-colors",
    "ring-highlight",
    "ring-appear",
    "ring-wrong",
];

#[test]
fn animations_longer_than_three_seconds_are_rejected() {
    for name in ANIMATION_NAMES {
        for (body, why) in [
            ("duration-ms 4294967295;", "u32::MAX easing"),
            ("duration-ms 3001;", "just over the cap"),
            (
                "spring damping-ratio=10.0 stiffness=1 epsilon=0.00001;",
                "a spring that takes a minute",
            ),
        ] {
            let text = format!("animations {{ {name} {{ {body} }}; }}");
            assert!(Config::parse_mem(&text).is_err(), "accepted {why}: {text}");
        }
        for body in [
            "duration-ms 3000;",
            "spring damping-ratio=1.0 stiffness=800 epsilon=0.001;",
        ] {
            let text = format!("animations {{ {name} {{ {body} }}; }}");
            if let Err(e) = Config::parse_mem(&text) {
                panic!("rejected {text}: {e:?}");
            }
        }
    }
}

/// A dots curve with y 1000 overshot without bound and wedged the locker after a few keys.
#[test]
fn cubic_bezier_y_is_bounded() {
    for name in ANIMATION_NAMES {
        for curve in [
            "0 1000 1 1000",
            "0 3.01 1 1",
            "0 0 1 -2.01",
            "0 -100000 1 0",
        ] {
            let text = format!("animations {{ {name} {{ curve \"cubic-bezier\" {curve}; }}; }}");
            assert!(Config::parse_mem(&text).is_err(), "accepted {text}");
        }
        for curve in ["0 3 1 -2", "0.34 1.56 0.64 1", "0.68 -0.6 0.32 1.6"] {
            let text = format!("animations {{ {name} {{ curve \"cubic-bezier\" {curve}; }}; }}");
            if let Err(e) = Config::parse_mem(&text) {
                panic!("rejected {text}: {e:?}");
            }
        }
    }
}

#[test]
fn input_field_animations_parse_and_unknown_names_still_fail() {
    let config = Config::parse_mem(
        r#"
        animations {
            input-field-width { duration-ms 800; curve "linear"; }
            input-field-colors { off; }
            input-field-dots { duration-ms 200; curve "linear"; }
        }
        "#,
    )
    .unwrap();
    let a = config.animations;
    assert!(matches!(
        a.input_field_width.kind,
        AnimationKind::Easing(e) if e.duration_ms == 800 && e.curve == AnimationCurve::Linear
    ));
    assert!(a.input_field_colors.off);
    assert!(matches!(
        a.input_field_dots.kind,
        AnimationKind::Easing(e) if e.duration_ms == 200
    ));
    // Defaults when not set.
    let d = Config::parse_mem("").unwrap().animations;
    assert!(matches!(
        d.input_field_colors.kind,
        AnimationKind::Easing(e) if e.duration_ms == 300 && e.curve == AnimationCurve::Linear
    ));
    assert!(Config::parse_mem("animations { input-field-height { off; }; }").is_err());
}

/// `false` turns fade-on-empty off whatever else is on the node.
#[test]
fn fade_on_empty_false_wins_over_timeout() {
    let config = Config::parse_mem("input-field { fade-on-empty false timeout-ms=5; }").unwrap();
    let Widget::InputField(f) = &config.widgets[0] else {
        panic!()
    };
    assert_eq!(f.fade_on_empty, None);
    let config = Config::parse_mem("input-field { fade-on-empty true timeout-ms=5; }").unwrap();
    let Widget::InputField(f) = &config.widgets[0] else {
        panic!()
    };
    assert_eq!(f.fade_on_empty, Some(5));
}

/// fade-on-empty took any argument or child (only `false` turned it off), and every flag node
/// ignored stray properties and children.
#[test]
fn flags_take_only_a_boolean() {
    for bad in [
        "input-field { fade-on-empty 0; }",
        "input-field { fade-on-empty 1; }",
        "input-field { fade-on-empty \"false\"; }",
        "input-field { fade-on-empty \"x\"; }",
        "input-field { fade-on-empty 5; }",
        "input-field { fade-on-empty { foo; }; }",
        "input-field { fade-on-empty true false; }",
        "input-field { fade-on-empty foo=1; }",
        "input-field { hide-input foo=1; }",
        "input-field { hide-input true { kid; }; }",
        "input-field { invert-numlock 0; }",
        "input-field { swap-font-color x=1; }",
        "shape { xray foo=1; }",
        "label { markup { kid; }; }",
        "background { screenshot 1; }",
        "ring-indicator { idle-visible \"yes\"; }",
        "general { hide-cursor x=1 { kid; }; }",
        "general { ignore-empty-input true true; }",
    ] {
        assert!(Config::parse_mem(bad).is_err(), "accepted {bad}");
    }
    for ok in [
        "input-field { fade-on-empty; }",
        "input-field { fade-on-empty true timeout-ms=100; }",
        "input-field { fade-on-empty false; }",
        "input-field { hide-input \"#ff0000\"; }",
        "shape { xray false; }",
        "general { hide-cursor false; ignore-empty-input; }",
    ] {
        if let Err(e) = Config::parse_mem(ok) {
            panic!("rejected {ok}: {e:?}");
        }
    }
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

/// The shipped PAM file is swaylock's and hyprlock's: auth.rs runs only the auth phase, so the
/// file has only an auth line. An account line would be dead (pam_acct_mgmt is never called)
/// and suggest that account policy is enforced at unlock.
#[test]
fn shipped_pam_file_is_auth_only() {
    let text =
        std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/pam/swaywardlock")).unwrap();
    let rules: Vec<Vec<&str>> = text
        .lines()
        .filter(|l| !l.trim_start().starts_with('#') && !l.trim().is_empty())
        .map(|l| l.split_whitespace().collect())
        .collect();
    assert_eq!(rules, vec![vec!["auth", "include", "login"]], "{text}");
    assert!(
        !text.contains("pam_permit") && !text.contains("pam_succeed_if"),
        "the shipped file must never accept by itself: {text}"
    );
    assert_eq!(
        Config::default().auth.pam_service.as_deref(),
        Some("swaywardlock"),
        "the default service name is the shipped file's name"
    );
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

/// Image paths resolve against the directory of the file that names them, like `include`:
/// the locker may chdir to / (daemonize) before it loads images.
#[test]
fn relative_image_paths_resolve_against_the_config_dir() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("sub")).unwrap();
    std::fs::write(
        dir.path().join("sub/inc.kdl"),
        "image { path \"face.png\"; }\n",
    )
    .unwrap();
    let main = dir.path().join("config.kdl");
    std::fs::write(
        &main,
        "background { path \"wall.png\"; }\nimage { path \"/abs.png\"; }\ninclude \"sub/inc.kdl\"\n",
    )
    .unwrap();
    let config = Config::load(Some(&main)).unwrap();
    let paths: Vec<_> = config
        .widgets
        .iter()
        .filter_map(|w| match w {
            Widget::Background(b) => b.path.clone(),
            Widget::Image(i) => Some(i.path.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(
        paths,
        [
            dir.path().join("wall.png"),
            "/abs.png".into(),
            dir.path().join("sub/face.png"),
        ]
    );
}

/// `-c rel.kdl`, `-c ./rel.kdl` and `-c dir/rel.kdl` all give absolute image paths: the locker
/// loads images after `-f` has changed the working directory to `/`.
#[test]
fn relative_config_path_gives_absolute_image_paths() {
    let cwd = std::env::current_dir().unwrap();
    let text = "background { path \"wall.png\"; }\nimage { path \"./face.png\"; }\n";
    for (config, dir) in [("rel.kdl", ""), ("./rel.kdl", ""), ("conf/rel.kdl", "conf")] {
        let config = Config::parse(Path::new(config), text).config.unwrap();
        let paths: Vec<_> = config
            .widgets
            .iter()
            .filter_map(|w| match w {
                Widget::Background(b) => b.path.clone(),
                Widget::Image(i) => Some(i.path.clone()),
                _ => None,
            })
            .collect();
        let dir = cwd.join(dir);
        assert_eq!(paths, [dir.join("wall.png"), dir.join("./face.png")]);
        assert!(paths.iter().all(|p| p.is_absolute()), "{paths:?}");
    }
}

/// `hide-input false` turns it off, like every other flag.
#[test]
fn hide_input_false_turns_it_off() {
    let field = |kdl: &str| match Config::parse_mem(&format!("input-field {{ {kdl}; }}"))
        .unwrap()
        .widgets
        .remove(0)
    {
        Widget::InputField(f) => f,
        _ => panic!(),
    };
    assert_eq!(field("hide-input false").hide_input, None);
    assert!(field("hide-input true").hide_input.is_some());
    assert!(field("hide-input").hide_input.is_some());
    let c = field("hide-input \"#ff0000\"").hide_input.unwrap();
    assert_eq!(c, Color::from_rgba8_unpremul(255, 0, 0, 255));
}

/// Sizes are never negative or non-finite: smithay's Size asserts on negative values in debug
/// builds (an abort after lock).
#[test]
fn widget_sizes_are_positive_and_finite() {
    for text in [
        "shape { size -50 -50; }",
        "shape { size -50 1; }",
        "shape { size \"-50%\" \"20%\"; }",
        "shape { size \"1e300%\" 1; }",
        "input-field { size -100 -20; }",
        "input-field { size 100 \"-1%\"; }",
    ] {
        assert!(Config::parse_mem(text).is_err(), "accepted {text}");
    }
    // Positions stay signed; a zero size is allowed (draws nothing).
    Config::parse_mem("shape { size 0 \"50%\"; position -10 \"-5%\"; }").unwrap();
}

/// Deep nesting overflowed the main thread's stack (exit 134 instead of a config error), and an
/// include diamond was walked once per path.
#[test]
fn deep_nesting_and_include_fan_out_are_config_errors() {
    let deep = "a {".repeat(5000) + &"}".repeat(5000);
    assert!(Config::parse_mem(&deep).is_err());
    // A line comment ended by a KDL newline other than LF hid the nesting from the scan.
    for nl in ["\r", "\u{c}", "\u{85}", "\u{2028}", "\u{2029}"] {
        let err = format!(
            "{:?}",
            Config::parse_mem(&format!("// x{nl}{deep}")).unwrap_err()
        );
        assert!(err.contains("nested"), "{nl:?}: {err}");
    }
    let comments = "/*".repeat(100_000);
    assert!(Config::parse_mem(&comments).is_err());
    // 32 levels still parse, on a 2 MiB test thread (the error is the unknown node, not the
    // depth). Braces in strings and comments do not count.
    let ok = "a {".repeat(32) + &"}".repeat(32) + " // {{{{ \"{{{{\"";
    let err = format!("{:?}", Config::parse_mem(&ok).unwrap_err());
    assert!(!err.contains("nested"), "{err}");
    // Depth is how deep, not how many: closed blocks and comments do not add up.
    Config::parse_mem(&"label { text \"x\"; }\n".repeat(40)).unwrap();
    Config::parse_mem(&format!(
        "{}label {{ text \"x\"; }}",
        "/* c */\n".repeat(40)
    ))
    .unwrap();
    let braces = "{".repeat(100);
    Config::parse_mem(&format!("label {{ text \"{braces}\"; }}")).unwrap();
    Config::parse_mem(&format!("label {{ text r#\"\"{braces}\"#; }}")).unwrap();

    let dir = tempfile::tempdir().unwrap();
    // g0 includes g1 ten times, g1 includes g2 ten times, ...: 10^8 paths without a cap.
    for i in 0..8 {
        let body = format!("include \"g{}.kdl\"\n", i + 1).repeat(10);
        std::fs::write(dir.path().join(format!("g{i}.kdl")), body).unwrap();
    }
    std::fs::write(dir.path().join("g8.kdl"), "").unwrap();
    let start = std::time::Instant::now();
    let err = Config::load(Some(&dir.path().join("g0.kdl"))).unwrap_err();
    assert!(start.elapsed().as_secs() < 5, "took {:?}", start.elapsed());
    assert!(format!("{err:?}").contains("include"), "{err:?}");
}

/// A chain of includes stops at the recursion limit; a shorter one loads.
#[test]
fn include_chains_stop_at_the_recursion_limit() {
    let dir = tempfile::tempdir().unwrap();
    let chain = |len: usize| {
        for i in 0..len {
            let body = format!("include \"c{len}-{}.kdl\"\n", i + 1);
            std::fs::write(dir.path().join(format!("c{len}-{i}.kdl")), body).unwrap();
        }
        let last = dir.path().join(format!("c{len}-{len}.kdl"));
        std::fs::write(last, "general { fail-timeout-ms 10; }\n").unwrap();
        Config::load(Some(&dir.path().join(format!("c{len}-0.kdl"))))
    };
    assert_eq!(chain(5).unwrap().general.fail_timeout_ms, 10);
    let err = format!("{:?}", chain(13).unwrap_err());
    assert!(err.contains("recursion limit"), "{err}");
}

/// contrib/hyprlock-to-kdl turns hyprlock's own example.conf into the shipped hand translation,
/// so a converter regression or a drifting example shows up here. Colour spelling and defaults
/// spelled out on one side only do not matter: both sides are compared as decoded configs.
#[test]
fn hyprlock_converter_reproduces_the_example() {
    let root = env!("CARGO_MANIFEST_DIR");
    let out = match std::process::Command::new("python3")
        .arg(format!("{root}/contrib/hyprlock-to-kdl"))
        .arg(format!("{root}/contrib/fixtures/hyprlock-example.conf"))
        .output()
    {
        Ok(out) => out,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            eprintln!("python3 not found: skipping hyprlock_converter_reproduces_the_example");
            return;
        }
        Err(e) => panic!("cannot run the converter: {e}"),
    };
    assert!(
        out.status.success(),
        "converter failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let converted = Config::parse_mem(std::str::from_utf8(&out.stdout).unwrap())
        .unwrap_or_else(|e| panic!("converted output does not parse: {e:?}"));
    let path = std::path::PathBuf::from(format!("{root}/examples/hyprlock-example.kdl"));
    let text = std::fs::read_to_string(&path).unwrap();
    let shipped = Config::parse(&path, &text).config.unwrap();
    assert_eq!(converted, shipped);
}

#[test]
fn reload_keys_decode() {
    let config = Config::parse_mem(
        "background { reload-cmd \"echo x\"; reload-ms 300; crossfade-ms 500; }\n\
         image { path \"/a.png\"; reload-ms 0; }\n",
    )
    .unwrap();
    let b = background(&config);
    assert_eq!(b.reload.cmd.as_deref(), Some("echo x"));
    assert_eq!(b.reload.every_ms, Some(300));
    assert_eq!(b.crossfade_ms, 500);
    let Widget::Image(i) = &config.widgets[1] else {
        panic!("an image")
    };
    assert_eq!(i.reload.every_ms, Some(0));
    assert_eq!(i.reload.cmd, None);
    // Off by default.
    let plain = Config::parse_mem("background {}\nimage { path \"/a.png\"; }").unwrap();
    assert!(!background(&plain).reload.is_set());
    assert_eq!(background(&plain).crossfade_ms, 0);
}

#[test]
fn crossfade_duration_is_bounded() {
    for ms in [0, 1, 3000] {
        let c = Config::parse_mem(&format!("background {{ crossfade-ms {ms}; }}")).unwrap();
        assert_eq!(background(&c).crossfade_ms, ms);
    }
    for ms in ["-1", "3001", "4294967295", "4294967296"] {
        assert!(Config::parse_mem(&format!("background {{ crossfade-ms {ms}; }}")).is_err());
    }
}

#[test]
fn reload_commands_reject_nul() {
    for node in ["background", "image"] {
        let kdl = format!("{node} {{ reload-cmd \"echo \\u{{0}}x\"; }}");
        assert!(Config::parse_mem(&kdl).is_err(), "{kdl}");
    }
}

#[test]
fn reload_keys_are_checked() {
    for bad in [
        // crossfade belongs to the background.
        "image { path \"/a.png\"; crossfade-ms 500; }",
        // A shell command a hundred times a second is not a reload interval.
        "background { reload-ms 5; }",
        "image { path \"/a.png\"; reload-ms 99; }",
        "background { reload-cmd \"\"; }",
        "background { reload-ms -1; }",
        "background { crossfade-ms \"x\"; }",
    ] {
        assert!(Config::parse_mem(bad).is_err(), "{bad}");
    }
    assert!(Config::parse_mem("background { reload-ms 100; }").is_ok());
}

/// A relative path a reload command prints resolves against the config file's directory, like
/// `path`: the reload spec remembers that directory.
#[test]
fn reload_spec_remembers_the_config_dir() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("sub")).unwrap();
    std::fs::write(
        dir.path().join("sub/inc.kdl"),
        "image { path \"a.png\"; reload-cmd \"echo\"; }\n",
    )
    .unwrap();
    let main = dir.path().join("config.kdl");
    std::fs::write(
        &main,
        "background { reload-cmd \"echo\"; }\ninclude \"sub/inc.kdl\"\n",
    )
    .unwrap();
    let config = Config::load(Some(&main)).unwrap();
    assert_eq!(background(&config).reload.base.as_deref(), Some(dir.path()));
    let Widget::Image(i) = &config.widgets[1] else {
        panic!("an image")
    };
    assert_eq!(i.reload.base, Some(dir.path().join("sub")));
}
