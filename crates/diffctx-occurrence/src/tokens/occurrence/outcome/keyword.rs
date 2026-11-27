//! What a keyword argument's name stands for: a parameter or a field of the callee, never a use
//! of a subject that merely shares the name.

use super::{Claim, Reading, writable};
use crate::tokens::occurrence::rows::KEYWORD_ARGUMENTS;
use crate::tokens::occurrence::{Basis, Occurrence, Outcome, Witness};
use diffctx_facts::facts::{DefKey, Fact, LocalKind, Name, Scope};
use diffctx_resolve::resolve::{Relation, Resolution};

impl Reading<'_> {
    /// The `(` of the call whose keyword argument is the occurrence (`f(name=value)`).
    fn keyword_call(&self, occurrence: &Occurrence) -> Option<usize> {
        if !KEYWORD_ARGUMENTS
            .iter()
            .any(|f| f.has(self.file.language()))
        {
            return None;
        }
        let (start, end) = (
            occurrence.bytes.start as usize,
            occurrence.bytes.end as usize,
        );
        let before = self.text[..start].trim_ascii_end();
        let after = self.text[end..].trim_ascii_start();
        if !matches!(before.last(), Some(b'(' | b','))
            || !after.starts_with(b"=")
            || after.starts_with(b"==")
        {
            return None;
        }
        let mut depth = 0;
        before.iter().rposition(|b| match b {
            b')' => {
                depth += 1;
                false
            }
            b'(' if depth > 0 => {
                depth -= 1;
                false
            }
            _ => *b == b'(',
        })
    }

    /// The definitions the call whose `(` is at `open` resolved to.
    fn callees(&self, open: usize) -> impl Iterator<Item = &DefKey> {
        self.view
            .in_file(&self.file.path)
            .filter_map(move |binding| match &binding.resolution {
                Resolution::Resolved { target, .. } if binding.site.bytes.end as usize == open => {
                    Some(target)
                }
                _ => None,
            })
    }

    /// `Reply(url=...)` writes the field `url` of the class it builds: the keyword is a use
    /// of that field when the callee, found at the call's opening parenthesis, is its class.
    pub(super) fn keyword_field(&self, occurrence: &Occurrence) -> Option<Claim> {
        let open = self.keyword_call(occurrence)?;
        let (name, owner) = self.subject.key.local.container.split_last()?;
        (self.callees(open))
            .any(|callee| {
                callee.module == self.subject.key.module
                    && callee.local.name == *name
                    && callee.local.container == owner
            })
            .then(|| Claim::new(Some(Relation::Writes), Outcome::Ours, Basis::Resolved))
    }

    /// A keyword argument's name names a parameter or a field of the callee: no use of a subject
    /// that no keyword can write, nor of one when the call resolved to a function that declares
    /// the name as a parameter (a keyword binds a parameter of its name, whatever else it could be).
    pub(super) fn keyword_name(&self, occurrence: &Occurrence) -> Option<Witness> {
        let open = self.keyword_call(occurrence)?;
        let spelled = self.spelled(occurrence);
        (!writable(self.subject) || self.declares_parameter(open, spelled))
            .then(|| Witness::Keyword(Name::new(String::from_utf8_lossy(spelled))))
    }

    fn declares_parameter(&self, open: usize, name: &[u8]) -> bool {
        self.callees(open).any(|callee| {
            (self.view.files.iter().filter(|file| file.module == callee.module))
                .flat_map(|file| file.blob.facts.iter().map(move |fact| (file, fact)))
                .any(|(file, fact)| {
                    matches!(fact, Fact::LocalBinding(local)
                        if local.kind == LocalKind::Parameter
                            && local.name.as_str().as_bytes() == name
                            && matches!(&local.enclosing, Scope::Def(def) if file.key(def) == *callee))
                })
        })
    }
}
