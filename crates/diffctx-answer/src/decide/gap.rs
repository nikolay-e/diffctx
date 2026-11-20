//! Why an answer is open: the sites and routes of a use, the gaps that keep a search from being complete
//! and the reasons an account of names left something unexplained.

use diffctx_snapshot::resource::LimitReason;

/// How the answer words a candidate use.
#[derive(
    serde::Serialize, schemars::JsonSchema, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug,
)]
#[serde(rename_all = "snake_case")]
pub enum Via {
    Call,
    Read,
    Unbound,
    Embedded,
}

/// A candidate use: where it is, and what it may be.
#[derive(serde::Serialize, schemars::JsonSchema, Clone, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub struct Site {
    pub path: String,
    pub line: u32,
    pub via: Via,
}

/// Why a definition's answer is incomplete.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Gap {
    /// `uses` of the name that nothing resolves or rules out may be uses of the
    /// definition: `sites` are the first of them, the rest are counted.
    Candidates { uses: usize, sites: Vec<Site> },
    /// The range pairs the definition's two sides ambiguously.
    Unpaired,
    /// The base side of the range could not be resolved: what still names a
    /// removed or moved definition is the base's question, and it went unasked.
    BaseUnresolved { reason: String },
    /// The run stopped (a deadline, a failed read) before the search finished.
    Stopped { reasons: Vec<LimitReason> },
    /// The run is limited to pathspecs, and `files` outside them could name the
    /// definition: their uses were left out.
    OutsideScope { files: usize },
    /// `files` of the definition's interaction group could neither be bound by
    /// the cell nor ruled out by the names they spell: a file the cell did not
    /// read, or read with a loss, that spells one of the names.
    Unaccounted { reason: Reason, files: usize },
    /// Files the run withheld by ignore rules were not read.
    Withheld { files: usize },
    /// Changed code in `files` lies outside every tracked definition (a
    /// module-level statement, a `__main__` block): what it changes is not traced.
    Untracked { files: usize },
    /// A trait's member, or the implementation of one: calls arrive through
    /// `dyn Trait` and generic bounds, which only candidates follow.
    Dispatched { via: String },
    /// `files` that could name the definition reach it through macros or define it
    /// under conditional compilation (Rust: `cfg_if!`, which defines items by
    /// expansion): what the preprocessor makes of it is not read.
    Macros { files: usize, what: &'static str },
    /// The answer cap shed the definition: its callers were found, not listed.
    Capped,
    /// A route no model traces can reach it: dispatch, an untyped receiver, a
    /// language no binding model reads.
    NotTraced,
    /// The definition is handed on as an argument: what the callees do with it is not
    /// followed. `to` are the callees the text names, sorted.
    PassedAsValue { to: Vec<String> },
    /// A language construct calls it without its name written (an iteration, a
    /// conversion, a context manager, a constructor).
    ImplicitlyConstructed,
    /// `files` hold a template (Helm, Go template files) that was read around,
    /// not expanded: what it renders is not known.
    Templated { files: usize },
    /// `files` of a chart or release whose values or context are not determined:
    /// a key that no release is known to read is not an unread key.
    ContextUnbound { files: usize },
    /// The object is generated (a Kustomize generator, `count` or `for_each`,
    /// a rendered chart): its name is not written in any file.
    #[allow(dead_code, reason = "the config cells of wave 7 build it")]
    RenderedElsewhere { files: usize },
    /// `pointers` name a source outside the repository (a remote repository,
    /// a registry module, an image): what they reach is not held.
    ExternalSource { pointers: usize },
    /// `files` run or read a path the shell computes at run time (`source
    /// "$x"`, `eval`): what they reach is not named in the text.
    SourceFlag { files: usize },
    /// Untracked directories the read of the checkout did not enter, `total` of
    /// them, `directories` the first named: a use in them is not known.
    UnreadDirectories {
        directories: Vec<String>,
        total: usize,
    },
    /// The name is shorter than the literal floor or as common as `main`: where
    /// a script, a template or a string spells it was not searched.
    NameUnsearched,
}

impl Gap {
    /// A candidate gap that counts more uses than it names, without the names:
    /// whether it had any.
    pub(crate) fn shed_sites(&mut self) -> bool {
        match self {
            Self::Candidates { uses, sites } if *uses > sites.len() && !sites.is_empty() => {
                sites.clear();
                true
            }
            _ => false,
        }
    }
}

