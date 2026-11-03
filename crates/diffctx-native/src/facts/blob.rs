//! The identity of a blob read from the working tree. A tree side names its blobs
//! by git's object id; a working-tree file has none until it is hashed, and
//! hashing the bytes that were read (not asking git to read the file again)
//! keeps the id and the facts about the same bytes. The id is git's own SHA-1
//! `blob` id, so a file the working tree holds unchanged shares its cache entry
//! with the commit it came from.

use crate::snapshot::{Oid, SnapshotError};

/// git's id of a blob holding `bytes` (`git hash-object`, no filters).
pub fn blob_oid(bytes: &[u8]) -> Result<Oid, SnapshotError> {
    let header = format!("blob {}\0", bytes.len());
    let digest = sha1(&[header.as_bytes(), bytes]);
    let hex: String = digest.iter().map(|byte| format!("{byte:02x}")).collect();
    Oid::parse(&hex)
}

struct Sha1 {
    state: [u32; 5],
    block: [u8; 64],
    filled: usize,
}

impl Sha1 {
    fn push(&mut self, byte: u8) {
        self.block[self.filled] = byte;
        self.filled += 1;
        if self.filled == 64 {
            compress(&mut self.state, &self.block);
            self.filled = 0;
        }
    }
}

fn sha1(parts: &[&[u8]]) -> [u8; 20] {
    let mut hash = Sha1 {
        state: [
            0x6745_2301,
            0xefcd_ab89,
            0x98ba_dcfe,
            0x1032_5476,
            0xc3d2_e1f0,
        ],
        block: [0; 64],
        filled: 0,
    };
    let length: u64 = parts.iter().map(|part| part.len() as u64).sum();
    for part in parts {
        for byte in *part {
            hash.push(*byte);
        }
    }
    hash.push(0x80);
    while hash.filled != 56 {
        hash.push(0);
    }
    for byte in (length * 8).to_be_bytes() {
        hash.push(byte);
    }
    let mut out = [0u8; 20];
    for (chunk, word) in out.chunks_mut(4).zip(hash.state) {
        chunk.copy_from_slice(&word.to_be_bytes());
    }
    out
}

fn compress(state: &mut [u32; 5], block: &[u8; 64]) {
    let mut w = [0u32; 80];
    for (i, word) in block.chunks(4).enumerate() {
        w[i] = u32::from_be_bytes([word[0], word[1], word[2], word[3]]);
    }
    for i in 16..80 {
        w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
    }
    let [mut a, mut b, mut c, mut d, mut e] = *state;
    for (i, word) in w.iter().enumerate() {
        let (f, k) = match i {
            0..=19 => ((b & c) | (!b & d), 0x5a82_7999),
            20..=39 => (b ^ c ^ d, 0x6ed9_eba1),
            40..=59 => ((b & c) | (b & d) | (c & d), 0x8f1b_bcdc),
            _ => (b ^ c ^ d, 0xca62_c1d6),
        };
        let next = a
            .rotate_left(5)
            .wrapping_add(f)
            .wrapping_add(e)
            .wrapping_add(k)
            .wrapping_add(*word);
        e = d;
        d = c;
        c = b.rotate_left(30);
        b = a;
        a = next;
    }
    for (slot, value) in state.iter_mut().zip([a, b, c, d, e]) {
        *slot = slot.wrapping_add(value);
    }
}
