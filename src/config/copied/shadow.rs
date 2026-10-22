//! Shadow config, copied from swayward-config `appearance/border.rs` (`Shadow`, `ShadowOffset`,
//! `ShadowRule`; the border, focus-ring and workspace-shadow types are not copied).

use super::color::Color;
use super::utils::{FloatOrInt, MergeWith};

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Shadow {
    pub on: bool,
    pub offset: ShadowOffset,
    pub softness: f64,
    pub spread: f64,
    pub draw_behind_window: bool,
    pub color: Color,
    pub inactive_color: Option<Color>,
}

impl Default for Shadow {
    fn default() -> Self {
        Self {
            on: false,
            offset: ShadowOffset {
                x: FloatOrInt(0.),
                y: FloatOrInt(5.),
            },
            softness: 30.,
            spread: 5.,
            draw_behind_window: false,
            color: Color::from_rgba8_unpremul(0, 0, 0, 0x77),
            inactive_color: None,
        }
    }
}

impl MergeWith<ShadowRule> for Shadow {
    fn merge_with(&mut self, part: &ShadowRule) {
        self.on |= part.on;
        if part.off {
            self.on = false;
        }

        merge!((self, part), softness, spread);

        merge_clone!((self, part), offset, draw_behind_window, color);

        merge_clone_opt!((self, part), inactive_color);
    }
}

#[derive(knuffel::Decode, Debug, Clone, Copy, PartialEq)]
pub struct ShadowOffset {
    #[knuffel(property, default)]
    pub x: FloatOrInt<-65535, 65535>,
    #[knuffel(property, default)]
    pub y: FloatOrInt<-65535, 65535>,
}

#[derive(knuffel::Decode, Debug, Default, Clone, Copy, PartialEq)]
pub struct ShadowRule {
    #[knuffel(child)]
    pub off: bool,
    #[knuffel(child)]
    pub on: bool,
    #[knuffel(child)]
    pub offset: Option<ShadowOffset>,
    #[knuffel(child, unwrap(argument))]
    pub softness: Option<FloatOrInt<0, 1024>>,
    #[knuffel(child, unwrap(argument))]
    pub spread: Option<FloatOrInt<-1024, 1024>>,
    #[knuffel(child, unwrap(argument))]
    pub draw_behind_window: Option<bool>,
    #[knuffel(child)]
    pub color: Option<Color>,
    #[knuffel(child)]
    pub inactive_color: Option<Color>,
}
