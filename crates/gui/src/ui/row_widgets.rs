use std::{borrow::Cow, cell::Ref};

use clipleaf_ipc::protocol::v1::models::{EntrySummary, Kind, ThumbState, TypeSummary};
use gtk4::{
    gdk::{Paintable, Texture},
    glib, pango,
    prelude::{BoxExt, WidgetExt},
};
use percent_encoding::percent_decode;
use unicode_segmentation::UnicodeSegmentation;

const MAX_TEXT_LINES: i32 = 7;
const MAX_THUMB_HEIGHT: u32 = 200;
const THUMB_PLACEHOLDER: i32 = 64;
const MAX_TEXT_CHARS: usize = 700;

pub struct RowWidgets {
    pub root: gtk4::Box,
    pub thumb: gtk4::Picture,
    pub text: gtk4::Label,
    pub meta_text: gtk4::Label,
}

impl RowWidgets {
    pub fn new() -> Self {
        let thumb = gtk4::Picture::builder()
            .can_shrink(true)
            .css_classes(["clipleaf-list-item-thumb"])
            .content_fit(gtk4::ContentFit::ScaleDown)
            .halign(gtk4::Align::Center)
            .build();

        let attrs = pango::AttrList::new();
        attrs.insert(pango::AttrInt::new_insert_hyphens(false));

        let text = gtk4::Label::builder()
            .css_classes(["clipleaf-list-item-text"])
            .wrap(true)
            .wrap_mode(gtk4::pango::WrapMode::WordChar)
            .lines(MAX_TEXT_LINES)
            .ellipsize(gtk4::pango::EllipsizeMode::End)
            .xalign(0.0)
            .hexpand(true)
            .attributes(&attrs)
            .build();
        let meta_text = gtk4::Label::builder()
            .css_classes(["clipleaf-list-item-meta"])
            .xalign(0.0)
            .ellipsize(gtk4::pango::EllipsizeMode::End)
            .attributes(&attrs)
            .build();

        let root = gtk4::Box::builder()
            .css_classes(["clipleaf-list-row"])
            .hexpand(true)
            .orientation(gtk4::Orientation::Vertical)
            .build();
        root.append(&thumb);
        root.append(&text);
        root.append(&meta_text);

        Self {
            root,
            thumb,
            text,
            meta_text,
        }
    }

    pub fn fill(&self, entry: &Ref<EntrySummary>) {
        let Some(preferred_type) = entry.types.first() else {
            self.clear();
            return;
        };
        match preferred_type.kind {
            Kind::Text | Kind::Html | Kind::Markdown | Kind::Password => {
                self.text.set_label(&self.display_text(&preferred_type.preview));
                self.text.set_visible(true);
                self.meta_text.set_label(&self.meta_text_type(preferred_type));
                self.thumb.set_paintable(None::<&Paintable>);
                self.thumb.set_visible(false);
            }
            Kind::Image => {
                self.text.set_label("");
                self.text.set_visible(false);
                self.thumb.set_visible(true);
                self.meta_text.set_label(&self.meta_image_type(preferred_type));
                let Some(ref t) = preferred_type.thumb else {
                    self.thumb.set_css_classes(&["clipleaf-list-item-thumb", "unavailable"]);
                    self.thumb.set_paintable(None::<&Paintable>);
                    self.thumb.set_size_request(THUMB_PLACEHOLDER, THUMB_PLACEHOLDER);
                    return;
                };

                if t.state == ThumbState::Pending {
                    self.thumb.set_size_request(THUMB_PLACEHOLDER, THUMB_PLACEHOLDER);
                    self.thumb.set_css_classes(&["clipleaf-list-item-thumb", "pending"]);
                    self.thumb.set_paintable(None::<&Paintable>);
                } else if t.state == ThumbState::Unavailable {
                    self.thumb.set_size_request(THUMB_PLACEHOLDER, THUMB_PLACEHOLDER);
                    self.thumb.set_css_classes(&["clipleaf-list-item-thumb", "unavailable"]);
                    self.thumb.set_paintable(None::<&Paintable>);
                } else {
                    match Texture::from_bytes(&glib::Bytes::from(&t.data.as_deref().unwrap_or(&[]))) {
                        Ok(texture) => {
                            let h = t.height.map_or(-1, |h| h.min(MAX_THUMB_HEIGHT) as i32);
                            self.thumb.set_size_request(-1, h);
                            self.thumb.set_css_classes(&["clipleaf-list-item-thumb"]);
                            self.thumb.set_paintable(Some(&texture));
                        }
                        Err(_) => {
                            self.thumb.set_size_request(THUMB_PLACEHOLDER, THUMB_PLACEHOLDER);
                            self.thumb.set_css_classes(&["clipleaf-list-item-thumb", "unavailable"]);
                            self.thumb.set_paintable(None::<&Paintable>);
                        }
                    }
                }
            }
            Kind::File => {
                self.text.set_label(&self.display_text_files(&preferred_type.preview));
                self.text.set_visible(true);
                self.meta_text.set_label(&self.meta_file_type(preferred_type));
                self.thumb.set_paintable(None::<&Paintable>);
                self.thumb.set_visible(false);
            }
            Kind::Other => {
                self.thumb.set_paintable(None::<&Paintable>);
                self.thumb.set_visible(false);
                let (unit, size) = get_unit_size(preferred_type.size);
                self.text.set_label(&preferred_type.mime);
                self.text.set_visible(true);
                self.meta_text.set_label(&format!("{} {}", size, unit));
            }
        }
    }

