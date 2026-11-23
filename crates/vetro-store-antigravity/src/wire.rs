//! Schema-less protobuf wire decoding. Every accessor is lenient: malformed input yields
//! nothing rather than an error, because the schema is not published and may change.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Value<'a> {
    Varint(u64),
    Fixed64(u64),
    Bytes(&'a [u8]),
    Fixed32(u32),
}

#[derive(Debug, Clone, Copy)]
pub struct Msg<'a>(pub &'a [u8]);

pub struct Fields<'a> {
    buf: &'a [u8],
}

fn varint(buf: &mut &[u8]) -> Option<u64> {
    let mut out = 0u64;
    for shift in (0..64).step_by(7) {
        let (&b, rest) = buf.split_first()?;
        *buf = rest;
        out |= u64::from(b & 0x7f) << shift;
        if b & 0x80 == 0 {
            return Some(out);
        }
    }
    None
}

fn take<'a>(buf: &mut &'a [u8], n: usize) -> Option<&'a [u8]> {
    if buf.len() < n {
        return None;
    }
    let (head, rest) = buf.split_at(n);
    *buf = rest;
    Some(head)
}

impl<'a> Iterator for Fields<'a> {
    type Item = (u32, Value<'a>);

    fn next(&mut self) -> Option<Self::Item> {
        if self.buf.is_empty() {
            return None;
        }
        let parsed = (|| {
            let key = varint(&mut self.buf)?;
            let num = u32::try_from(key >> 3).ok().filter(|n| *n > 0)?;
            let value = match key & 7 {
                0 => Value::Varint(varint(&mut self.buf)?),
                1 => Value::Fixed64(u64::from_le_bytes(take(&mut self.buf, 8)?.try_into().ok()?)),
                2 => {
                    let len = usize::try_from(varint(&mut self.buf)?).ok()?;
                    Value::Bytes(take(&mut self.buf, len)?)
                }
                5 => Value::Fixed32(u32::from_le_bytes(take(&mut self.buf, 4)?.try_into().ok()?)),
                _ => return None,
            };
            Some((num, value))
        })();
        if parsed.is_none() {
            self.buf = &[];
        }
        parsed
    }
}

impl<'a> Msg<'a> {
    pub fn fields(self) -> Fields<'a> {
        Fields { buf: self.0 }
    }

    /// True when the whole buffer parses as a sequence of well-formed fields.
    pub fn is_valid(self) -> bool {
        let mut buf = self.0;
        while !buf.is_empty() {
            let Some(key) = varint(&mut buf) else {
                return false;
            };
            if key >> 3 == 0 {
                return false;
            }
            let ok = match key & 7 {
                0 => varint(&mut buf).is_some(),
                1 => take(&mut buf, 8).is_some(),
                2 => varint(&mut buf)
                    .and_then(|n| usize::try_from(n).ok())
                    .and_then(|n| take(&mut buf, n))
                    .is_some(),
                5 => take(&mut buf, 4).is_some(),
                _ => false,
            };
            if !ok {
                return false;
            }
        }
        true
    }

