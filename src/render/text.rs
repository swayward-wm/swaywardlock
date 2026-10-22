//! Pango text into a cairo surface.

use std::borrow::Cow;

use pangocairo::{cairo, pango};

use crate::config::{Color, TextAlign};

/// Most lines a label lays out. Pango's layout time grows faster than linearly with the line
/// count and runs on the main thread, so a log-tail `cmd[]` label could stall input for minutes.
pub const MAX_LINES: usize = 100;

/// `text` up to its first [`MAX_LINES`] lines, counting every line break Pango honours: LF, CR,
/// CR LF (one break), U+2028 and U+2029. CR-separated progress output (curl, rsync, pv) got past
/// a newline-only count and laid out 32k lines.
pub fn clamp_lines(text: &str) -> &str {
    let mut breaks = 0;
    let mut chars = text.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        match c {
            '\r' => {
                chars.next_if(|&(_, c)| c == '\n');
            }
            '\n' | '\u{2028}' | '\u{2029}' => {}
            _ => continue,
        }
        breaks += 1;
        if breaks == MAX_LINES {
            return &text[..i];
        }
    }
    text
}

/// Render `text` at `scale`, at most [`MAX_LINES`] lines of it. Returns `None` for empty text.
/// Invalid markup falls back to the literal text with a warning, so a typo shows up on screen
/// instead of hiding the label.
pub fn render(
    text: &str,
    font: &str,
    color: Color,
    align: TextAlign,
    markup: bool,
    scale: f64,
) -> anyhow::Result<Option<cairo::ImageSurface>> {
    // glib asserts on interior NULs in debug builds (an abort after lock) and cuts the string at
    // the first one in release builds. Text can be cmd[] output, so anything can reach here.
    let text = clamp_lines(&strip_nul(text)).to_owned();
    let text = text.as_str();
    if text.is_empty() {
        return Ok(None);
    }
    let mut desc = pango::FontDescription::from_string(&strip_nul(font));
    let size = if desc.size() > 0 {
        f64::from(desc.size()) / f64::from(pango::SCALE)
    } else {
        16.
    };
    desc.set_absolute_size(size * scale * f64::from(pango::SCALE));

    // Parse markup once here: character references such as &#13; break lines only after
    // parsing, so the parsed text is cut too (and then shown without its attributes).
    let (shown, attrs) = match markup.then(|| pango::parse_markup(text, '\0')) {
        Some(Ok((attrs, plain, _))) if clamp_lines(&plain).len() == plain.len() => {
            (plain.to_string(), Some(attrs))
        }
        Some(Ok((_, plain, _))) => {
            tracing::warn!("label: markup text is over {MAX_LINES} lines; cut, without markup");
            (clamp_lines(&plain).to_owned(), None)
        }
        Some(Err(_)) => {
            tracing::warn!("label: invalid Pango markup, showing it as text: {text:?}");
            (text.to_owned(), None)
        }
        None => (text.to_owned(), None),
    };
    let shown = cap_tabs(shown);
    let layout_for = |cr: &cairo::Context| {
        let layout = pangocairo::functions::create_layout(cr);
        layout.set_font_description(Some(&desc));
        layout.set_alignment(match align {
            TextAlign::Left => pango::Alignment::Left,
            TextAlign::Center => pango::Alignment::Center,
            TextAlign::Right => pango::Alignment::Right,
        });
        layout.set_text(&shown);
        layout.set_attributes(attrs.as_ref());
        layout
    };

    let probe = cairo::ImageSurface::create(cairo::Format::ARgb32, 1, 1)?;
    let (w, h) = layout_for(&cairo::Context::new(&probe)?).pixel_size();
    if w == 0 || h == 0 {
        return Ok(None);
    }
    super::check_texture_size(w, h)?;
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

/// Most TABs a label lays out as tab stops. Pango's layout time grows faster than linearly with
/// the tab stops in one paragraph (one line of 16000 took about 0.5 s per layout, 32000 seconds).
pub const MAX_TABS: usize = 256;

/// `text` with every TAB after the first [`MAX_TABS`] replaced by a space. Both are one byte, so
/// Pango attribute offsets into the text stay valid.
fn cap_tabs(text: String) -> String {
    let Some((cut, _)) = text.match_indices('\t').nth(MAX_TABS) else {
        return text;
    };
    tracing::warn!("label: more than {MAX_TABS} TABs; the rest are drawn as spaces");
    let (head, tail) = text.split_at(cut);
    head.to_owned() + &tail.replace('\t', " ")
}

/// `s` without NUL characters, which no Pango or glib string may contain.
pub fn strip_nul(s: &str) -> Cow<'_, str> {
    if s.contains('\0') {
        Cow::Owned(s.replace('\0', ""))
    } else {
        Cow::Borrowed(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A NUL in text or font (cmd[] output, a config string) aborted debug builds in glib.
    #[test]
    fn interior_nul_is_dropped() {
        let c = Color::from_rgba8_unpremul(255, 255, 255, 255);
        let with = render("a\0b", "Sans\0 12", c, TextAlign::Left, true, 1.)
            .unwrap()
            .unwrap();
        let without = render("ab", "Sans 12", c, TextAlign::Left, true, 1.)
            .unwrap()
            .unwrap();
        assert_eq!(
            (with.width(), with.height()),
            (without.width(), without.height())
        );
    }

    /// Text over the GPU texture limit is refused before cairo allocates it.
    #[test]
    fn text_over_the_texture_limit_is_refused() {
        let c = Color::from_rgba8_unpremul(255, 255, 255, 255);
        let err = render("WWWW", "Sans 20000", c, TextAlign::Left, false, 1.).unwrap_err();
        assert!(err.to_string().contains("texture limit"), "{err}");
    }

    #[test]
    fn empty_text_draws_nothing() {
        let c = Color::from_rgba8_unpremul(255, 255, 255, 255);
        assert!(render("", "Sans 12", c, TextAlign::Center, true, 1.)
            .unwrap()
            .is_none());
    }

    #[test]
    fn clamp_lines_keeps_the_first_max_lines() {
        let many: String = (0..1000).map(|i| format!("{i}\n")).collect();
        let cut = clamp_lines(&many);
        assert_eq!(cut.lines().count(), MAX_LINES);
        assert!(cut.ends_with("\n99"));
        assert_eq!(clamp_lines("a\nb\n"), "a\nb\n");
    }

    /// Pango also breaks lines at CR, CR LF, U+2028 and U+2029; progress output uses CR.
    #[test]
    fn clamp_lines_counts_every_pango_line_break() {
        for sep in ["\r", "\r\n", "\u{2028}", "\u{2029}"] {
            let many: String = (0..1000).map(|i| format!("{i}{sep}")).collect();
            let cut = clamp_lines(&many);
            assert!(
                cut.ends_with(&format!("{sep}99")),
                "{sep:?}: {:?}",
                &cut[cut.len() - 9..]
            );
        }
        // CR LF is one break, LF CR two: three per item, so the 100th is the CR LF of item 33.
        let mixed: String = (0..60).map(|i| format!("{i}\r\n{i}\n\r")).collect();
        assert!(clamp_lines(&mixed).ends_with("\n32\n\r33"));
    }

    /// 32k CR-separated lines fit in cmd[]'s 64 KiB and kept the main thread in Pango.
    #[test]
    fn render_lays_out_at_most_a_hundred_lines_of_any_break() {
        let c = Color::from_rgba8_unpremul(255, 255, 255, 255);
        let draw = |t: &str, markup| {
            render(t, "Sans 10", c, TextAlign::Left, markup, 1.)
                .unwrap()
                .unwrap()
                .height()
        };
        let hundred: String = (0..100).map(|i| format!("{i}\n")).collect();
        let want = draw(hundred.trim_end(), false);
        for sep in ["\r", "\r\n", "\u{2028}", "\u{2029}"] {
            let many: String = (0..1000).map(|i| format!("{i}{sep}")).collect();
            assert_eq!(draw(&many, false), want, "{sep:?}");
        }
        // Character references break lines only after Pango parses the markup.
        let refs: String = (0..1000).map(|i| format!("{i}&#13;")).collect();
        assert_eq!(draw(&refs, true), want);
    }

    #[test]
    fn tabs_past_the_cap_become_spaces() {
        let many = "\t".repeat(MAX_TABS + 3);
        let capped = cap_tabs(many.clone());
        assert_eq!(capped.len(), many.len());
        assert_eq!(capped.matches('\t').count(), MAX_TABS);
        assert!(capped.ends_with("   "));
        assert_eq!(cap_tabs("a\tb".into()), "a\tb");
    }

    /// Pango's layout time grows faster than linearly with the TAB stops in one paragraph: one
    /// line of 32000 took seconds per layout, inside the byte and line caps. Raw TABs and markup
    /// character references both count.
    #[test]
    fn many_tabs_lay_out_quickly() {
        let c = Color::from_rgba8_unpremul(255, 255, 255, 255);
        let raw = "a\t".repeat(32000);
        let refs = "a&#9;".repeat(32000);
        for (text, markup) in [(raw.as_str(), false), (refs.as_str(), true)] {
            let start = std::time::Instant::now();
            // Wide text is then refused by the texture guard; the layout time is what matters.
            let _ = render(text, "Sans 6", c, TextAlign::Left, markup, 1.);
            let took = start.elapsed();
            assert!(took.as_millis() < 500, "markup {markup}: {took:?}");
        }
    }

    /// Substituted label text is not capped by `cmd[]`; laying out thousands of lines stalled the
    /// main thread.
    #[test]
    fn render_lays_out_at_most_a_hundred_lines() {
        let line = |i| format!("{i}\n");
        let many: String = (0..1000).map(line).collect();
        let hundred: String = (0..100).map(line).collect();
        let c = Color::from_rgba8_unpremul(255, 255, 255, 255);
        let draw = |t: &str| {
            render(t, "Sans 10", c, TextAlign::Left, false, 1.)
                .unwrap()
                .unwrap()
                .height()
        };
        assert_eq!(draw(&many), draw(hundred.trim_end()));
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
