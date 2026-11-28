//! BLAKE3 (the default 32-byte hash of the BLAKE3 specification, no key, no derivation
//! context), written out here because the workspace takes no external crates. The tests check
//! the digests the reference implementation gives for the standard test inputs (the bytes
//! `i mod 251`) at lengths that cross every boundary of the tree: blocks (64), chunks (1024)
//! and powers of two of chunks.
//!
//! The hash is computed over a byte slice in one pass: each 1024-byte chunk is compressed block
//! by block into a chaining value, and chaining values are merged as a left-complete binary
//! tree with a stack of at most 54 entries. Work is linear in the input; nothing is allocated
//! beyond that stack.

#![allow(
    clippy::cast_possible_truncation,
    clippy::chunks_exact_to_as_chunks,
    reason = "the specification's 32-bit words from 64-bit counters and fixed block sizes"
)]

const IV: [u32; 8] = [
    0x6A09_E667,
    0xBB67_AE85,
    0x3C6E_F372,
    0xA54F_F53A,
    0x510E_527F,
    0x9B05_688C,
    0x1F83_D9AB,
    0x5BE0_CD19,
];

const MSG_PERMUTATION: [usize; 16] = [2, 6, 3, 10, 7, 0, 4, 13, 1, 11, 12, 5, 9, 14, 15, 8];

const CHUNK_START: u32 = 1;
const CHUNK_END: u32 = 2;
const PARENT: u32 = 4;
const ROOT: u32 = 8;

const BLOCK_LEN: usize = 64;
const CHUNK_LEN: usize = 1024;

#[allow(clippy::many_single_char_names, reason = "the specification's names")]
fn g(v: &mut [u32; 16], a: usize, b: usize, c: usize, d: usize, mx: u32, my: u32) {
    v[a] = v[a].wrapping_add(v[b]).wrapping_add(mx);
    v[d] = (v[d] ^ v[a]).rotate_right(16);
    v[c] = v[c].wrapping_add(v[d]);
    v[b] = (v[b] ^ v[c]).rotate_right(12);
    v[a] = v[a].wrapping_add(v[b]).wrapping_add(my);
    v[d] = (v[d] ^ v[a]).rotate_right(8);
    v[c] = v[c].wrapping_add(v[d]);
    v[b] = (v[b] ^ v[c]).rotate_right(7);
}

fn round(v: &mut [u32; 16], m: &[u32; 16]) {
    // Columns, then diagonals.
    g(v, 0, 4, 8, 12, m[0], m[1]);
    g(v, 1, 5, 9, 13, m[2], m[3]);
    g(v, 2, 6, 10, 14, m[4], m[5]);
    g(v, 3, 7, 11, 15, m[6], m[7]);
    g(v, 0, 5, 10, 15, m[8], m[9]);
    g(v, 1, 6, 11, 12, m[10], m[11]);
    g(v, 2, 7, 8, 13, m[12], m[13]);
    g(v, 3, 4, 9, 14, m[14], m[15]);
}

/// The compression function: the 16 output words.
fn compress(
    cv: &[u32; 8],
    block: &[u32; 16],
    counter: u64,
    block_len: u32,
    flags: u32,
) -> [u32; 16] {
    let mut v = [
        cv[0],
        cv[1],
        cv[2],
        cv[3],
        cv[4],
        cv[5],
        cv[6],
        cv[7],
        IV[0],
        IV[1],
        IV[2],
        IV[3],
        counter as u32,
        (counter >> 32) as u32,
        block_len,
        flags,
    ];
    let mut m = *block;
    for r in 0..7 {
        round(&mut v, &m);
        if r < 6 {
            let mut permuted = [0u32; 16];
            for (i, p) in MSG_PERMUTATION.iter().enumerate() {
                permuted[i] = m[*p];
            }
            m = permuted;
        }
    }
    for i in 0..8 {
        v[i] ^= v[i + 8];
        v[i + 8] ^= cv[i];
    }
    v
}

fn first_eight(w: [u32; 16]) -> [u32; 8] {
    [w[0], w[1], w[2], w[3], w[4], w[5], w[6], w[7]]
}

/// A block of up to 64 bytes as 16 little-endian words, padded with zeros.
fn words(bytes: &[u8]) -> [u32; 16] {
    let mut padded = [0u8; BLOCK_LEN];
    padded[..bytes.len()].copy_from_slice(bytes);
    let mut w = [0u32; 16];
    for (i, c) in padded.chunks_exact(4).enumerate() {
        w[i] = u32::from_le_bytes([c[0], c[1], c[2], c[3]]);
    }
    w
}

