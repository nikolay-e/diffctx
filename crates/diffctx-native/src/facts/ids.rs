use std::fmt;
use std::ops::Range;

use serde::{Deserialize, Serialize};

macro_rules! text_id {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Debug, Default, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(pub String);

        impl $name {
            pub fn new(text: impl Into<String>) -> Self {
                Self(text.into())
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl From<&str> for $name {
            fn from(text: &str) -> Self {
                Self(text.to_string())
            }
        }

        impl From<String> for $name {
            fn from(text: String) -> Self {
                Self(text)
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }
    };
}

text_id! {
    /// The project's root directory in the repository (empty for the repository
    /// root and for a path under no manifest): stable across snapshots, unlike
    /// the workspace model's index. Extractors never set it; `facts_of` does.
    ProjectId
}
text_id! {
    /// The language-level path of a module (`pkg.sub.mod`, `src/lib`, ...): what
    /// an import names. Derived from the file's path by `facts_of`, never by an
    /// extractor, so one blob read at two paths is two modules.
    ModulePath
}
text_id! {
    /// An identifier as written.
    Name
}
text_id! {
    /// A type as written at a use site or in an `impl`.
    TypeName
}

/// A region of a blob: byte offsets and 1-based inclusive lines.
#[derive(Clone, PartialEq, Eq, Hash, Debug, Default, Serialize, Deserialize)]
pub struct Span {
    pub bytes: Range<u32>,
    pub lines: (u32, u32),
}

impl Span {
    pub fn new(bytes: Range<u32>, lines: (u32, u32)) -> Self {
        Self { bytes, lines }
    }
}
