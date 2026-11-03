use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::snapshot::error::{Result, SnapshotError};

/// A blob's bytes, shared: a read is handed to several consumers.
pub type Bytes = Arc<[u8]>;

/// A full object id, lowercase hex (40 or 64 digits). Abbreviations are
/// refused: an id that is not the whole object is not an identity.
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
pub struct Oid(String);

impl Oid {
    pub fn parse(hex: &str) -> Result<Self> {
        let hex = hex.trim();
        if matches!(hex.len(), 40 | 64) && hex.bytes().all(|b| b.is_ascii_hexdigit()) {
            Ok(Self(hex.to_ascii_lowercase()))
        } else {
            Err(SnapshotError::InvalidRange(format!(
                "'{hex}' is not a full object id"
            )))
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// git's null id: an unborn branch, a created or deleted ref.
    pub fn is_zero(&self) -> bool {
        self.0.bytes().all(|b| b == b'0')
    }
}

impl fmt::Display for Oid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A path inside the repository: relative, `/`-separated, without `.`, `..`
/// or empty components. The name git stores, never a location on the disk.
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
pub struct RepoPath(String);

impl RepoPath {
    pub fn new(path: &str) -> Result<Self> {
        let ok = !path.is_empty()
            && !path.starts_with('/')
            && !path.contains('\0')
            && path
                .split('/')
                .all(|part| !part.is_empty() && part != "." && part != "..");
        if ok {
            Ok(Self(path.to_string()))
        } else {
            Err(SnapshotError::InvalidPath(path.to_string()))
        }
    }

    /// A name that is not a valid path, kept for disclosure only (an
    /// unaddressable tree entry): never to be joined or read.
    pub(crate) fn unchecked(path: String) -> Self {
        Self(path)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn file_name(&self) -> &str {
        self.0.rsplit('/').next().unwrap_or(&self.0)
    }

    pub fn parent(&self) -> Option<&str> {
        self.0.rsplit_once('/').map(|(parent, _)| parent)
    }

    /// Where the path lies under `root` on the disk.
    pub fn under(&self, root: &Path) -> PathBuf {
        self.0
            .split('/')
            .fold(root.to_path_buf(), |acc, part| acc.join(part))
    }
}

impl fmt::Display for RepoPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Mode {
    Regular,
    Executable,
    Symlink,
    Submodule,
}

impl Mode {
    pub fn is_symlink(self) -> bool {
        self == Self::Symlink
    }

    /// From git's octal mode text (`100644`, `120000`, ...). `None` for the
    /// null mode of an absent side.
    pub fn from_git(octal: &str) -> Option<Self> {
        let Ok(bits) = u32::from_str_radix(octal, 8) else {
            return None;
        };
        match bits & 0o170_000 {
            0o120_000 => Some(Self::Symlink),
            0o160_000 => Some(Self::Submodule),
            0o100_000 if bits & 0o111 != 0 => Some(Self::Executable),
            0o100_000 => Some(Self::Regular),
            _ => None,
        }
    }
}

/// What a snapshot is identified by: a tree oid, or a digest for a side that
/// is not an object (the working tree).
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct ContentId(pub String);

impl fmt::Display for ContentId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct Remote(pub String);

/// A full ref name (`HEAD`, `refs/heads/main`, `refs/remotes/origin/main`).
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct RefName(pub String);

/// A position in a reflog: the byte length already read. Serialises as a
/// number, so a hook can keep it between two of its runs.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub struct Cursor(pub u64);

impl fmt::Display for Cursor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::str::FromStr for Cursor {
    type Err = SnapshotError;

    fn from_str(s: &str) -> Result<Self> {
        s.trim()
            .parse()
            .map(Self)
            .map_err(|_| SnapshotError::Range(format!("'{s}' is not a reflog cursor")))
    }
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ReflogEntry {
    pub old: Oid,
    pub new: Oid,
    /// git's own words for the move: `commit`, `commit (amend)`, `rebase (pick)`,
    /// `update by push`, `checkout`, ...
    pub action: String,
    pub subject: String,
    pub timestamp: i64,
}

/// FNV-1a over parts, twice with different seeds: a 128-bit digest for
/// identities, with no dependency and a value that does not change between
/// toolchains the way `DefaultHasher`'s may.
pub(crate) fn digest(parts: &[&[u8]]) -> String {
    let mut lanes = [0xcbf2_9ce4_8422_2325_u64, 0x8422_2325_cbf2_9ce4_u64];
    for part in parts {
        for lane in &mut lanes {
            for byte in part.iter().chain(&[0xff_u8]) {
                *lane ^= u64::from(*byte);
                *lane = lane.wrapping_mul(0x0100_0000_01b3);
            }
        }
    }
    format!("{:016x}{:016x}", lanes[0], lanes[1])
}

pub(crate) fn fnv64(parts: &[&[u8]]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for part in parts {
        for byte in part.iter().chain(&[0xff_u8]) {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(0x0100_0000_01b3);
        }
    }
    hash
}
