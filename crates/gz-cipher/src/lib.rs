//! Exact port of the ITZ link cipher used on the client-facing links
//! (blueprint 01 Part A §2, task T-004).
//!
//! Legacy source: `NetworkLib\ITZNetLib\ITZEncrypt.cpp` and `.h`. Every rule the
//! legacy applies is reproduced here, including the ones that only exist because
//! of how the C was written — see the crate README for the full list.
//!
//! # What the cipher does
//!
//! A packet is framed by a little-endian `u16` at offset 0 (the `usSize` field of
//! `sPacketHeader`). Offset 2 (`ucChk`) carries a one-byte integrity value, and
//! everything from offset 3 onwards is enciphered. The output is the same length
//! as the input.
//!
//! # State
//!
//! The cipher is a per-link stream cipher whose only persistent state is the two
//! key bytes, and **both keys advance by one on every call** — including the
//! `size <= 3` passthrough branch and calls that are later rejected. Peers only
//! stay in step if they call `encode`/`decode` the same number of times in the
//! same order, so one `Cipher` per direction per connection.

/// `itzbase.h` line 45. `decode` rejects any declared size above this.
///
/// Note the legacy asymmetry: `Encode` has no such check, so a frame it produced
/// with a size in `65001..=65535` cannot be decoded.
pub const PACKET_MAX_SIZE: usize = 65000;

/// Key byte 1 of the fixed second-instance link key, `ITZSession.cpp` line 56.
pub const LINK_KEY1: u8 = 0x12;

/// Key byte 2 of the fixed second-instance link key, `ITZSession.cpp` line 56.
pub const LINK_KEY2: u8 = 0x34;

/// Why a call could not be completed.
///
/// The legacy returns `void` from `Encode` and `FALSE` from `Decode`, and reads
/// past the end of the caller's buffer when a length field lies. Those reads are
/// rejected here instead; a lie that survives both checks still produces a
/// rejected decode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CipherError {
    /// Fewer than two bytes, so the length field cannot be read at all.
    Truncated,
    /// The length field claims more bytes than the source buffer holds. The
    /// legacy would read past the end here.
    LengthMismatch {
        /// Size taken from the frame header.
        declared: u16,
        /// Bytes actually available.
        have: usize,
    },
    /// Declared size above [`PACKET_MAX_SIZE`], which `decode` rejects outright.
    TooLarge(u16),
    /// The destination buffer is shorter than the declared size.
    DestTooSmall {
        /// Size taken from the frame header.
        declared: u16,
        /// Bytes actually available.
        have: usize,
    },
}

impl std::fmt::Display for CipherError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Truncated => write!(f, "frame shorter than the 2-byte size field"),
            Self::LengthMismatch { declared, have } => {
                write!(f, "frame declares {declared} bytes but holds {have}")
            }
            Self::TooLarge(size) => {
                write!(f, "frame declares {size} bytes, above PACKET_MAX_SIZE")
            }
            Self::DestTooSmall { declared, have } => {
                write!(f, "destination holds {have} bytes, needs {declared}")
            }
        }
    }
}

impl std::error::Error for CipherError {}

/// The legacy `ITZEncrypt` instance: two key bytes that tick once per call.
///
/// Not thread-safe by design, matching the legacy, which relied on the enclosing
/// session's critical section.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cipher {
    key1: u8,
    key2: u8,
}

impl Cipher {
    /// A cipher with both keys at zero.
    ///
    /// The legacy constructor body is empty, so the legacy keys are whatever was
    /// already in the freshly allocated memory; zero is this port's defined
    /// choice. The two configured instances are [`Cipher::session_cipher`] and
    /// [`Cipher::link_cipher`] — always prefer those.
    ///
    /// This is not just tidiness. The legacy only ever keys its link cipher from
    /// `ITZSession::Init`, reached by virtual dispatch through `ITZList::batch`,
    /// so correctness there depends on a session being promoted into the master
    /// list before anything decodes on it. (Tracked as BUG-011 in
    /// `known_legacy_bugs.md`.) Here, a `Cipher` is usable the moment it exists.
    #[must_use]
    pub const fn new() -> Self {
        Self { key1: 0, key2: 0 }
    }

