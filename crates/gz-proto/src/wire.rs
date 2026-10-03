//! Little-endian byte reader/writer for the fixed C structs on the wire.
//!
//! The legacy never byte-swaps packet data (blueprint 01 Part A §1.6) and every
//! client-facing struct is an MSVC x86 layout: little-endian, `TCHAR` = 1 byte,
//! default packing `/Zp8` unless the header says otherwise. The reader/writer
//! here are layout-free primitives; the callers in [`crate::front`] and
//! [`crate::game`] place the explicit padding that the legacy packing inserted.
//!
//! Payloads here are **headerless**: the 5-byte `sPacketHeader` is owned by
//! [`gz_net`]. `payload.len() + gz_net::HEADER_SIZE` is the struct's C
//! `sizeof`, which is what every size assertion in the tests checks.

/// Why a read could not be completed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WireError {
    /// The buffer ended before every field had been read.
    Truncated {
        /// Bytes the reader still needed.
        needed: usize,
        /// Bytes actually left in the buffer.
        have: usize,
    },
}

impl std::fmt::Display for WireError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Truncated { needed, have } => {
                write!(f, "payload truncated: needed {needed} bytes, {have} left")
            }
        }
    }
}

impl std::error::Error for WireError {}

/// Builds a payload field by field.
#[derive(Debug, Default, Clone)]
pub struct Writer {
    buf: Vec<u8>,
}

impl Writer {
    /// An empty writer.
    #[must_use]
    pub const fn new() -> Self {
        Self { buf: Vec::new() }
    }

    /// An empty writer with room for `n` bytes.
    #[must_use]
    pub fn with_capacity(n: usize) -> Self {
        Self {
            buf: Vec::with_capacity(n),
        }
    }

    /// Appends one byte.
    pub fn u8(&mut self, v: u8) {
        self.buf.push(v);
    }

    /// Appends a signed byte.
    pub fn i8(&mut self, v: i8) {
        self.buf.push(v as u8);
    }

    /// Appends a little-endian `u16`.
    pub fn u16(&mut self, v: u16) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }

    /// Appends a little-endian `i16`.
    pub fn i16(&mut self, v: i16) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }

    /// Appends a little-endian `u32`.
    pub fn u32(&mut self, v: u32) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }

    /// Appends a little-endian `i32`.
    pub fn i32(&mut self, v: i32) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }

    /// Appends a little-endian `i64`.
    pub fn i64(&mut self, v: i64) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }

    /// Appends raw bytes.
    pub fn bytes(&mut self, b: &[u8]) {
        self.buf.extend_from_slice(b);
    }

    /// Writes a C string into a fixed `n`-byte field: copied, truncated at `n`,
    /// NUL-padded to `n`. A `str` that fills the field exactly keeps no
    /// terminator, matching the legacy's `strcpy`-into-a-full-buffer behaviour.
    pub fn fixed_str(&mut self, s: &str, n: usize) {
        let bytes = s.as_bytes();
        let take = bytes.len().min(n);
        self.buf.extend_from_slice(&bytes[..take]);
        self.buf.resize(self.buf.len() + (n - take), 0);
    }

    /// Appends `n` zero bytes (the padding the C compiler inserted).
    pub fn pad(&mut self, n: usize) {
        self.buf.resize(self.buf.len() + n, 0);
    }

    /// Bytes written so far.
    #[must_use]
    pub fn len(&self) -> usize {
        self.buf.len()
    }

    /// Whether nothing has been written.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.buf.is_empty()
    }

    /// Consumes the writer.
    #[must_use]
    pub fn finish(self) -> Vec<u8> {
        self.buf
    }
}

