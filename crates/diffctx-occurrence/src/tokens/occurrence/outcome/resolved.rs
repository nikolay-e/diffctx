//! The claim of a binding that resolved to a definition: the subject's own, another's, or one of
//! several expansions of the same macro.

use super::{Claim, Reading};
use crate::tokens::occurrence::rows::{PAIRED_PROTOTYPES, SCHEMA_OBJECTS};
use crate::tokens::occurrence::{Basis, Outcome, Reason, Witness};
use diffctx_facts::facts::{DefKey, Namespace, Scope};
use diffctx_resolve::resolve::{Binding, Relation};

impl Reading<'_> {
    pub(super) fn resolved(&self, target: &DefKey, binding: &Binding) -> Claim {
        let relation = Some(binding.relation);
        if *target == self.subject.key || self.paired(target) || self.redefined(target) {
            Claim::new(relation, Outcome::Ours, Basis::Resolved)
        } else if binding.relation != Relation::Overrides
            && (self.family.contains(target) || self.view.stranded_at(binding))
        {
            Claim::candidate(relation, Reason::Dispatch)
        } else if matches!(&binding.from, Scope::Def(key) if key.ns == Namespace::Macro) {
            // Only the expansion of a macro body's `self` call binds from a macro scope, once per
            // expanding type: a sibling expansion of the one site, not a conflict.
            Claim::not_ours(None, Witness::OtherDefinition(target.clone()))
        } else if self.view.family_unproven
            && target.local.name == self.subject.key.local.name
            && !self.view.index.definitions.contains(&self.subject.key)
        {
            Claim::candidate(relation, Reason::OverrideUnproven)
        } else if self.view.index.definitions.contains(target) {
            Claim::not_ours(relation, Witness::OtherDefinition(target.clone()))
        } else {
            Claim::candidate(relation, Reason::BasisUnverified)
        }
    }

    /// Whether `target` is another file's definition of the subject's own key, where that is the
    /// one object (`CREATE OR REPLACE`).
    fn redefined(&self, target: &DefKey) -> bool {
        let subject = &self.subject.key;
        SCHEMA_OBJECTS.iter().any(|f| {
            f.has(self.file.language()) && f.has(target.language) && f.has(subject.language)
        }) && target.project == subject.project
            && target.local == subject.local
    }

    /// Whether `target` and the subject are a prototype and its definition.
    fn paired(&self, target: &DefKey) -> bool {
        let (subject, prototypes) = (&self.subject.key, &self.view.index.prototypes);
        PAIRED_PROTOTYPES.iter().any(|f| {
            f.has(self.file.language()) && f.has(target.language) && f.has(subject.language)
        }) && target.project == subject.project
            && target.local == subject.local
            && (prototypes.contains(target) || prototypes.contains(subject))
    }
}
