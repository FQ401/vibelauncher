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
    let kept = bytes.iter().copied().filter(|b| !skipped(*b));
    // The length enters the hash first; counting needs a pass of its own.
    let len = bytes.iter().filter(|b| !skipped(**b)).count();
    murmur2_iter(kept, len, 1)
}

/// MurmurHash2 (32-bit) of `bytes` with `seed`.
pub fn murmur2(bytes: &[u8], seed: u32) -> u32 {
    murmur2_iter(bytes.iter().copied(), bytes.len(), seed)
}

/// MurmurHash2 over `len` bytes from `bytes`. A length above `u32::MAX` wraps, as the reference
/// implementation's 32-bit length does.
fn murmur2_iter(bytes: impl Iterator<Item = u8>, len: usize, seed: u32) -> u32 {
    let mut h = seed ^ (len as u32);
    let mut block = [0u8; 4];
    let mut filled = 0;
    for byte in bytes {
        if let Some(slot) = block.get_mut(filled) {
            *slot = byte;
        }
        filled += 1;
        if filled == 4 {
            let mut k = u32::from_le_bytes(block);
            k = k.wrapping_mul(M);
            k ^= k >> 24;
            k = k.wrapping_mul(M);
            h = h.wrapping_mul(M);
            h ^= k;
            filled = 0;
        }
    }
    if filled > 0 {
        let [b0, b1, b2, _] = block;
        if filled == 3 {
            h ^= u32::from(b2) << 16;
        }
        if filled >= 2 {
            h ^= u32::from(b1) << 8;
        }
        h ^= u32::from(b0);
        h = h.wrapping_mul(M);
    }
    h ^= h >> 13;
    h = h.wrapping_mul(M);
    h ^ (h >> 15)
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
}