/// Reads a payload field by field.
#[derive(Debug, Clone)]
pub struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    /// A reader over the whole payload.
    #[must_use]
    pub const fn new(buf: &'a [u8]) -> Self {
        Self { buf, pos: 0 }
    }

    /// Bytes not yet consumed.
    #[must_use]
    pub fn remaining(&self) -> usize {
        self.buf.len() - self.pos
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8], WireError> {
        if self.remaining() < n {
            return Err(WireError::Truncated {
                needed: n,
                have: self.remaining(),
            });
        }
        let out = &self.buf[self.pos..self.pos + n];
        self.pos += n;
        Ok(out)
    }

    /// Reads one byte.
    ///
    /// # Errors
    ///
    /// [`WireError::Truncated`] if no byte is left.
    pub fn u8(&mut self) -> Result<u8, WireError> {
        Ok(self.take(1)?[0])
    }

    /// Reads one signed byte.
    ///
    /// # Errors
    ///
    /// [`WireError::Truncated`] if no byte is left.
    pub fn i8(&mut self) -> Result<i8, WireError> {
        Ok(self.take(1)?[0] as i8)
    }

    /// Reads a little-endian `u16`.
    ///
    /// # Errors
    ///
    /// [`WireError::Truncated`] if fewer than 2 bytes remain.
    pub fn u16(&mut self) -> Result<u16, WireError> {
        let b = self.take(2)?;
        Ok(u16::from_le_bytes([b[0], b[1]]))
    }

    /// Reads a little-endian `i16`.
    ///
    /// # Errors
    ///
    /// [`WireError::Truncated`] if fewer than 2 bytes remain.
    pub fn i16(&mut self) -> Result<i16, WireError> {
        Ok(self.u16()? as i16)
    }

    /// Reads a little-endian `i32`.
    ///
    /// # Errors
    ///
    /// [`WireError::Truncated`] if fewer than 4 bytes remain.
    pub fn i32(&mut self) -> Result<i32, WireError> {
        let b = self.take(4)?;
        Ok(i32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    /// Reads a little-endian `u32`.
    ///
    /// # Errors
    ///
    /// [`WireError::Truncated`] if fewer than 4 bytes remain.
    pub fn u32(&mut self) -> Result<u32, WireError> {
        let b = self.take(4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    /// Reads a little-endian `i64`.
    ///
    /// # Errors
    ///
    /// [`WireError::Truncated`] if fewer than 8 bytes remain.
    pub fn i64(&mut self) -> Result<i64, WireError> {
        let b = self.take(8)?;
        Ok(i64::from_le_bytes([
            b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7],
        ]))
    }

    /// Reads a fixed `n`-byte C string, cutting at the first NUL. Invalid UTF-8
    /// is replaced rather than rejected (legacy strings are MBCS).
    ///
    /// # Errors
    ///
    /// [`WireError::Truncated`] if fewer than `n` bytes remain.
    pub fn fixed_str(&mut self, n: usize) -> Result<String, WireError> {
        let b = self.take(n)?;
        let end = b.iter().position(|&c| c == 0).unwrap_or(b.len());
        Ok(String::from_utf8_lossy(&b[..end]).into_owned())
    }

    /// Reads `n` raw bytes.
    ///
    /// # Errors
    ///
    /// [`WireError::Truncated`] if fewer than `n` bytes remain.
    pub fn bytes(&mut self, n: usize) -> Result<&'a [u8], WireError> {
        self.take(n)
    }

    /// Skips `n` bytes.
    ///
    /// # Errors
    ///
    /// [`WireError::Truncated`] if fewer than `n` bytes remain.
    pub fn skip(&mut self, n: usize) -> Result<(), WireError> {
        self.take(n).map(|_| ())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn integers_round_trip_little_endian() {
        let mut w = Writer::new();
        w.u8(0xab);
        w.i8(-1);
        w.u16(0x1234);
        w.i16(-2);
        w.u32(0xdead_beef);
        w.i32(-3);
        w.i64(-4);
        let bytes = w.finish();
        assert_eq!(&bytes[..2], &[0xab, 0xff]);
        assert_eq!(&bytes[2..4], &[0x34, 0x12]);

        let mut r = Reader::new(&bytes);
        assert_eq!(r.u8().unwrap(), 0xab);
        assert_eq!(r.i8().unwrap(), -1);
        assert_eq!(r.u16().unwrap(), 0x1234);
        assert_eq!(r.i16().unwrap(), -2);
        assert_eq!(r.u32().unwrap(), 0xdead_beef);
        assert_eq!(r.i32().unwrap(), -3);
        assert_eq!(r.i64().unwrap(), -4);
        assert_eq!(r.remaining(), 0);
    }

    #[test]
    fn fixed_str_is_nul_padded_and_truncated() {
        let mut w = Writer::new();
        w.fixed_str("hi", 5);
        assert_eq!(w.finish(), b"hi\0\0\0");

        let mut w = Writer::new();
        w.fixed_str("abcdef", 3);
        assert_eq!(w.finish(), b"abc");

        let mut r = Reader::new(b"hi\0\0\0");
        assert_eq!(r.fixed_str(5).unwrap(), "hi");
    }

    #[test]
    fn reader_reports_truncation() {
        let mut r = Reader::new(&[1, 2, 3]);
        assert_eq!(r.i32(), Err(WireError::Truncated { needed: 4, have: 3 }));
    }
}
