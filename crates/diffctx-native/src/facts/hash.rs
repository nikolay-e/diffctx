//! The hashes extractors fill `Def` with. FNV-1a, so a value is the same on every
//! toolchain and platform and a cached `Def` stays comparable with a fresh one.

const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
const PRIME: u64 = 0x0100_0000_01b3;

/// A streaming hash of tokens or bytes. Each [`Hasher::token`] is delimited, so
/// `["ab", "c"]` and `["a", "bc"]` differ.
#[derive(Clone, Copy, Debug)]
pub struct Hasher {
    state: u64,
    any: bool,
}

impl Default for Hasher {
    fn default() -> Self {
        Self::new()
    }
}

impl Hasher {
    pub const fn new() -> Self {
        Self {
            state: OFFSET,
            any: false,
        }
    }

    /// Mixes raw bytes in, with no delimiter.
    pub fn bytes(&mut self, bytes: &[u8]) -> &mut Self {
        self.any = true;
        self.state = mix(self.state, bytes);
        self
    }

    /// Mixes one token in, delimited from the next.
    pub fn token(&mut self, token: &[u8]) -> &mut Self {
        self.bytes(token);
        self.state = mix(self.state, &[0xff]);
        self
    }

    /// Never `0`: `0` is the value of a `Def` without a body. `None` when nothing
    /// was mixed in.
    pub fn finish(&self) -> Option<u64> {
        self.any.then(|| nonzero(self.state))
    }
}

/// The bytes as they stand, whitespace and comments included: `Def::text_hash`.
pub fn hash_text(text: &[u8]) -> u64 {
    nonzero(mix(OFFSET, text))
}

/// A token stream, whatever lay between the tokens left out: `Def::body_hash`
/// and `Def::doc_hash`. `None` for no tokens at all.
pub fn hash_tokens<'a>(tokens: impl IntoIterator<Item = &'a [u8]>) -> Option<u64> {
    let mut hasher = Hasher::new();
    for token in tokens {
        hasher.token(token);
    }
    hasher.finish()
}

fn mix(mut hash: u64, bytes: &[u8]) -> u64 {
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(PRIME);
    }
    hash
}

fn nonzero(hash: u64) -> u64 {
    if hash == 0 { 1 } else { hash }
}

/// 128 bits as hex: a stored entry's checksum.
pub(crate) fn checksum(bytes: &[u8]) -> String {
    let low = mix(OFFSET, bytes);
    let high = mix(OFFSET ^ 0x8422_2325_cbf2_9ce4, bytes);
    format!("{high:016x}{low:016x}")
}