    /// The negotiated-key instance (`ITZSession::GetEncrypt`).
    ///
    /// Keyed from the server, by `sResponse_SecretKey` on the client side and by
    /// `char.cpp` line 5201 on the server side.
    #[must_use]
    pub const fn session_cipher(key1: u8, key2: u8) -> Self {
        Self { key1, key2 }
    }

    /// The fixed-key instance (`ITZSession::GetEncrypt2`), keyed `0x12,0x34` by
    /// `ITZSession::Init`.
    #[must_use]
    pub const fn link_cipher() -> Self {
        Self {
            key1: LINK_KEY1,
            key2: LINK_KEY2,
        }
    }

    /// `SetKey`. Also moves the counter, since the keys *are* the counter.
    pub fn set_key(&mut self, key1: u8, key2: u8) {
        self.key1 = key1;
        self.key2 = key2;
    }

    /// `GetKey1`. The value at the current call position, not the initial key.
    #[must_use]
    pub const fn key1(&self) -> u8 {
        self.key1
    }

    /// `GetKey2`. The value at the current call position, not the initial key.
    #[must_use]
    pub const fn key2(&self) -> u8 {
        self.key2
    }

    /// Little-endian `usSize` from the front of a frame.
    fn declared_len(src: &[u8]) -> Result<usize, CipherError> {
        let raw = src
            .get(..2)
            .ok_or(CipherError::Truncated)?
            .try_into()
            .expect("slice of length 2");
        Ok(u16::from_le_bytes(raw) as usize)
    }

    /// `Encode`. Returns the `size` bytes the legacy call writes.
    ///
    /// Advances both keys first, exactly like the legacy, and does so even on the
    /// passthrough branch.
    ///
    /// # Errors
    ///
    /// [`CipherError::Truncated`] or [`CipherError::LengthMismatch`] when the
    /// length field is unreadable or overruns `src`. The legacy reads out of
    /// bounds in the second case.
    pub fn encode(&mut self, src: &[u8]) -> Result<Vec<u8>, CipherError> {
        self.key1 = self.key1.wrapping_add(1);
        self.key2 = self.key2.wrapping_add(1);

        let size = Self::declared_len(src)?;
        if src.len() < size {
            return Err(CipherError::LengthMismatch {
                declared: size as u16,
                have: src.len(),
            });
        }

        // size <= 3 is a verbatim copy, and it is the only branch that carries
        // offset 2 through unchanged.
        if size <= 3 {
            return Ok(src[..size].to_vec());
        }

        let mut dest = vec![0u8; size];
        // Offset 2 is the integrity slot: the last plaintext byte, plus key1.
        dest[2] = src[size - 1].wrapping_add(self.key1);
        dest[..2].copy_from_slice(&(size as u16).to_le_bytes());

        let mut process1 = self.key1;
        let mut process2 = self.key2;
        for offset in 3..size {
            process1 ^= src[offset];
            dest[offset] = process1.wrapping_add(process2);
            process2 = process2.wrapping_add(src[offset]);
        }
        Ok(dest)
    }

    /// `Decode`. Returns whether the integrity byte matched.
    ///
    /// Advances both keys first, like the legacy, including on the paths that are
    /// rejected.
    ///
    /// `dest` must be at least `size` bytes. **`dest[2]` is left untouched**,
    /// because the legacy loop writes its first byte at `dest[3]`. The legacy
    /// call sites all pass a long-lived member array that is never cleared, so
    /// that byte is left over from the previous packet — and on the first
    /// packet, uninitialised memory. (Tracked as BUG-012 in
    /// `known_legacy_bugs.md`.)
    ///
    /// It is currently harmless: nothing in the legacy tree ever reads `ucChk`
    /// out of a decoded buffer, because the two places that read it read the raw
    /// *source* instead. Do not assume the byte you find there is a decoded
    /// `ucChk`, and do not let a reused buffer plus an unwritten field reach a
    /// struct that message code goes on to read.
    ///
    /// # Errors
    ///
    /// [`CipherError::Truncated`], [`CipherError::TooLarge`] when the declared
    /// size exceeds [`PACKET_MAX_SIZE`] (the legacy returns `FALSE` here), or
    /// [`CipherError::LengthMismatch`] / [`CipherError::DestTooSmall`].
    pub fn decode(&mut self, src: &[u8], dest: &mut [u8]) -> Result<bool, CipherError> {
        self.key1 = self.key1.wrapping_add(1);
        self.key2 = self.key2.wrapping_add(1);

        let size = Self::declared_len(src)?;
        if size > PACKET_MAX_SIZE {
            return Err(CipherError::TooLarge(size as u16));
        }
        if src.len() < size {
            return Err(CipherError::LengthMismatch {
                declared: size as u16,
                have: src.len(),
            });
        }
        if dest.len() < size {
            return Err(CipherError::DestTooSmall {
                declared: size as u16,
                have: dest.len(),
            });
        }

        if size <= 3 {
            dest[..size].copy_from_slice(&src[..size]);
            return Ok(true);
        }

        // The legacy decrements its *source* buffer here in place; doing it to a
        // local keeps `src` immutable with no change to the result below.
        let integrity = src[2].wrapping_sub(self.key1);

        let mut process1 = self.key1;
        let mut process2 = self.key2;
        dest[..2].copy_from_slice(&(size as u16).to_le_bytes());
        for offset in 3..size {
            let mut plain = src[offset].wrapping_sub(process2);
            plain ^= process1;
            dest[offset] = plain;
            process1 ^= plain;
            process2 = process2.wrapping_add(plain);
        }

        Ok(integrity == dest[size - 1])
    }
}

