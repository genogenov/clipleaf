use std::{borrow::Cow, cell::Ref};

use clipleaf_ipc::protocol::v1::models::{EntrySummary, Kind, ThumbState, TypeSummary};
use gtk4::{
    gdk::{Paintable, Texture},
    glib,
    prelude::{GridExt, WidgetExt},
};
use percent_encoding::percent_decode;
use unicode_segmentation::UnicodeSegmentation;

const THUMB_WIDTH: i32 = 200;
const THUMB_HEIGHT: i32 = 200;
const MAX_TEXT_LINES: i32 = 7;
const MAX_TEXT_CHARS: usize = 1000;

pub struct RowWidgets {
    pub root: gtk4::Grid,
    pub thumb: gtk4::Picture,
    pub text: gtk4::Label,
    pub meta_text: gtk4::Label,
}

impl RowWidgets {
    pub fn new() -> Self {
        let thumb = gtk4::Picture::builder()
            .can_shrink(true)
            .css_classes(["clipleaf-list-item-thumb"])
            .content_fit(gtk4::ContentFit::Contain)
            .width_request(THUMB_WIDTH)
            .height_request(THUMB_HEIGHT)
            .build();
        let text = gtk4::Label::builder()
            .css_classes(["clipleaf-list-item-text"])
            .wrap(true)
            .wrap_mode(gtk4::pango::WrapMode::WordChar)
            .lines(MAX_TEXT_LINES)
            .ellipsize(gtk4::pango::EllipsizeMode::End)
            .xalign(0.0)
            .hexpand(true)
            .build();
        let meta_text = gtk4::Label::builder().css_classes(["clipleaf-list-item-meta"]).xalign(0.0).build();

        let root = gtk4::Grid::builder()
            .css_classes(["clipleaf-list-row"])
            .hexpand(true)
            .orientation(gtk4::Orientation::Horizontal)
            .build();
        root.attach(&thumb, 0, 0, 1, 2);
        root.attach(&text, 1, 0, 1, 1);
        root.attach(&meta_text, 1, 1, 1, 1);

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
                self.meta_text.set_label(&self.meta_text_type(preferred_type));
                self.thumb.set_paintable(None::<&Paintable>);
                self.thumb.set_visible(false);
            }
            Kind::Image => {
                self.text.set_label("");
                self.thumb.set_visible(true);
                self.meta_text.set_label(&self.meta_image_type(preferred_type));
                let Some(ref t) = preferred_type.thumb else {
                    self.thumb.set_css_classes(&["clipleaf-list-item-thumb", "unavailable"]);
                    self.thumb.set_paintable(None::<&Paintable>);
                    return;
                };

                if t.state == ThumbState::Pending {
                    self.thumb.set_css_classes(&["clipleaf-list-item-thumb", "pending"]);
                    self.thumb.set_paintable(None::<&Paintable>);
                } else if t.state == ThumbState::Unavailable {
                    self.thumb.set_css_classes(&["clipleaf-list-item-thumb", "unavailable"]);
                    self.thumb.set_paintable(None::<&Paintable>);
                } else {
                    match Texture::from_bytes(&glib::Bytes::from(&t.data.as_deref().unwrap_or(&[]))) {
                        Ok(texture) => {
                            self.thumb.set_css_classes(&["clipleaf-list-item-thumb"]);
                            self.thumb.set_paintable(Some(&texture));
                        }
                        Err(_) => {
                            self.thumb.set_css_classes(&["clipleaf-list-item-thumb", "unavailable"]);
                            self.thumb.set_paintable(None::<&Paintable>);
                        }
                    }
                }
            }
            Kind::File => {
                self.text.set_label(&self.display_text_files(&preferred_type.preview));
                self.meta_text.set_label(&self.meta_file_type(preferred_type));
                self.thumb.set_paintable(None::<&Paintable>);
                self.thumb.set_visible(false);
            }
            Kind::Other => {
                self.text.set_label("");
                self.thumb.set_paintable(None::<&Paintable>);
                self.thumb.set_visible(false);
                let (unit, size) = get_unit_size(preferred_type.size);
                self.text.set_label(&preferred_type.mime);
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
            Some(t) => format!(
                "{} · {}x{} · {} {}",
                format,
                t.src_width.unwrap_or_default(),
                t.src_height.unwrap_or_default(),
                size,
                unit
            ),
            None => format!("{} {}", size, unit),
        }
    }

    fn display_text(&self, bytes: &[u8]) -> String {
        let text = decode_utf8(bytes);
        let trimmed = text.trim_start_matches(['\n', '\r']);
        limit_max_chars(trimmed)
    }

    fn display_text_files(&self, bytes: &[u8]) -> String {
        let mut names = Vec::new();
        // +1 for GNOME copy/cut line, and another +1 so Pango sees extra content and ellipsizes.
        for line in bytes.split(|&b| b == b'\n').take(MAX_TEXT_LINES as usize + 2) {
            let line = line.strip_suffix(b"\r").unwrap_or(line);
            if line.is_empty() || line == b"copy" || line == b"cut" {
                continue;
            }
            if !names.is_empty() {
                names.push(b'\n');
            }
            names.extend(percent_decode(line.strip_prefix(b"file://").unwrap_or(line)));
        }
        limit_max_chars(&decode_utf8(&names))
    }
}

fn limit_max_chars(s: &str) -> String {
    let mut result = String::new();

    let mut lines = 1;
    for c in s.graphemes(true).take(MAX_TEXT_CHARS) {
        let is_break = match c {
            "\r\n" | "\r" | "\n" | "\u{2029}" => true,
            _ => {
                result.push_str(c);
                false
            }
        };
        if is_break {
            if lines == MAX_TEXT_LINES as usize + 1 {
                break;
            }
            lines += 1;
            result.push('\u{2028}');
        }
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

fn get_unit_size(size: u64) -> (&'static str, u64) {
    if size < 1024 {
        return ("B", size);
    }
    let size = size / 1024;
    if size < 1024 {
        return ("KB", size);
    }
    let size = size / 1024;
    if size < 1024 {
        return ("MB", size);
    }
    let size = size / 1024;
    if size < 1024 {
        return ("GB", size);
    }
    let size = size / 1024;
    ("TB", size)
}
