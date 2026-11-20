//! What may be shown, decided once. The facade kept four predicates for it
//! (`is_secret_path`, `is_ignored_path`, `is_withheld`, `withheld_paths`) that
//! the readers composed differently (#507); the reader here asks `decide`
//! for every entry of a snapshot and answers only for what it admits.

use std::path::Path;

use crate::snapshot::types::{Mode, RepoPath, fnv64};

/// Why a path is kept out. Never shown next to the path: a withheld path is
/// counted, not named.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum WithholdReason {
    /// Private-key, keystore and credential files, by file name.
    Secret,
    /// Inside git's own directory.
    RepoInternal,
    /// Excluded by `.gitignore` as git itself resolves it.
    Gitignore,
    /// Excluded by `.diffctx/ignore`, a declared confidentiality policy.
    DiffctxPolicy,
}

/// Why a path is listed but not read as source.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum NotAnalysedReason {
    /// A link is its target's name, not content; it is followed only inside
    /// the snapshot, through `SnapshotReader::resolve_link`.
    Symlink,
    Submodule,
    /// Larger than the admission's size cap.
    Oversize,
    /// Untracked, and the policy admits tracked files only.
    Untracked,
    /// The name is not UTF-8, or not a path inside the repository, so nothing
    /// can address it.
    Unaddressable,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Decision {
    Admit,
    Withhold(WithholdReason),
    NotAnalysed(NotAnalysedReason),
}

/// Which rule family excluded a path.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum IgnoreSource {
    Gitignore,
    DiffctxPolicy,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum UntrackedPolicy {
    /// Untracked files that no ignore rule excludes are part of a
    /// working-tree snapshot, as `git ls-files --others --exclude-standard`
    /// lists them.
    Admit,
    Exclude,
}

/// The facts `decide` weighs, gathered by the reader.
///
/// `ignored` is what the
/// ignore rules said for this entry, already read the way the entry's
/// tracked-ness requires: a tracked file under a directory a rule excludes
/// is not ignored (#153).
#[derive(Clone, Copy, Debug)]
pub struct Meta {
    pub mode: Mode,
    pub size: u64,
    pub tracked: bool,
    pub ignored: Option<IgnoreSource>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct AdmissionPolicy {
    pub size_cap: u64,
    pub untracked: UntrackedPolicy,
}

impl Default for AdmissionPolicy {
    fn default() -> Self {
        Self {
            size_cap: crate::snapshot::MAX_BLOB_READ_BYTES,
            untracked: UntrackedPolicy::Admit,
        }
    }
}

const SECRET_FLOOR_VERSION: &[u8] = b"secret-floor/1";

#[derive(Clone, Debug, Default)]
pub struct Admission {
    policy: AdmissionPolicy,
}

impl Admission {
    pub fn new(policy: AdmissionPolicy) -> Self {
        Self { policy }
    }

    pub fn policy(&self) -> &AdmissionPolicy {
        &self.policy
    }

    /// The decision for one path. The secret and repository-internal floor
    /// holds before anything else; `.env` is kept deliberately (a changed
    /// `.env` is legitimate change context).
    pub fn decide(&self, path: &RepoPath, meta: &Meta) -> Decision {
        if let Some(floor) = floor(path) {
            return Decision::Withhold(floor);
        }
        match meta.ignored {
            Some(IgnoreSource::DiffctxPolicy) => {
                return Decision::Withhold(WithholdReason::DiffctxPolicy);
            }
            Some(IgnoreSource::Gitignore) => return Decision::Withhold(WithholdReason::Gitignore),
            None => {}
        }
        match meta.mode {
            Mode::Symlink => Decision::NotAnalysed(NotAnalysedReason::Symlink),
            Mode::Submodule => Decision::NotAnalysed(NotAnalysedReason::Submodule),
            Mode::Regular | Mode::Executable => {
                if !meta.tracked && self.policy.untracked == UntrackedPolicy::Exclude {
                    Decision::NotAnalysed(NotAnalysedReason::Untracked)
                } else if meta.size > self.policy.size_cap {
                    Decision::NotAnalysed(NotAnalysedReason::Oversize)
                } else {
                    Decision::Admit
                }
            }
        }
    }

    /// What the decisions depend on besides the paths: a context's identity
    /// folds it in, so a changed policy never reuses an answer.
    pub fn hash(&self) -> u64 {
        fnv64(&[
            SECRET_FLOOR_VERSION,
            &self.policy.size_cap.to_le_bytes(),
            &[self.policy.untracked as u8],
        ])
    }
}

/// The part of the decision that needs no lookup: the reader skips the ignore
/// query for such paths.
pub(crate) fn floor(path: &RepoPath) -> Option<WithholdReason> {
    if is_secret_path(Path::new(path.as_str())) {
        Some(WithholdReason::Secret)
    } else if is_repo_internal(path.as_str()) {
        Some(WithholdReason::RepoInternal)
    } else {
        None
    }
}

/// THE secret-path policy, for every surface.
///
/// Private-key, keystore and
/// credential files never reach LLM-bound output: not as diff context, not in
/// a tree map, not through an MCP fetch. Matches by file name only, so public
/// keys (`*.pub`) stay visible. `.env` is intentionally NOT here.
pub fn is_secret_path(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
        return false;
    };
    // Whole-name matches. `_sk` is the sealed-secret half of an SSH key pair
    // written by `ssh-keygen -O`; `.netrc`/`credentials` carry passwords in
    // plain text and are the shapes CI images most often leak.
    if matches!(
        name,
        "id_rsa"
            | "id_dsa"
            | "id_ecdsa"
            | "id_ed25519"
            | "id_ed25519_sk"
            | "id_ecdsa_sk"
            | ".netrc"
            | "_netrc"
            | "credentials"
            | ".npmrc"
            | ".pypirc"
    ) {
        return true;
    }
    matches!(
        path.extension().and_then(|e| e.to_str()),
        // `.ppk` is PuTTY's private key, `.p8` Apple's signing key, `.asc` an
        // armoured PGP export: private-key containers the first list happened
        // not to name.
        Some("pem" | "key" | "pfx" | "p12" | "keystore" | "jks" | "ppk" | "p8" | "asc")
    )
}

/// git's own directory. It is not ignored (git never reports on paths inside
/// it) and not secret by name, yet `.git/config` carries the remote URL,
/// which routinely embeds an access token, and `.git/logs/HEAD` the whole
/// branch history. Nothing that reads repository content has business there.
fn is_repo_internal(rel: &str) -> bool {
    rel.split('/').any(|part| part == ".git")
}