/// The state of a node just before its last compression, which is the root's when it is the
/// last node.
struct Output {
    cv: [u32; 8],
    block: [u32; 16],
    counter: u64,
    block_len: u32,
    flags: u32,
}

impl Output {
    fn chaining_value(&self) -> [u32; 8] {
        first_eight(compress(
            &self.cv,
            &self.block,
            self.counter,
            self.block_len,
            self.flags,
        ))
    }

    /// The first 32 bytes of the root output.
    fn root(&self) -> [u8; 32] {
        let w = compress(&self.cv, &self.block, 0, self.block_len, self.flags | ROOT);
        let mut out = [0u8; 32];
        for (i, word) in w.iter().take(8).enumerate() {
            out[4 * i..4 * i + 4].copy_from_slice(&word.to_le_bytes());
        }
        out
    }
}

/// A chunk (at most 1024 bytes), compressed up to its last block.
fn chunk_output(chunk: &[u8], counter: u64) -> Output {
    let blocks = chunk.len().div_ceil(BLOCK_LEN).max(1);
    let mut cv = IV;
    for b in 0..blocks - 1 {
        let block = words(&chunk[b * BLOCK_LEN..(b + 1) * BLOCK_LEN]);
        let flags = if b == 0 { CHUNK_START } else { 0 };
        cv = first_eight(compress(&cv, &block, counter, BLOCK_LEN as u32, flags));
    }
    let last = &chunk[(blocks - 1) * BLOCK_LEN..];
    Output {
        cv,
        block: words(last),
        counter,
        block_len: last.len() as u32,
        flags: CHUNK_END | if blocks == 1 { CHUNK_START } else { 0 },
    }
}

fn parent_output(left: [u32; 8], right: [u32; 8]) -> Output {
    let mut block = [0u32; 16];
    block[..8].copy_from_slice(&left);
    block[8..].copy_from_slice(&right);
    Output {
        cv: IV,
        block,
        counter: 0,
        block_len: BLOCK_LEN as u32,
        flags: PARENT,
    }
}

/// The BLAKE3 hash of `input`.
#[must_use]
pub(crate) fn hash(input: &[u8]) -> [u8; 32] {
    let chunks = input.len().div_ceil(CHUNK_LEN).max(1);
    let mut stack: Vec<[u32; 8]> = Vec::with_capacity(54);
    for i in 0..chunks - 1 {
        let chunk = &input[i * CHUNK_LEN..(i + 1) * CHUNK_LEN];
        let mut cv = chunk_output(chunk, i as u64).chaining_value();
        // Merge finished subtrees: one per trailing zero bit of the chunk count so far.
        let mut total = i as u64 + 1;
        while total & 1 == 0 {
            let left = stack.pop().unwrap_or(IV);
            cv = parent_output(left, cv).chaining_value();
            total >>= 1;
        }
        stack.push(cv);
    }
    let last = &input[(chunks - 1) * CHUNK_LEN..];
    let mut output = chunk_output(last, (chunks - 1) as u64);
    while let Some(left) = stack.pop() {
        output = parent_output(left, output.chaining_value());
    }
    output.root()
}

#[cfg(test)]
mod tests {
    use super::hash;

    fn hex(bytes: &[u8]) -> String {
        ratio_check::hex(bytes)
    }

    fn pattern(len: usize) -> Vec<u8> {
        (0..len).map(|i| (i % 251) as u8).collect()
    }