    pub fn get(self, num: u32) -> Option<Value<'a>> {
        self.fields().find(|(n, _)| *n == num).map(|(_, v)| v)
    }

    pub fn all(self, num: u32) -> impl Iterator<Item = Value<'a>> {
        self.fields()
            .filter(move |(n, _)| *n == num)
            .map(|(_, v)| v)
    }

    pub fn bytes(self, num: u32) -> Option<&'a [u8]> {
        match self.get(num)? {
            Value::Bytes(b) => Some(b),
            _ => None,
        }
    }

    pub fn msg(self, num: u32) -> Option<Msg<'a>> {
        self.bytes(num).map(Msg)
    }

    pub fn msgs(self, num: u32) -> impl Iterator<Item = Msg<'a>> {
        self.all(num).filter_map(|v| match v {
            Value::Bytes(b) => Some(Msg(b)),
            _ => None,
        })
    }

    pub fn str(self, num: u32) -> Option<&'a str> {
        self.bytes(num).and_then(|b| std::str::from_utf8(b).ok())
    }

    /// A non-empty string field.
    pub fn text(self, num: u32) -> Option<&'a str> {
        self.str(num).filter(|s| !s.is_empty())
    }

    pub fn uint(self, num: u32) -> Option<u64> {
        match self.get(num)? {
            Value::Varint(v) | Value::Fixed64(v) => Some(v),
            Value::Fixed32(v) => Some(u64::from(v)),
            Value::Bytes(_) => None,
        }
    }

    pub fn int(self, num: u32) -> Option<i64> {
        self.uint(num).map(|v| v as i64)
    }

    pub fn bool(self, num: u32) -> bool {
        self.uint(num).is_some_and(|v| v != 0)
    }

    /// Follow a path of nested message fields.
    pub fn path(self, path: &[u32]) -> Option<Msg<'a>> {
        path.iter().try_fold(self, |m, n| m.msg(*n))
    }
}

/// `google.protobuf.Timestamp` (seconds = 1, nanos = 2) as milliseconds since the epoch.
pub fn timestamp_ms(m: Msg<'_>) -> Option<i64> {
    let secs = m.int(1)?;
    if secs <= 0 {
        return None;
    }
    Some(secs * 1000 + m.int(2).unwrap_or(0) / 1_000_000)
}

#[cfg(test)]
pub mod enc {
    //! Minimal encoder for synthetic fixtures.

    #[derive(Default, Clone)]
    pub struct Enc(pub Vec<u8>);

    fn varint(out: &mut Vec<u8>, mut v: u64) {
        while v >= 0x80 {
            out.push((v as u8) | 0x80);
            v >>= 7;
        }
        out.push(v as u8);
    }

    impl Enc {
        pub fn new() -> Self {
            Self::default()
        }

        pub fn uint(mut self, num: u32, v: u64) -> Self {
            varint(&mut self.0, u64::from(num) << 3);
            varint(&mut self.0, v);
            self
        }

        pub fn bytes(mut self, num: u32, b: &[u8]) -> Self {
            varint(&mut self.0, (u64::from(num) << 3) | 2);
            varint(&mut self.0, b.len() as u64);
            self.0.extend_from_slice(b);
            self
        }

        pub fn str(self, num: u32, s: &str) -> Self {
            self.bytes(num, s.as_bytes())
        }

        pub fn msg(self, num: u32, m: Enc) -> Self {
            self.bytes(num, &m.0)
        }

        pub fn ts(self, num: u32, ms: i64) -> Self {
            self.msg(
                num,
                Enc::new()
                    .uint(1, (ms / 1000) as u64)
                    .uint(2, ((ms % 1000) * 1_000_000) as u64),
            )
        }

        pub fn build(self) -> Vec<u8> {
            self.0
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{enc::Enc, *};

    #[test]
    fn decodes_nested_fields() {
        let buf = Enc::new()
            .uint(1, 300)
            .str(2, "hi")
            .msg(3, Enc::new().uint(1, 7))
            .ts(4, 1_700_000_000_123)
            .build();
        let m = Msg(&buf);
        assert!(m.is_valid());
        assert_eq!(m.uint(1), Some(300));
        assert_eq!(m.text(2), Some("hi"));
        assert_eq!(m.path(&[3]).and_then(|m| m.uint(1)), Some(7));
        assert_eq!(m.msg(4).and_then(timestamp_ms), Some(1_700_000_000_123));
    }

    #[test]
    fn truncated_input_stops_quietly() {
        let mut buf = Enc::new().uint(1, 5).str(2, "hello").build();
        buf.truncate(buf.len() - 2);
        let m = Msg(&buf);
        assert!(!m.is_valid());
        assert_eq!(m.fields().count(), 1);
        assert_eq!(m.text(2), None);
    }
}