    pub fn clear(&self) {
        // log_debug!("Clearing");
        self.thumb.set_paintable(None::<&Paintable>);
        self.thumb.set_visible(false);
        self.text.set_label("");
        self.meta_text.set_label("");
    }

    fn meta_text_type(&self, type_summary: &TypeSummary) -> String {
        let byte_size = type_summary.size;
        let (unit, size) = get_unit_size(byte_size);

        format!("Text · {} {}", size, unit)
    }

    fn meta_file_type(&self, type_summary: &TypeSummary) -> String {
        match type_summary.file_count {
            Some(1) => "1 file".into(),
            Some(n) => format!("{n} files"),
            None => "Files".into(),
        }
    }

    fn meta_image_type(&self, type_summary: &TypeSummary) -> String {
        let byte_size = type_summary.size;
        let format = &type_summary.mime;

        let (unit, size) = get_unit_size(byte_size);
        match &type_summary.thumb {
            Some(t) => {
                if let Some(t_width) = t.src_width
                    && let Some(t_height) = t.src_height
                {
                    format!("{} · {}x{} · {} {}", format, t_width, t_height, size, unit)
                } else {
                    format!("{} · {} {}", format, size, unit)
                }
            }
            None => format!("{} {}", size, unit),
        }
    }

    fn display_text(&self, bytes: &[u8]) -> String {
        let text = decode_utf8(bytes);
        let trimmed = text.trim_start_matches(['\n', '\r']);
        limit_max_chars_and_lines(trimmed)
    }

    fn display_text_files(&self, bytes: &[u8]) -> String {
        let mut names = Vec::new();
        // +1 for GNOME copy/cut line and +1 so we can see there is more and elipsize.
        for line in bytes.split(|&b| b == b'\n').take(MAX_TEXT_LINES as usize + 2) {
            let line = line.strip_suffix(b"\r").unwrap_or(line);
            if line.is_empty() || line == b"copy" || line == b"cut" || line.starts_with(b"#") {
                continue;
            }
            if !names.is_empty() {
                names.push(b'\n');
            }
            names.extend(percent_decode(line.strip_prefix(b"file://").unwrap_or(line)));
        }
        limit_max_chars_and_lines(&decode_utf8(&names))
    }
}

fn limit_max_chars_and_lines(s: &str) -> String {
    let s = s.trim_end_matches(['\n', '\r', '\u{2029}']);
    let mut result = String::new();
    let mut lines = 1;
    let mut graphemes = s.graphemes(true);
    for g in graphemes.by_ref().take(MAX_TEXT_CHARS) {
        if matches!(g, "\r\n" | "\r" | "\n" | "\u{2029}") {
            if lines == MAX_TEXT_LINES as usize {
                result.push('…');
                return result;
            }
            lines += 1;
        }
        result.push_str(if g == "\0" { "\u{FFFD}" } else { g });
    }
    if graphemes.next().is_some() {
        result.push('…');
    }
    result
}