    /// Digests from the reference implementation (`blake3` 1.8.7) of the test inputs.
    #[test]
    fn the_reference_digests() {
        let cases: [(usize, &str); 32] = [
            (
                0,
                "af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262",
            ),
            (
                1,
                "2d3adedff11b61f14c886e35afa036736dcd87a74d27b5c1510225d0f592e213",
            ),
            (
                2,
                "7b7015bb92cf0b318037702a6cdd81dee41224f734684c2c122cd6359cb1ee63",
            ),
            (
                3,
                "e1be4d7a8ab5560aa4199eea339849ba8e293d55ca0a81006726d184519e647f",
            ),
            (
                4,
                "f30f5ab28fe047904037f77b6da4fea1e27241c5d132638d8bedce9d40494f32",
            ),
            (
                5,
                "b40b44dfd97e7a84a996a91af8b85188c66c126940ba7aad2e7ae6b385402aa2",
            ),
            (
                63,
                "e9bc37a594daad83be9470df7f7b3798297c3d834ce80ba85d6e207627b7db7b",
            ),
            (
                64,
                "4eed7141ea4a5cd4b788606bd23f46e212af9cacebacdc7d1f4c6dc7f2511b98",
            ),
            (
                65,
                "de1e5fa0be70df6d2be8fffd0e99ceaa8eb6e8c93a63f2d8d1c30ecb6b263dee",
            ),
            (
                127,
                "d81293fda863f008c09e92fc382a81f5a0b4a1251cba1634016a0f86a6bd640d",
            ),
            (
                128,
                "f17e570564b26578c33bb7f44643f539624b05df1a76c81f30acd548c44b45ef",
            ),
            (
                129,
                "683aaae9f3c5ba37eaaf072aed0f9e30bac0865137bae68b1fde4ca2aebdcb12",
            ),
            (
                1023,
                "10108970eeda3eb932baac1428c7a2163b0e924c9a9e25b35bba72b28f70bd11",
            ),
            (
                1024,
                "42214739f095a406f3fc83deb889744ac00df831c10daa55189b5d121c855af7",
            ),
            (
                1025,
                "d00278ae47eb27b34faecf67b4fe263f82d5412916c1ffd97c8cb7fb814b8444",
            ),
            (
                2047,
                "58830fbf51a4423c573b164471690570e544cfe793bead46225664796b4b1467",
            ),
            (
                2048,
                "e776b6028c7cd22a4d0ba182a8bf62205d2ef576467e838ed6f2529b85fba24a",
            ),
            (
                2049,
                "5f4d72f40d7a5f82b15ca2b2e44b1de3c2ef86c426c95c1af0b6879522563030",
            ),
            (
                3072,
                "b98cb0ff3623be03326b373de6b9095218513e64f1ee2edd2525c7ad1e5cffd2",
            ),
            (
                3073,
                "7124b49501012f81cc7f11ca069ec9226cecb8a2c850cfe644e327d22d3e1cd3",
            ),
            (
                4096,
                "015094013f57a5277b59d8475c0501042c0b642e531b0a1c8f58d2163229e969",
            ),
            (
                4097,
                "9b4052b38f1c5fc8b1f9ff7ac7b27cd242487b3d890d15c96a1c25b8aa0fb995",
            ),
            (
                5120,
                "9cadc15fed8b5d854562b26a9536d9707cadeda9b143978f319ab34230535833",
            ),
            (
                5121,
                "628bd2cb2004694adaab7bbd778a25df25c47b9d4155a55f8fbd79f2fe154cff",
            ),
            (
                6144,
                "3e2e5b74e048f3add6d21faab3f83aa44d3b2278afb83b80b3c35164ebeca205",
            ),
            (
                6145,
                "f1323a8631446cc50536a9f705ee5cb619424d46887f3c376c695b70e0f0507f",
            ),
            (
                7168,
                "61da957ec2499a95d6b8023e2b0e604ec7f6b50e80a9678b89d2628e99ada77a",
            ),
            (
                7169,
                "a003fc7a51754a9b3c7fae0367ab3d782dccf28855a03d435f8cfe74605e7817",
            ),
            (
                8192,
                "aae792484c8efe4f19e2ca7d371d8c467ffb10748d8a5a1ae579948f718a2a63",
            ),
            (
                8193,
                "bab6c09cb8ce8cf459261398d2e7aef35700bf488116ceb94a36d0f5f1b7bc3b",
            ),
            (
                31744,
                "62b6960e1a44bcc1eb1a611a8d6235b6b4b78f32e7abc4fb4c6cdcce94895c47",
            ),
            (
                102_400,
                "bc3e3d41a1146b069abffad3c0d44860cf664390afce4d9661f7902e7943e085",
            ),
        ];
        for (len, want) in cases {
            assert_eq!(hex(&hash(&pattern(len))), want, "{len} bytes");
        }
    }

    #[test]
    fn text_digests() {
        assert_eq!(
            hex(&hash(b"abc")),
            "6437b3ac38465133ffb63b75273a8db548c558465d79db03fd359c6cd5bd9d85"
        );
        assert_eq!(
            hex(&hash(b"hello world")),
            "d74981efa70a0c880b8d8c1985d075dbcbf679b99a5f9914e5aaf96b831a9e24"
        );
    }
}
