//! SHA-256, in-tree, because the firmware may not carry it.
//!
//! `EFI_HASH2_PROTOCOL` would do this, and on the machines that have it it
//! does it well. It is an *optional* driver stack, which is exactly the trap
//! `EFI_HTTP` already set for this project: code written against a protocol
//! the firmware is allowed to omit works on the desk and fails on the one
//! server model that matters, at the point in the boot where there is nothing
//! to read the failure from. The list of protocols this binary depends on is
//! one entry long (`EFI_TCP4`) and this is not going to be the second.
//!
//! So the digest is computed here. It is ~120 lines of arithmetic from FIPS
//! 180-4, it needs no allocation, and it costs a few milliseconds on the only
//! thing it will ever be pointed at — a 45 KB `BOOTX64.EFI`.
//!
//! Lands ahead of its consumer. The self-update path (#2) verifies what it
//! wrote *before* swapping it in, since the stick is the boot path and a bad
//! write is a machine that does not POST into anything.
//!
//! ## Testing
//!
//! There is no `cargo test` in this crate — it is `no_main`/`no_std` with no
//! host target. This module is deliberately the exception that can still be
//! tested: it touches nothing but `core`, names no `crate::` item, and so it
//! compiles standalone as its own crate. On dev:
//!
//! ```text
//! rustc --test src/sha256.rs -o /build/cargo/stormbootx/sha256-test && \
//!   /build/cargo/stormbootx/sha256-test
//! ```
//!
//! Keep it that way. A dependency on anything else in this crate is what
//! would take the FIPS vectors below out of reach.

#![allow(dead_code)]

use core::fmt;

/// FIPS 180-4 §4.2.2 — the first 32 bits of the fractional parts of the cube
/// roots of the first 64 primes.
const K: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1,
    0x923f82a4, 0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3,
    0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786,
    0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147,
    0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13,
    0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
    0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a,
    0x5b9cca4f, 0x682e6ff3, 0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208,
    0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
];

/// The square roots of the first eight primes, same treatment.
const H0: [u32; 8] = [
    0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c,
    0x1f83d9ab, 0x5be0cd19,
];

/// A streaming SHA-256.
///
/// Streaming rather than one-shot on purpose: the things worth hashing here
/// arrive from `EFI_FILE_PROTOCOL` a buffer at a time, and holding a whole
/// payload in memory to hash it would mean the update path needs as much free
/// pool as the largest binary it might ever be handed.
#[derive(Clone)]
pub struct Sha256 {
    state: [u32; 8],
    /// Bytes of the current block not yet compressed. Always < 64.
    block: [u8; 64],
    filled: usize,
    /// Total bytes fed in. The padding encodes this as a *bit* count, which
    /// is where a 32-bit counter would quietly wrap at 512 MB.
    total: u64,
}

impl Default for Sha256 {
    fn default() -> Self {
        Self::new()
    }
}

impl Sha256 {
    pub const fn new() -> Self {
        Self { state: H0, block: [0u8; 64], filled: 0, total: 0 }
    }

    pub fn update(&mut self, mut data: &[u8]) {
        self.total = self.total.wrapping_add(data.len() as u64);

        // Top up a partial block first, so the fast path below always starts
        // on a block boundary.
        if self.filled > 0 {
            let want = 64 - self.filled;
            let take = want.min(data.len());
            self.block[self.filled..self.filled + take].copy_from_slice(&data[..take]);
            self.filled += take;
            data = &data[take..];
            if self.filled < 64 {
                return;
            }
            let block = self.block;
            self.compress(&block);
            self.filled = 0;
        }

        let mut chunks = data.chunks_exact(64);
        for chunk in &mut chunks {
            let mut block = [0u8; 64];
            block.copy_from_slice(chunk);
            self.compress(&block);
        }

        let rest = chunks.remainder();
        self.block[..rest.len()].copy_from_slice(rest);
        self.filled = rest.len();
    }

    pub fn finalize(mut self) -> Digest {
        // FIPS 180-4 §5.1.1: a single 1 bit, zeros, then the length in bits
        // as a 64-bit big-endian integer, filling to a block boundary.
        let bits = self.total.wrapping_mul(8);
        self.update(&[0x80]);
        while self.filled != 56 {
            self.update(&[0x00]);
        }
        self.update(&bits.to_be_bytes());
        debug_assert_eq!(self.filled, 0);

        let mut out = [0u8; 32];
        for (word, slot) in self.state.iter().zip(out.chunks_exact_mut(4)) {
            slot.copy_from_slice(&word.to_be_bytes());
        }
        Digest(out)
    }

    fn compress(&mut self, block: &[u8; 64]) {
        let mut w = [0u32; 64];
        for (i, word) in block.chunks_exact(4).enumerate() {
            w[i] = u32::from_be_bytes([word[0], word[1], word[2], word[3]]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }

        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = self.state;
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ ((!e) & g);
            let t1 = h
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(maj);

            h = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = b;
            b = a;
            a = t1.wrapping_add(t2);
        }

        for (slot, v) in self.state.iter_mut().zip([a, b, c, d, e, f, g, h]) {
            *slot = slot.wrapping_add(v);
        }
    }
}

/// Hash a buffer already in memory.
pub fn digest(data: &[u8]) -> Digest {
    let mut h = Sha256::new();
    h.update(data);
    h.finalize()
}

/// 32 bytes, and the ways this project needs to look at them.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Digest([u8; 32]);

