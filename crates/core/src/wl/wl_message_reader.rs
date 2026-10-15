pub struct WlMessageReader<'a>(&'a [u8]);

impl<'a> WlMessageReader<'a> {
    pub fn new(body: &'a [u8]) -> Self {
        Self(body)
    }

    pub fn u32(&mut self) -> Option<u32> {
        let (chunk, rest) = self.0.split_first_chunk::<4>()?;
        self.0 = rest;

        let value = u32::from_ne_bytes(*chunk);
        Some(value)
    }

    pub fn str(&mut self) -> Option<&'a [u8]> {
        let (chunk, rest) = self.0.split_first_chunk::<4>()?;

        let length = u32::from_ne_bytes(*chunk) as usize;
        if length == 0 {
            return None;
        }

        let padded = length.checked_add(3)? & !3;
        let (str_chunk, rest) = rest.split_at_checked(padded)?;

        if str_chunk[length - 1] != 0 {
            return None;
        }

        self.0 = rest;

        Some(&str_chunk[..length - 1])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn push_wl_str(out: &mut Vec<u8>, s: &[u8]) {
        out.extend_from_slice(&((s.len() + 1) as u32).to_ne_bytes());
        out.extend_from_slice(s);
        out.push(0);
        out.resize(out.len().next_multiple_of(4), 0);
    }

    #[test]
    fn u32_reads_in_order_and_stops_at_end() {
        let mut body = Vec::new();
        body.extend_from_slice(&1u32.to_ne_bytes());
        body.extend_from_slice(&2u32.to_ne_bytes());
        body.extend_from_slice(&[0xff, 0xff]);
        let mut r = WlMessageReader::new(&body);
        assert_eq!(r.u32(), Some(1));
        assert_eq!(r.u32(), Some(2));
        assert_eq!(r.u32(), None);
    }

    #[test]
    fn str_handles_every_padding_length() {
        let text = b"abcdefgh";
        for n in 0..=text.len() {
            let mut body = Vec::new();
            push_wl_str(&mut body, &text[..n]);
            body.extend_from_slice(&0xdead_beefu32.to_ne_bytes());
            let mut r = WlMessageReader::new(&body);
            assert_eq!(r.str(), Some(&text[..n]), "len {n}");
            // Lands exactly after the padding.
            assert_eq!(r.u32(), Some(0xdead_beef), "len {n}");
            assert_eq!(r.u32(), None);
        }
    }

    #[test]
    fn str_rejects_null_string() {
        let body = 0u32.to_ne_bytes();
        assert_eq!(WlMessageReader::new(&body).str(), None);
    }

    #[test]
    fn str_rejects_truncated_body() {
        let mut body = Vec::new();
        push_wl_str(&mut body, b"hello");
        assert_eq!(WlMessageReader::new(&body[..body.len() - 1]).str(), None);
        assert_eq!(WlMessageReader::new(&body[..3]).str(), None);
    }

    #[test]
    fn str_rejects_missing_nul() {
        let mut body = Vec::new();
        body.extend_from_slice(&4u32.to_ne_bytes());
        body.extend_from_slice(b"abcd");
        assert_eq!(WlMessageReader::new(&body).str(), None);
    }

    #[test]
    fn str_rejects_huge_length() {
        let mut body = Vec::new();
        body.extend_from_slice(&u32::MAX.to_ne_bytes());
        body.extend_from_slice(b"abc\0");
        assert_eq!(WlMessageReader::new(&body).str(), None);
    }
}
