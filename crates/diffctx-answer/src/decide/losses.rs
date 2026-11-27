//! What a loss of the snapshot reader does to a claim: its effect, what stopped the run, and the losses
//! a cell holds.

use std::collections::HashMap;

use diffctx_snapshot::resource::LimitReason;

use diffctx_snapshot::snapshot::{Loss, NotAnalysedReason, RepoPath, SideTag};

use super::gap::{Gap, Reason};

/// What a loss does to an answer: leaves one file unaccounted, the files under a
/// directory, or the whole run.
pub(crate) enum Effect {
    File(Reason),
    Subtree(Reason, String),
    Run(Gap),
}

/// The one reading of a loss. A new variant of `Loss` does not compile until it
/// is decided here.
pub(crate) fn effect_of(loss: &Loss) -> Effect {
    match loss {
        Loss::ReadFailed { .. } => Effect::File(Reason::ReadFailed),
        Loss::SnapshotChanged { .. } => Effect::File(Reason::Changed),
        Loss::TooLarge { .. } => Effect::File(Reason::TooLarge),
        Loss::NotAnalysed { reason, .. } => Effect::File(match reason {
            NotAnalysedReason::Oversize => Reason::TooLarge,
            NotAnalysedReason::Symlink
            | NotAnalysedReason::Submodule
            | NotAnalysedReason::Untracked
            | NotAnalysedReason::Unaddressable => Reason::NotRead,
        }),
        Loss::ParseError { .. } | Loss::GrammarOutdated { .. } => Effect::File(Reason::Unparsed),
        Loss::Unsupported { .. } => Effect::File(Reason::NoExtractor),
        Loss::ExtractorFailed { .. } => Effect::File(Reason::Failed),
        Loss::AmbiguousMatch { .. } => Effect::File(Reason::Ambiguous),
        Loss::ManifestUnread { path, .. } => {
            Effect::Subtree(Reason::Manifest, path.parent().unwrap_or("").to_string())
        }
        Loss::Deadline { .. } => Effect::Run(Gap::Stopped {
            reasons: vec![LimitReason::Deadline],
        }),
        Loss::Withheld { count } => Effect::Run(Gap::Withheld { files: *count }),
        Loss::DirectoryUnread { .. } => Effect::Run(Gap::Stopped {
            reasons: vec![LimitReason::DiscoveryTruncated],
        }),
    }
}

/// The reasons that leave a search a lower bound: a read that failed or a
/// deadline.
pub(super) fn stopped_by(losses: &[Loss]) -> Vec<LimitReason> {
    let mut out = Vec::new();
    for loss in losses {
        let reasons = match effect_of(loss) {
            Effect::Run(Gap::Stopped { reasons }) => reasons,
            Effect::File(Reason::ReadFailed) => vec![LimitReason::ReadFailed],
            _ => continue,
        };
        for reason in reasons {
            if !out.contains(&reason) {
                out.push(reason);
            }
        }
    }
    out
}

/// What a set of losses does, by where it lands.
#[derive(Default)]
pub(super) struct Losses {
    files: HashMap<String, (Reason, Loss)>,
    subtrees: Vec<(String, Reason, Loss)>,
}

impl Losses {
    pub(super) fn of<'a>(losses: impl IntoIterator<Item = &'a Loss>) -> Self {
        let mut out = Self::default();
        for loss in losses {
            match effect_of(loss) {
                Effect::File(reason) => {
                    if let Some(path) = loss.path() {
                        (out.files.entry(path.as_str().to_string()))
                            .or_insert((reason, loss.clone()));
                    }
                }
                Effect::Subtree(reason, dir) => out.subtrees.push((dir, reason, loss.clone())),
                Effect::Run(_) => {}
            }
        }
        out
    }

    /// Why a loss about `file` leaves it unaccounted, and the loss: the first one.
    pub(super) fn about(&self, file: &str) -> Option<(Reason, &Loss)> {
        (self.files.get(file).map(|(reason, loss)| (*reason, loss))).or_else(|| {
            self.subtrees
                .iter()
                .find(|(dir, ..)| {
                    dir.is_empty()
                        || file
                            .strip_prefix(dir.as_str())
                            .is_some_and(|rest| rest.starts_with('/'))
                })
                .map(|(_, reason, loss)| (*reason, loss))
        })
    }
}

/// A file the run lost is unread by the occurrence account, whatever its facts say.
pub(super) fn lost_in<'a>(
    fixed: Option<&'a Losses>,
    moving: &'a Losses,
) -> impl Fn(&RepoPath) -> Option<Loss> + 'a {
    move |file| match fixed {
        None => Some(Loss::ReadFailed {
            path: file.clone(),
            side: SideTag::Head,
        }),
        Some(fixed) => (fixed
            .about(file.as_str())
            .or_else(|| moving.about(file.as_str())))
        .map(|(_, loss)| loss.clone()),
    }
}
