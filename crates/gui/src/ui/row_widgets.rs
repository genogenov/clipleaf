use std::cell::Ref;

use clipleaf_core::log_debug;
use clipleaf_ipc::protocol::v1::models::{EntrySummary, Kind, ThumbState};
use gtk4::{
    gdk::{Paintable, Texture},
    glib,
    prelude::{BoxExt, GridExt, WidgetExt},
};
use unicode_segmentation::UnicodeSegmentation;

const THUMB_WIDTH: i32 = 200;
const THUMB_HEIGHT: i32 = 200;
const MAX_TEXT_LINES: i32 = 10;
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
        // log_debug!("Filling");
        match entry.kind {
            Kind::Text | Kind::Html | Kind::Markdown | Kind::Password => {
                self.text.set_label(&self.display_text(&entry.preview));
                self.meta_text.set_label(&self.meta_text_type(entry));
                self.thumb.set_paintable(None::<&Paintable>);
                self.thumb.set_visible(false);
            }
            Kind::Image => {
                self.text.set_label("");
                self.thumb.set_visible(true);
                self.meta_text.set_label(&self.meta_image_type(entry));
                let Some(ref t) = entry.thumb else {
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
                self.text.set_label(&self.display_text(&entry.preview));
                self.meta_text.set_label(&self.meta_file_type(entry));
                self.thumb.set_paintable(None::<&Paintable>);
                self.thumb.set_visible(false);
            }
            Kind::Other => {
                self.text.set_label("");
                self.thumb.set_paintable(None::<&Paintable>);
                self.thumb.set_visible(false);
                match entry.types.first() {
                    Some(first_type) => {
                        let (unit, size) = get_unit_size(first_type.size);
                        self.text.set_label(&first_type.mime);
                        self.meta_text.set_label(&format!("{} {}", size, unit));
                    }
                    None => {
                        self.text.set_label("");
                        self.meta_text.set_label("");
                    }
                }
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

    fn meta_text_type(&self, entry: &EntrySummary) -> String {
        let Some(first_type) = entry.types.first() else {
            return "".to_owned();
        };
        let byte_size = first_type.size;
        let (unit, size) = get_unit_size(byte_size);

        format!("Text · {} {}", size, unit)
    }

    fn meta_file_type(&self, entry: &EntrySummary) -> String {
        format!("{} files", entry.file_count.unwrap_or_default())
    }

    fn meta_image_type(&self, entry: &EntrySummary) -> String {
        let Some(first_type) = entry.types.first() else {
            return "".to_owned();
        };
        let byte_size = first_type.size;
        let format = &first_type.mime;

        let (unit, size) = get_unit_size(byte_size);
        match &entry.thumb {
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
        let text = match str::from_utf8(bytes) {
            Ok(txt) => txt,
            Err(e) if e.error_len() == None => str::from_utf8(&bytes[..e.valid_up_to()]).unwrap_or_default(),
            Err(_) => &String::from_utf8_lossy(bytes),
        };

        let trimmed = text.trim_start_matches(['\n', '\r']);
        let mut result = String::new();
        let mut iter = trimmed.graphemes(true).take(MAX_TEXT_CHARS);

        let mut lines = 1;
        while let Some(c) = iter.next() {
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
    return ("TB", size);
}
