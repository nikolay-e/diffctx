//! The identity of a blob read from the working tree. A tree side names its blobs
//! by git's object id; a working-tree file has none until it is hashed, and
//! hashing the bytes that were read (not asking git to read the file again)
//! keeps the id and the facts about the same bytes. The id is git's own SHA-1
//! `blob` id, so a file the working tree holds unchanged shares its cache entry
//! with the commit it came from.

use diffctx_snapshot::snapshot::{Oid, SnapshotError};

/// git's id of a blob holding `bytes` (`git hash-object`, no filters).
pub fn blob_oid(bytes: &[u8]) -> Result<Oid, SnapshotError> {
    let mut hash = Sha1::new();
    hash.update(format!("blob {}\0", bytes.len()).as_bytes());
    hash.update(bytes);
    let mut hex = String::with_capacity(40);
    for byte in hash.finish() {
        for nibble in [byte >> 4, byte & 15] {
            hex.push(char::from_digit(u32::from(nibble), 16).unwrap_or('0'));
        }
    }
    Oid::parse(&hex)
}

/// SHA-1 over whole 64-byte blocks: a block is compressed straight from the
/// input, and only the partial block at either end is copied.
struct Sha1 {
    state: [u32; 5],
    block: [u8; 64],
    filled: usize,
    length: u64,
}

impl Sha1 {
    fn new() -> Self {
        Self {
            state: [
                0x6745_2301,
                0xefcd_ab89,
                0x98ba_dcfe,
                0x1032_5476,
                0xc3d2_e1f0,
            ],
            block: [0; 64],
            filled: 0,
            length: 0,
        }
    }

    fn update(&mut self, mut bytes: &[u8]) {
        self.length += bytes.len() as u64;
        if self.filled > 0 {
            let take = bytes.len().min(64 - self.filled);
            self.block[self.filled..self.filled + take].copy_from_slice(&bytes[..take]);
            self.filled += take;
            bytes = &bytes[take..];
            if self.filled < 64 {
                return;
            }
            compress(&mut self.state, &self.block);
            self.filled = 0;
        }
        let blocks = bytes.chunks_exact(64);
        let tail = blocks.remainder();
        for block in blocks {
            if let Ok(block) = <&[u8; 64]>::try_from(block) {
                compress(&mut self.state, block);
            }
        }
        self.block[..tail.len()].copy_from_slice(tail);
        self.filled = tail.len();
    }

    fn finish(mut self) -> [u8; 20] {
        let bits = self.length * 8;
        self.block[self.filled] = 0x80;
        self.block[self.filled + 1..].fill(0);
        if self.filled + 1 > 56 {
            compress(&mut self.state, &self.block);
            self.block.fill(0);
        }
        self.block[56..].copy_from_slice(&bits.to_be_bytes());
        compress(&mut self.state, &self.block);
        let mut out = [0u8; 20];
        for (chunk, word) in out.chunks_mut(4).zip(self.state) {
            chunk.copy_from_slice(&word.to_be_bytes());
        }
        out
    }
}

/// One round: `e` takes the round's sum and `b` its rotation, so five rounds in
/// a row turn the five registers' roles without moving a value.
macro_rules! round {
    ($a:ident, $b:ident, $e:ident, $f:expr, $k:expr, $word:expr) => {
        $e = $e
            .wrapping_add($a.rotate_left(5))
            .wrapping_add($f)
            .wrapping_add($k)
            .wrapping_add($word);
        $b = $b.rotate_left(30);
    };
}

/// Twenty rounds of one boolean function and constant.
macro_rules! phase {
    ($words:expr, $regs:expr, $k:expr, $mix:ident) => {{
        let [mut a, mut b, mut c, mut d, mut e] = *$regs;
        for group in $words.chunks_exact(5) {
            round!(a, b, e, $mix(b, c, d), $k, group[0]);
            round!(e, a, d, $mix(a, b, c), $k, group[1]);
            round!(d, e, c, $mix(e, a, b), $k, group[2]);
            round!(c, d, b, $mix(d, e, a), $k, group[3]);
            round!(b, c, a, $mix(c, d, e), $k, group[4]);
        }
        *$regs = [a, b, c, d, e];
    }};
}

fn choose(x: u32, y: u32, z: u32) -> u32 {
    (x & y) | (!x & z)
}

fn parity(x: u32, y: u32, z: u32) -> u32 {
    x ^ y ^ z
}

fn majority(x: u32, y: u32, z: u32) -> u32 {
    (x & y) | (x & z) | (y & z)
}

fn compress(state: &mut [u32; 5], block: &[u8; 64]) {
    let mut w = [0u32; 80];
    for (slot, word) in w.iter_mut().zip(block.chunks_exact(4)) {
        *slot = u32::from_be_bytes([word[0], word[1], word[2], word[3]]);
    }
    for i in 16..80 {
        w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
    }
    let mut next = *state;
    phase!(&w[0..20], &mut next, 0x5a82_7999_u32, choose);
    phase!(&w[20..40], &mut next, 0x6ed9_eba1_u32, parity);
    phase!(&w[40..60], &mut next, 0x8f1b_bcdc_u32, majority);
    phase!(&w[60..80], &mut next, 0xca62_c1d6_u32, parity);
    for (slot, value) in state.iter_mut().zip(next) {
        *slot = slot.wrapping_add(value);
    }
}