impl Digest {
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// Lowercase hex, as it is written to the `stamp` line.
    pub fn to_hex(&self) -> [u8; 64] {
        const HEX: &[u8; 16] = b"0123456789abcdef";
        let mut out = [0u8; 64];
        for (byte, pair) in self.0.iter().zip(out.chunks_exact_mut(2)) {
            pair[0] = HEX[(byte >> 4) as usize];
            pair[1] = HEX[(byte & 0x0f) as usize];
        }
        out
    }

    /// Does this digest match a digest someone else wrote down?
    ///
    /// Tolerant on purpose, because the two ends are written by different
    /// hands: an optional `sha256:` prefix (how the registry names a blob),
    /// either case, and surrounding whitespace from a config line. Intolerant
    /// of everything else — a short, long or non-hex string is *not* a match,
    /// and the update path treats "not a match" as "do not swap".
    pub fn matches_hex(&self, s: &str) -> bool {
        let s = s.trim();
        let s = s.strip_prefix("sha256:").unwrap_or(s);
        if s.len() != 64 {
            return false;
        }
        let mine = self.to_hex();
        s.bytes()
            .zip(mine.iter())
            .all(|(theirs, &ours)| theirs.to_ascii_lowercase() == ours)
    }
}

impl fmt::Display for Digest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for byte in self.0.iter() {
            write!(f, "{byte:02x}")?;
        }
        Ok(())
    }
}

impl fmt::Debug for Digest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "sha256:{}", self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // These only ever build under `rustc --test`, which is a std build — the
    // crate itself compiles this module out entirely. So the std prelude is
    // available here and `extern crate alloc` is not needed.

    fn hex(data: &[u8]) -> String {
        format!("{}", digest(data))
    }

    /// FIPS 180-2 Appendix B, plus the two lengths that bracket the padding
    /// boundary — 55 bytes fits its length field in the same block, 56 does
    /// not and forces a second one. That off-by-one is the classic way to get
    /// a SHA-256 that is right for every input anyone happens to try.
    #[test]
    fn fips_vectors() {
        assert_eq!(
            hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            hex(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"),
            "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
        );
        let two_blocks = b"abcdefghbcdefghicdefghijdefghijkefghijklfghijklmghijklmnhijklmno\
ijklmnopjklmnopqklmnopqrlmnopqrsmnopqrstnopqrstu";
        assert_eq!(
            hex(two_blocks),
            "cf5b16a778af8380036ce59e7b0492370b249b11e8f07a51afac45037afee9d1"
        );
    }

    #[test]
    fn padding_boundary() {
        // 55 bytes: 0x80 plus the 8-byte length exactly fill one block.
        assert_eq!(
            hex(&[b'a'; 55]),
            "9f4390f8d30c2dd92ec9f095b65e2b9ae9b0a925a5258e241c9f1e910f734318"
        );
        // 56: the length no longer fits, so a whole second block is padding.
        assert_eq!(
            hex(&[b'a'; 56]),
            "b35439a4ac6f0948b6d6f9e3c6af0f5f590ce20f1bde7090ef7970686ec6738a"
        );
        // 64: an exact block, and still a full block of padding after it.
        assert_eq!(
            hex(&[b'a'; 64]),
            "ffe054fe7ae0cb6dc65c3af9b61d5209f439851db43d0ba5997337df154668eb"
        );
    }

    /// A megabyte, which is also the only test here that would catch a length
    /// counter narrower than 64 bits doing something wrong at scale.
    #[test]
    fn long_input() {
        assert_eq!(
            hex(&[b'a'; 1_000_000]),
            "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0"
        );
    }

    /// The streaming path has to agree with the one-shot path for *every*
    /// split, not just the tidy ones. Odd chunk sizes exercise topping up a
    /// partial block, spilling across a boundary, and the remainder tail.
    #[test]
    fn streaming_matches_one_shot() {
        let data: Vec<u8> = (0..1000u32).map(|i| (i % 251) as u8).collect();
        let want = digest(&data);

        for chunk in [1usize, 3, 7, 31, 63, 64, 65, 127, 128, 999, 1000] {
            let mut h = Sha256::new();
            for part in data.chunks(chunk) {
                h.update(part);
            }
            assert_eq!(h.finalize(), want, "chunk size {chunk}");
        }
    }

    #[test]
    fn empty_updates_change_nothing() {
        let mut h = Sha256::new();
        h.update(b"");
        h.update(b"ab");
        h.update(b"");
        h.update(b"c");
        h.update(b"");
        assert_eq!(h.finalize(), digest(b"abc"));
    }

    #[test]
    fn matches_hex_is_tolerant_where_it_should_be() {
        let d = digest(b"abc");
        let want = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";

        assert!(d.matches_hex(want));
        assert!(d.matches_hex(&format!("  {want}\t")));
        assert!(d.matches_hex(&format!("sha256:{want}")));
        assert!(d.matches_hex(&want.to_uppercase()));

        // And unforgiving everywhere else: a truncated, extended, empty or
        // non-hex stamp is not a match, because the caller reads "no match"
        // as "do not swap the boot binary".
        assert!(!d.matches_hex(&want[..63]));
        assert!(!d.matches_hex(&format!("{want}0")));
        assert!(!d.matches_hex(""));
        assert!(!d.matches_hex("sha256:"));
        assert!(!d.matches_hex(&"z".repeat(64)));
        assert!(!d.matches_hex(&digest(b"abd").to_string()));
    }

    #[test]
    fn debug_names_the_algorithm() {
        assert_eq!(
            format!("{:?}", digest(b"abc")),
            "sha256:ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }
}