impl Default for Cipher {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const _: () = {
        // itzbase.h line 45 and ITZSession.cpp line 56.
        assert!(PACKET_MAX_SIZE == 65000);
        assert!(LINK_KEY1 == 0x12);
        assert!(LINK_KEY2 == 0x34);
    };

    #[test]
    fn link_cipher_carries_the_legacy_key() {
        let cipher = Cipher::link_cipher();
        assert_eq!(cipher.key1(), 0x12);
        assert_eq!(cipher.key2(), 0x34);
    }

    #[test]
    fn keys_advance_on_every_call_including_passthrough() {
        let mut cipher = Cipher::new();
        let passthrough = [0x03, 0x00, 0xab];
        assert_eq!(cipher.encode(&passthrough).unwrap().len(), 3);
        assert_eq!((cipher.key1(), cipher.key2()), (1, 1));
        assert_eq!(cipher.encode(&[0x04, 0x00, 0x00, 0x11]).unwrap()[3], 0x15);
        assert_eq!((cipher.key1(), cipher.key2()), (2, 2));
    }

    #[test]
    fn oversize_frame_is_rejected_without_reading_the_body() {
        // The legacy checks the size before touching the body, so two bytes are
        // enough to drive the rejection.
        let mut cipher = Cipher::new();
        let mut dest = [0u8; 4];
        assert_eq!(
            cipher.decode(&[0xff, 0xff], &mut dest),
            Err(CipherError::TooLarge(0xffff))
        );
        assert_eq!((cipher.key1(), cipher.key2()), (1, 1));
    }

    #[test]
    fn encode_rejects_a_length_field_that_overruns_the_source() {
        let mut cipher = Cipher::new();
        assert_eq!(
            cipher.encode(&[0x08, 0x00, 0x01]),
            Err(CipherError::LengthMismatch {
                declared: 8,
                have: 3
            })
        );
    }

    #[test]
    fn decode_leaves_the_source_untouched() {
        // The legacy rewrites the caller's source buffer at offset 2; this port
        // does not, which is a deliberate deviation.
        let mut tx = Cipher::link_cipher();
        let mut rx = Cipher::link_cipher();
        let frame = [0x08, 0x00, 0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff];
        let wire = tx.encode(&frame).unwrap();
        let before = wire.clone();
        let mut plain = vec![0u8; frame.len()];
        assert!(rx.decode(&wire, &mut plain).unwrap());
        assert_eq!(wire, before);
        assert_eq!(&plain[..2], &frame[..2]);
        assert_eq!(&plain[3..], &frame[3..]);
    }

    #[test]
    fn decoded_integrity_byte_is_the_callers_buffer_not_the_plaintext() {
        // Documented consequence of the legacy skipping dest[2]: the value the
        // legacy caller ends up reading as `ucChk` is whatever it had before.
        let mut tx = Cipher::new();
        let mut rx = Cipher::new();
        let frame = [0x04, 0x00, 0x00, 0x11];
        let wire = tx.encode(&frame).unwrap();
        let mut dest = vec![0xAA; frame.len()];
        assert!(rx.decode(&wire, &mut dest).unwrap());
        assert_eq!(dest, vec![0x04, 0x00, 0xAA, 0x11]);
    }
}
