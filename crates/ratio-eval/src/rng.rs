//! Per-branch random generators (chapter 01, chapter 06 `rand`; design PC-1).
//!
//! Each branch draws from its own `ChaCha20` stream seeded with
//! `BLAKE3(turn seed ‖ frame path ‖ 0 ‖ branch path)`, so a draw is a function of the branch's
//! position alone: concurrent branches draw the same values whatever their timing, and the
//! draws need no tape entries (the turn's seed is in the tape's header). The `rand` built-ins
//! arrive with stage 11b and draw from here.

/// A branch's generator: `ChaCha20` (RFC 8439 block function, 20 rounds), key from the seed,
/// nonce zero, counter from 0.
#[derive(Clone, Debug)]
pub struct BranchRng {
    key: [u32; 8],
    counter: u64,
    block: [u32; 16],
    used: usize,
}

fn quarter(s: &mut [u32; 16], a: usize, b: usize, c: usize, d: usize) {
    s[a] = s[a].wrapping_add(s[b]);
    s[d] = (s[d] ^ s[a]).rotate_left(16);
    s[c] = s[c].wrapping_add(s[d]);
    s[b] = (s[b] ^ s[c]).rotate_left(12);
    s[a] = s[a].wrapping_add(s[b]);
    s[d] = (s[d] ^ s[a]).rotate_left(8);
    s[c] = s[c].wrapping_add(s[d]);
    s[b] = (s[b] ^ s[c]).rotate_left(7);
}

/// The `ChaCha20` block for `key`, a 64-bit block counter and a zero nonce.
fn block(key: &[u32; 8], counter: u64) -> [u32; 16] {
    let mut s = [0u32; 16];
    s[..4].copy_from_slice(&[0x6170_7865, 0x3320_646e, 0x7962_2d32, 0x6b20_6574]);
    s[4..12].copy_from_slice(key);
    #[allow(clippy::cast_possible_truncation, reason = "the counter's two words")]
    {
        s[12] = counter as u32;
        s[13] = (counter >> 32) as u32;
    }
    let init = s;
    for _ in 0..10 {
        quarter(&mut s, 0, 4, 8, 12);
        quarter(&mut s, 1, 5, 9, 13);
        quarter(&mut s, 2, 6, 10, 14);
        quarter(&mut s, 3, 7, 11, 15);
        quarter(&mut s, 0, 5, 10, 15);
        quarter(&mut s, 1, 6, 11, 12);
        quarter(&mut s, 2, 7, 8, 13);
        quarter(&mut s, 3, 4, 9, 14);
    }
    for (x, i) in s.iter_mut().zip(init) {
        *x = x.wrapping_add(i);
    }
    s
}

impl BranchRng {
    /// The generator of the branch at `branch_path` in the frame at `frame_path`, for the
    /// turn whose seed is `seed`.
    #[must_use]
    pub fn new(seed: &[u8; 32], frame_path: &str, branch_path: &str) -> BranchRng {
        let mut input = Vec::with_capacity(32 + frame_path.len() + branch_path.len() + 1);
        input.extend_from_slice(seed);
        input.extend_from_slice(frame_path.as_bytes());
        input.push(0);
        input.extend_from_slice(branch_path.as_bytes());
        BranchRng::from_key(&crate::builtins::blake3::hash(&input))
    }

    /// A generator keyed directly (for the `ChaCha20` test vectors).
    #[must_use]
    pub fn from_key(key: &[u8; 32]) -> BranchRng {
        let mut k = [0u32; 8];
        for (i, w) in k.iter_mut().enumerate() {
            *w = u32::from_le_bytes([key[i * 4], key[i * 4 + 1], key[i * 4 + 2], key[i * 4 + 3]]);
        }
        BranchRng {
            key: k,
            counter: 0,
            block: [0; 16],
            used: 16,
        }
    }

    /// The next 32 bits of the stream.
    pub fn next_u32(&mut self) -> u32 {
        if self.used == 16 {
            self.block = block(&self.key, self.counter);
            self.counter = self.counter.wrapping_add(1);
            self.used = 0;
        }
        let w = self.block[self.used];
        self.used += 1;
        w
    }

    /// The next 64 bits.
    pub fn next_u64(&mut self) -> u64 {
        let lo = u64::from(self.next_u32());
        let hi = u64::from(self.next_u32());
        lo | (hi << 32)
    }

    /// A uniform integer in `0..n` (`n > 0`), without modulo bias.
    pub fn below(&mut self, n: u64) -> u64 {
        if n <= 1 {
            return 0;
        }
        let zone = u64::MAX - (u64::MAX % n) - 1;
        loop {
            let x = self.next_u64();
            if x <= zone {
                return x % n;
            }
        }
    }

    /// A uniform float in `[0, 1)` (53 bits).
    #[allow(clippy::cast_precision_loss, reason = "53 bits are exact in an f64")]
    pub fn float(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RFC 8439 §2.3.2: the block function with key 00..1f, counter 1 and nonce
    /// 00:00:00:09:00:00:00:4a:00:00:00:00 gives a known first word. Our nonce is zero, so we
    /// check the all-zero key and counter 0 against the well-known keystream instead.
    #[test]
    fn chacha20_zero_key_keystream() {
        let mut r = BranchRng::from_key(&[0; 32]);
        // The first four little-endian words of `ChaCha20`(key 0, nonce 0, counter 0):
        // 76 b8 e0 ad a0 f1 3d 90 40 5d 6a e5 53 86 bd 28 …
        assert_eq!(r.next_u32(), 0xade0_b876);
        assert_eq!(r.next_u32(), 0x903d_f1a0);
        assert_eq!(r.next_u32(), 0xe56a_5d40);
        assert_eq!(r.next_u32(), 0x28bd_8653);
    }

    #[test]
    fn branches_draw_by_position() {
        let seed = [7; 32];
        let a1: Vec<u32> = {
            let mut r = BranchRng::new(&seed, "turn", "c12#0.b0");
            (0..4).map(|_| r.next_u32()).collect()
        };
        let a2: Vec<u32> = {
            let mut r = BranchRng::new(&seed, "turn", "c12#0.b0");
            (0..4).map(|_| r.next_u32()).collect()
        };
        let b: Vec<u32> = {
            let mut r = BranchRng::new(&seed, "turn", "c12#0.b1");
            (0..4).map(|_| r.next_u32()).collect()
        };
        assert_eq!(a1, a2);
        assert_ne!(a1, b);
        let mut r = BranchRng::new(&seed, "turn", "");
        assert!((0..1000).all(|_| r.below(7) < 7));
        assert!((0..1000).all(|_| (0.0..1.0).contains(&r.float())));
    }
}
