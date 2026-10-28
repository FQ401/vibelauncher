//! The CurseForge file fingerprint.
//!
//! CurseForge identifies a file by MurmurHash2 (32-bit, seed `1`) of its bytes with every tab
//! (`9`), line feed (`10`), carriage return (`13`) and space (`32`) left out.

/// The MurmurHash2 multiplier.
const M: u32 = 0x5bd1_e995;

/// Whether CurseForge leaves this byte out of a fingerprint.
fn skipped(byte: u8) -> bool {
    matches!(byte, 9 | 10 | 13 | 32)
}

/// The CurseForge fingerprint of a file's bytes.
pub fn fingerprint(bytes: &[u8]) -> u32 {
    let mut hasher = Fingerprinter::new(fingerprint_len(bytes));
    hasher.update(bytes);
    hasher.finish()
}

/// How many bytes of `bytes` count for a fingerprint (all but tab, line feed, carriage return
/// and space). Summed over the pieces of a file, it is the length [`Fingerprinter::new`] takes.
pub fn fingerprint_len(bytes: &[u8]) -> u64 {
    bytes.iter().filter(|b| !skipped(**b)).count() as u64
}

/// The CurseForge fingerprint of a file read in pieces. The length enters the hash first, so a
/// file is read twice: once summing [`fingerprint_len`] of its pieces, once feeding them to
/// [`update`](Self::update).
#[derive(Debug, Clone)]
pub struct Fingerprinter(Murmur2);

impl Fingerprinter {
    /// A fingerprint of a file whose counted length is `len`.
    pub fn new(len: u64) -> Self {
        Self(Murmur2::new(len, 1))
    }

    /// Feeds the next piece of the file.
    pub fn update(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            if !skipped(byte) {
                self.0.push(byte);
            }
        }
    }

    /// The fingerprint.
    pub fn finish(self) -> u32 {
        self.0.finish()
    }
}

/// MurmurHash2 (32-bit) of `bytes` with `seed`.
pub fn murmur2(bytes: &[u8], seed: u32) -> u32 {
    let mut hasher = Murmur2::new(bytes.len() as u64, seed);
    for &byte in bytes {
        hasher.push(byte);
    }
    hasher.finish()
}

/// MurmurHash2 state over a stream of a known length.
#[derive(Debug, Clone)]
struct Murmur2 {
    h: u32,
    block: [u8; 4],
    filled: usize,
}

impl Murmur2 {
    /// A hash of `len` bytes. A length above `u32::MAX` wraps, as the reference
    /// implementation's 32-bit length does.
    fn new(len: u64, seed: u32) -> Self {
        Self {
            h: seed ^ (len as u32),
            block: [0; 4],
            filled: 0,
        }
    }

    fn push(&mut self, byte: u8) {
        if let Some(slot) = self.block.get_mut(self.filled) {
            *slot = byte;
        }
        self.filled += 1;
        if self.filled == 4 {
            let mut k = u32::from_le_bytes(self.block);
            k = k.wrapping_mul(M);
            k ^= k >> 24;
            k = k.wrapping_mul(M);
            self.h = self.h.wrapping_mul(M);
            self.h ^= k;
            self.filled = 0;
        }
    }

    fn finish(self) -> u32 {
        let mut h = self.h;
        if self.filled > 0 {
            let [b0, b1, b2, _] = self.block;
            if self.filled == 3 {
                h ^= u32::from(b2) << 16;
            }
            if self.filled >= 2 {
                h ^= u32::from(b1) << 8;
            }
            h ^= u32::from(b0);
            h = h.wrapping_mul(M);
        }
        h ^= h >> 13;
        h = h.wrapping_mul(M);
        h ^ (h >> 15)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Known answers of MurmurHash2 with seed 1, worked out with a separate implementation.
    #[test]
    fn murmur2_known_answers() {
        assert_eq!(murmur2(b"", 0), 0);
        assert_eq!(murmur2(b"abc", 0), 324_500_635);
        assert_eq!(murmur2(b"", 1), 1_540_447_798);
        assert_eq!(murmur2(b"a", 1), 626_045_324);
        assert_eq!(murmur2(b"ab", 1), 1_692_487_918);
        assert_eq!(murmur2(b"abc", 1), 1_621_425_345);
        assert_eq!(murmur2(b"abcd", 1), 3_376_380_438);
        assert_eq!(murmur2(b"Hello, world!", 1), 1_671_593_801);
        assert_eq!(murmur2(b"Hello, world!", 0x9747_b28c), 0xbeba_9b12);
        let all: Vec<u8> = (0..=255).collect();
        assert_eq!(murmur2(&all, 1), 253_525_554);
    }

    #[test]
    fn fingerprint_known_answers() {
        assert_eq!(fingerprint(b""), 1_540_447_798);
        assert_eq!(fingerprint(b"Hello, world!"), 2_173_663_876);
        assert_eq!(
            fingerprint(b"The quick brown fox jumps over the lazy dog"),
            3_751_777_527
        );
        let all: Vec<u8> = (0..=255).collect();
        assert_eq!(fingerprint(&all), 2_094_645_347);
    }

    #[test]
    fn whitespace_bytes_do_not_count() {
        assert_eq!(fingerprint(b"a b\tc\r\nd"), fingerprint(b"abcd"));
        assert_eq!(fingerprint(b"a b\tc\r\nd"), 3_376_380_438);
        assert_eq!(fingerprint(b" \t\r\n"), fingerprint(b""));
        // Other control bytes count.
        assert_ne!(fingerprint(b"ab\x0bcd"), fingerprint(b"abcd"));
    }

    #[test]
    fn pieces_of_any_size_give_the_same_fingerprint() {
        let data: Vec<u8> = (0..1000u32).map(|i| (i * 7 % 251) as u8).collect();
        for size in [1, 3, 4, 5, 64, 999, 1000] {
            let len: u64 = data.chunks(size).map(fingerprint_len).sum();
            let mut hasher = Fingerprinter::new(len);
            for piece in data.chunks(size) {
                hasher.update(piece);
            }
            assert_eq!(hasher.finish(), fingerprint(&data), "pieces of {size}");
        }
    }
}
