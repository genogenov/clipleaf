use std::io::BufRead;

use crate::mime_types::{GNOME_COPIED_FILES, KDE_URI_LIST, TEXT_URI_LIST};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileListFormat {
    UriList,
    GnomeCopiedFiles,
}

impl FileListFormat {
    pub fn from_mime(mime: &str) -> Option<Self> {
        match mime {
            TEXT_URI_LIST | KDE_URI_LIST => Some(FileListFormat::UriList),
            GNOME_COPIED_FILES => Some(FileListFormat::GnomeCopiedFiles),
            _ => None,
        }
    }
}

pub struct FileListScan {
    format: FileListFormat,
    line: Line,
    first_line: bool,
    items: u32,
}

#[derive(PartialEq, Eq)]
enum Line {
    Start,
    Item,
    Skip,
}

impl FileListScan {
    pub fn new(format: FileListFormat) -> Self {
        Self {
            format,
            line: Line::Start,
            first_line: true,
            items: 0,
        }
    }
    pub fn feed(&mut self, bytes: &[u8]) {
        let mut rest = bytes;
        loop {
            match self.line {
                Line::Start => {
                    if rest.is_empty() || rest[0] == b'\n' || rest[0] == b'\r' {
                        if rest.len() < 2 {
                            return;
                        }
                        rest = &rest[1..];
                    } else if (rest[0] == b'#' && self.format == FileListFormat::UriList)
                        || (self.first_line && self.format == FileListFormat::GnomeCopiedFiles)
                    {
                        self.line = Line::Skip;
                    } else {
                        self.line = Line::Item
                    }

                    if self.first_line && (self.line == Line::Skip || self.line == Line::Item) {
                        self.first_line = false;
                    }
                }
                Line::Item => {
                    let Some(n) = find_newline(rest) else {
                        return;
                    };

                    self.items = self.items.saturating_add(1);
                    self.line = Line::Start;
                    rest = &rest[n + 1..];
                }
                Line::Skip => {
                    let Some(n) = find_newline(rest) else {
                        return;
                    };
                    self.line = Line::Start;
                    rest = &rest[n + 1..];
                }
            }
        }
    }
    pub fn finish(self) -> u32 {
        let mut result = self.items;
        if self.line == Line::Item {
            result = self.items.saturating_add(1);
        }

        result
    }
}

fn find_newline(s: &[u8]) -> Option<usize> {
    let mut r = s;
    let n = r.skip_until(b'\n').unwrap_or(0); // can't fail on &[u8]
    (n > 0 && s[n - 1] == b'\n').then(|| n - 1)
}
