//! Pango text into a cairo surface.

use pangocairo::{cairo, pango};

use crate::config::{Color, TextAlign};

/// Render `text` at `scale`. Returns `None` for empty text. Invalid markup falls back to the
/// literal text with a warning, so a typo shows up on screen instead of hiding the label.
pub fn render(
    text: &str,
    font: &str,
    color: Color,
    align: TextAlign,
    markup: bool,
    scale: f64,
) -> anyhow::Result<Option<cairo::ImageSurface>> {
    if text.is_empty() {
        return Ok(None);
    }
    let mut desc = pango::FontDescription::from_string(font);
    let size = if desc.size() > 0 {
        f64::from(desc.size()) / f64::from(pango::SCALE)
    } else {
        16.
    };
    desc.set_absolute_size(size * scale * f64::from(pango::SCALE));

    let layout_for = |cr: &cairo::Context| {
        let layout = pangocairo::functions::create_layout(cr);
        layout.set_font_description(Some(&desc));
        layout.set_alignment(match align {
            TextAlign::Left => pango::Alignment::Left,
            TextAlign::Center => pango::Alignment::Center,
            TextAlign::Right => pango::Alignment::Right,
        });
        if markup && pango::parse_markup(text, '\0').is_ok() {
            layout.set_markup(text);
        } else {
            if markup {
                tracing::warn!("label: invalid Pango markup, showing it as text: {text:?}");
            }
            layout.set_text(text);
        }
        layout
    };

    let probe = cairo::ImageSurface::create(cairo::Format::ARgb32, 1, 1)?;
    let (w, h) = layout_for(&cairo::Context::new(&probe)?).pixel_size();
    if w == 0 || h == 0 {
        return Ok(None);
    }
    let surface = cairo::ImageSurface::create(cairo::Format::ARgb32, w, h)?;
    let cr = cairo::Context::new(&surface)?;
    cr.set_source_rgba(
        color.r.into(),
        color.g.into(),
        color.b.into(),
        color.a.into(),
    );
    pangocairo::functions::show_layout(&cr, &layout_for(&cr));
    drop(cr);
    Ok(Some(surface))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_text_draws_nothing() {
        let c = Color::from_rgba8_unpremul(255, 255, 255, 255);
        assert!(render("", "Sans 12", c, TextAlign::Center, true, 1.)
            .unwrap()
            .is_none());
    }

    #[test]
    fn scale_doubles_the_size_and_markup_falls_back() {
        let c = Color::from_rgba8_unpremul(255, 255, 255, 255);
        let a = render("Hello", "Sans 20", c, TextAlign::Left, true, 1.)
            .unwrap()
            .unwrap();
        let b = render("Hello", "Sans 20", c, TextAlign::Left, true, 2.)
            .unwrap()
            .unwrap();
        assert!((f64::from(b.height()) / f64::from(a.height()) - 2.).abs() < 0.2);
        // Unclosed tag: still renders (as literal text) instead of failing.
        assert!(render("<b>oops", "Sans 20", c, TextAlign::Left, true, 1.)
            .unwrap()
            .is_some());
        let bold = render("<b>Hi</b>", "Sans 20", c, TextAlign::Left, true, 1.)
            .unwrap()
            .unwrap();
        let raw = render("<b>Hi</b>", "Sans 20", c, TextAlign::Left, false, 1.)
            .unwrap()
            .unwrap();
        assert!(raw.width() > bold.width(), "markup off shows the tags");
    }
}
