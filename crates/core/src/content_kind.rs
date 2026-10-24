use crate::{
    Mime, MimeKind,
    mime_types::{TEXT_HTML, TEXT_MARKDOWN},
};

#[repr(u32)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ContentKind {
    Text = 1,
    Image = 1 << 1,
    File = 1 << 2,
    Password = 1 << 3,
    Markdown = 1 << 4,
    Html = 1 << 5,
    Other = 1 << 6,
}

impl ContentKind {
    pub fn from_num(n: u32) -> Option<ContentKind> {
        match n {
            1 => Some(ContentKind::Text),
            2 => Some(ContentKind::Image),
            4 => Some(ContentKind::File),
            8 => Some(ContentKind::Password),
            16 => Some(ContentKind::Markdown),
            32 => Some(ContentKind::Html),
            64 => Some(ContentKind::Other),
            _ => None,
        }
    }

    pub fn from_mime<T: AsRef<str>>(mime: &Mime<T>) -> Option<ContentKind> {
        let mime_type = mime.mime().as_ref();
        match mime.kind() {
            MimeKind::TextLike if mime_type == TEXT_MARKDOWN => Some(ContentKind::Markdown),
            MimeKind::TextLike if mime_type == TEXT_HTML => Some(ContentKind::Html),
            MimeKind::Text | MimeKind::TextExtended | MimeKind::TextLike => Some(ContentKind::Text),
            MimeKind::FileList => Some(ContentKind::File),
            MimeKind::Image => Some(ContentKind::Image),
            MimeKind::Other => Some(ContentKind::Other),
            MimeKind::Password => Some(ContentKind::Password),
            MimeKind::FileModifier => None,
        }
    }
}

#[derive(Clone, Copy)]
pub struct ContentKinds(u32);

impl ContentKinds {
    pub fn of<'a, T: AsRef<str> + 'a>(types: impl Iterator<Item = &'a Mime<T>>) -> Self {
        let mut result = ContentKinds(0);

        for t in types {
            if let Some(kind) = ContentKind::from_mime(t) {
                result.0 |= kind as u32;
            }
        }

        result
    }

    pub fn has(&self, kind: ContentKind) -> bool {
        self.0 & kind as u32 != 0
    }

    pub fn iter(&self) -> impl Iterator<Item = ContentKind> {
        ContentKindsIterator { val: *self, pos: 1 }
    }
}

pub struct ContentKindsIterator {
    val: ContentKinds,
    pos: u32,
}

impl Iterator for ContentKindsIterator {
    type Item = ContentKind;

    fn next(&mut self) -> Option<Self::Item> {
        while self.pos <= self.val.0 {
            let current = self.pos;
            self.pos <<= 1;

            if self.val.0 & current != 0 {
                return ContentKind::from_num(current);
            }
        }
        None
    }
}