impl std::fmt::Display for Gap {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Candidates { uses, .. } => write!(f, "{uses} use(s) no binding settles"),
            Self::Unpaired => f.write_str("its two sides of the range pair ambiguously"),
            Self::BaseUnresolved { reason } => {
                write!(f, "the base side of the range was not resolved ({reason})")
            }
            Self::Stopped { reasons } => {
                write!(
                    f,
                    "the run was limited ({})",
                    crate::impact::output::reason_names(reasons)
                )
            }
            Self::OutsideScope { files } => {
                write!(
                    f,
                    "{files} file(s) outside the run's pathspecs were left out"
                )
            }
            Self::Unaccounted { reason, files: 0 } => {
                write!(f, "no file of its interaction group was read: {reason}")
            }
            Self::Unaccounted { reason, files } => {
                write!(
                    f,
                    "{files} file(s) of its interaction group not read: {reason}"
                )
            }
            Self::Withheld { files } => write!(f, "{files} file(s) withheld by ignore rules"),
            Self::Untracked { files } => write!(
                f,
                "changed code outside any tracked definition in {files} file(s)"
            ),
            Self::Dispatched { via } => write!(
                f,
                "it is a member of `{via}`, whose calls through `dyn {via}` or a bound are candidates"
            ),
            Self::Macros { files, what } => {
                write!(f, "{files} file(s) that could name it {what}")
            }
            Self::Capped => f.write_str("the answer cap shed it"),
            Self::NotTraced => f.write_str("a route no model traces can reach it"),
            Self::PassedAsValue { to } => write_passed_as_value(f, to),
            Self::ImplicitlyConstructed => f.write_str("constructed implicitly"),
            Self::Templated { files } => {
                write!(f, "{files} template file(s) were read around, not expanded")
            }
            Self::ContextUnbound { files } => write!(
                f,
                "{files} file(s) of a chart or release whose context is not determined"
            ),
            Self::RenderedElsewhere { files } => write!(
                f,
                "{files} file(s) generate objects whose names are not written in any file"
            ),
            Self::ExternalSource { pointers } => write!(
                f,
                "{pointers} source(s) outside the repository were not followed"
            ),
            Self::SourceFlag { files } => {
                write!(f, "{files} file(s) read a path computed at run time")
            }
            Self::UnreadDirectories { directories, total } => {
                write!(f, "{total} untracked director(y/ies) not read: ")?;
                f.write_str(&directories.join(", "))?;
                match total.saturating_sub(directories.len()) {
                    0 => Ok(()),
                    more => write!(f, " and {more} more"),
                }
            }
            Self::NameUnsearched => f.write_str(
                "its name is too short or too common to search as text: a script, a template or a string that spells it was not looked for",
            ),
        }
    }
}

/// The reason a value handed to callees is not followed, in the answer's words: the
/// callees by name, the first three, then a count.
pub(crate) fn write_passed_as_value(
    f: &mut std::fmt::Formatter<'_>,
    to: &[String],
) -> std::fmt::Result {
    f.write_str("passed as a value to ")?;
    let shown: Vec<String> = to.iter().take(3).map(|name| format!("`{name}`")).collect();
    match (shown.as_slice(), to.len().saturating_sub(3)) {
        ([], _) => f.write_str("a callee the text does not name"),
        (_, 0) => f.write_str(&shown.join(", ")),
        (_, more) => write!(f, "{} and {more} more", shown.join(", ")),
    }
}

/// Why a file of a definition's interaction group is not accounted for.
#[derive(
    serde::Serialize, schemars::JsonSchema, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug,
)]
#[serde(rename_all = "snake_case")]
pub enum Reason {
    ReadFailed,
    Changed,
    TooLarge,
    NotRead,
    Unparsed,
    NoExtractor,
    Failed,
    Ambiguous,
    Manifest,
    /// The file is of the group, spells a name, and no cell binds names to this
    /// definition's language.
    OtherLanguage,
    /// The definition is in no group, or its group holds no file.
    NoGroup,
}

impl Reason {
    /// Whether a file left unaccounted for this reason is listed by name in the
    /// answer: one that was lost or not read is, one only in a language no cell
    /// binds is counted in the gap and never listed as a use.
    pub const fn lists_the_file(self) -> bool {
        !matches!(self, Self::OtherLanguage | Self::NoGroup)
    }

    /// How an unanalysed-file row of the answer states the reason: its own
    /// strings, so that neither schema moves.
    pub const fn row_reason(self) -> &'static str {
        match self {
            Self::ReadFailed | Self::Changed | Self::NotRead | Self::Manifest => "unreadable",
            Self::TooLarge => "too_large",
            Self::Unparsed | Self::Failed | Self::Ambiguous => "parse_failed",
            Self::NoExtractor => "no_grammar",
            Self::OtherLanguage | Self::NoGroup => "not_traced",
        }
    }
}

impl std::fmt::Display for Reason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::ReadFailed => "a read failed",
            Self::Changed => "changed on disk after the listing",
            Self::TooLarge => "over the size cap",
            Self::NotRead => "listed, not read as source",
            Self::Unparsed => "parse errors",
            Self::NoExtractor => "no extractor for its language",
            Self::Failed => "the extractor failed",
            Self::Ambiguous => "a changed definition has more than one counterpart",
            Self::Manifest => "a manifest above it could not be read",
            Self::OtherLanguage => "in a language no cell binds to this definition",
            Self::NoGroup => "in no group of languages",
        })
    }
}