fn decode_utf8(bytes: &[u8]) -> Cow<'_, str> {
    match str::from_utf8(bytes) {
        Ok(s) => Cow::Borrowed(s),
        Err(e) if e.error_len().is_none() => Cow::Borrowed(str::from_utf8(&bytes[..e.valid_up_to()]).unwrap_or_default()),
        Err(_) => String::from_utf8_lossy(bytes),
    }
}

const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];

fn get_unit_size(size: u64) -> (&'static str, String) {
    let mut unit = 0;

    let mut size_f = size as f64;

    while size_f >= 1024f64 && unit < UNITS.len() - 1 {
        size_f /= 1024f64;
        unit += 1;
    }

    let s_round = size_f.round();
    let size_str = {
        if s_round < 10f64 && unit > 0 {
            format!("{:.1}", size_f)
        } else {
            format!("{:.0}", s_round)
        }
    };

    (UNITS[unit], size_str)
}

#[cfg(test)]
mod tests {
    use std::borrow::Cow;

    use super::*;

    #[test]
    fn line_breaks_are_kept_as_they_are_and_trailing_ones_dropped() {
        // Each break starts a Pango paragraph, and `Label::lines` caps each one. "\r\n" is one
        // grapheme, one break.
        assert_eq!(limit_max_chars_and_lines("a\r\nb\rc\nd\u{2029}e"), "a\r\nb\rc\nd\u{2029}e");
        // A trailing break would show as an empty last line.
        assert_eq!(limit_max_chars_and_lines("one\ntwo\r\n\n"), "one\ntwo");
    }

    #[test]
    fn text_past_the_line_cap_is_cut_after_the_last_line_with_an_ellipsis() {
        let lines = |n: usize| (1..=n).map(|i| format!("line {i}")).collect::<Vec<_>>().join("\n");
        let cap = MAX_TEXT_LINES as usize;
        assert_eq!(limit_max_chars_and_lines(&lines(30)), format!("{}…", lines(cap)));
        assert_eq!(limit_max_chars_and_lines(&lines(cap + 1)), format!("{}…", lines(cap)));
        // Exactly at the cap: everything, no ellipsis.
        assert_eq!(limit_max_chars_and_lines(&lines(cap)), lines(cap));
    }

    #[test]
    fn text_past_the_character_cap_is_cut_with_an_ellipsis_never_inside_a_grapheme() {
        // "e" + combining acute: two chars, one grapheme. A family emoji: five chars, one grapheme.
        for g in ["e\u{301}", "👨‍👩‍👧"] {
            let shown = limit_max_chars_and_lines(&g.repeat(MAX_TEXT_CHARS + 500));
            assert_eq!(shown, format!("{}…", g.repeat(MAX_TEXT_CHARS)));
            // Exactly at the cap: everything, no ellipsis.
            assert_eq!(limit_max_chars_and_lines(&g.repeat(MAX_TEXT_CHARS)), g.repeat(MAX_TEXT_CHARS));
        }
    }

    #[test]
    fn a_nul_anywhere_becomes_a_replacement_character() {
        // GTK labels are C strings: a NUL aborts debug builds (glib checks for interior NULs)
        // and cuts the label in release. `find -print0`, Windows-style text, UTF-16.
        assert_eq!(limit_max_chars_and_lines("a\0b\0c"), "a\u{fffd}b\u{fffd}c");
        assert_eq!(limit_max_chars_and_lines("\0start"), "\u{fffd}start");
        assert_eq!(limit_max_chars_and_lines("end\0"), "end\u{fffd}");
        assert!(!limit_max_chars_and_lines(&"x\0".repeat(MAX_TEXT_CHARS)).contains('\0'));
    }

    #[test]
    fn a_head_cut_mid_character_drops_the_half_and_invalid_bytes_are_replaced() {
        assert!(matches!(decode_utf8("ok ✓".as_bytes()), Cow::Borrowed("ok ✓")));
        // The 5 KiB head ended inside "✓" (3 bytes): only the whole characters before it.
        let cut = &"ok ✓".as_bytes()[..4];
        assert_eq!(decode_utf8(cut), "ok ");
        // Not UTF-8 in the middle: shown with U+FFFD, the rest kept.
        assert_eq!(decode_utf8(b"a\xffb"), "a\u{fffd}b");
    }
}
