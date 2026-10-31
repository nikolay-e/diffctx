use super::BoxError;

/// What the model reads: the paths of a snapshot and their bytes.
///
/// The one implementation that matters is `SnapshotReader`; the trait keeps
/// the readers of this module from knowing how a snapshot is stored, and from
/// ever reaching for the disk.
pub trait ManifestSource {
    /// Every admitted path, repo-relative with `/` separators.
    fn paths(&self) -> Result<Vec<String>, BoxError>;

    /// `Ok(None)` is an absent path; `Err` is a read that failed.
    fn read(&self, path: &str) -> Result<Option<Vec<u8>>, BoxError>;

    /// `(path, target)` of every symlink; a symlink's blob is its target.
    fn symlinks(&self) -> Result<Vec<(String, String)>, BoxError> {
        Ok(Vec::new())
    }
}

impl ManifestSource for crate::SnapshotReader {
    fn paths(&self) -> Result<Vec<String>, BoxError> {
        Ok(self
            .list()?
            .entries
            .iter()
            .map(|entry| entry.path.to_string())
            .collect())
    }

    fn read(&self, path: &str) -> Result<Option<Vec<u8>>, BoxError> {
        let path = crate::RepoPath::new(path)?;
        Ok(Self::read(self, &path)?.map(|bytes| bytes.to_vec()))
    }

    fn symlinks(&self) -> Result<Vec<(String, String)>, BoxError> {
        let mut links = Vec::new();
        for (path, reason) in &self.list()?.not_analysed {
            if *reason != crate::NotAnalysedReason::Symlink {
                continue;
            }
            if let Some(target) = Self::read(self, path)? {
                links.push((
                    path.to_string(),
                    String::from_utf8_lossy(&target).into_owned(),
                ));
            }
        }
        Ok(links)
    }
}
