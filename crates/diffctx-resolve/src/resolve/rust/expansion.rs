//! Macros: which `macro_rules!` an invocation names, and a `self.m(..)` in the body of one,
//! whose `self` is the type of each `impl` that expands it.

use super::{
    Binding, CandidateReason, DefKind, Engine, Ent, Evidence, EvidenceKind, Name, Outcome,
    Receiver, Ref, Resolution, Scope, Site, Typed, UnresolvedReason, Want,
};

/// The macros the standard library provides.
const STD_MACROS: &[&str] = &[
    "println",
    "print",
    "eprintln",
    "eprint",
    "format",
    "format_args",
    "write",
    "writeln",
    "vec",
    "panic",
    "assert",
    "assert_eq",
    "assert_ne",
    "debug_assert",
    "debug_assert_eq",
    "debug_assert_ne",
    "unreachable",
    "unimplemented",
    "todo",
    "matches",
    "dbg",
    "cfg",
    "include_str",
    "include_bytes",
    "include",
    "env",
    "option_env",
    "concat",
    "stringify",
    "line",
    "file",
    "column",
    "module_path",
    "compile_error",
    "thread_local",
    "try",
];

impl Engine<'_> {
    pub(super) fn macro_outcome(&self, site: &Site, r: &Ref) -> Outcome {
        let name = Name::new(r.name.as_str().trim_end_matches('!'));
        let mut found: Vec<usize> = Vec::new();
        let krate = self.ix.mods[site.module].krate;
        if !r.qualifier.is_empty() {
            let segs = r.qualifier.clone();
            if let Some(Ent::Mod(m)) = self.walk(site, &segs, Want::Type, 0) {
                if let Some(ids) = self.ix.items.get(m, name.as_str()) {
                    found.extend(ids.iter().copied());
                }
            } else {
                return Outcome::Unresolved(UnresolvedReason::External);
            }
        } else {
            for d in self.ix.macros.get(&name).into_iter().flatten() {
                let same_crate = self.ix.mods[self.ix.defs[*d].home].krate == krate;
                if same_crate || self.reachable(site.file, *d) {
                    found.push(*d);
                }
            }
        }
        found.retain(|d| self.ix.defs[*d].def.kind == DefKind::Macro);
        found.sort_unstable();
        found.dedup();
        match found.as_slice() {
            [] if STD_MACROS.contains(&name.as_str()) => {
                Outcome::Unresolved(UnresolvedReason::Builtin)
            }
            [] => Outcome::Unresolved(UnresolvedReason::External),
            [only] => Outcome::Found(*only, EvidenceKind::SameScope),
            many => Outcome::Candidates(many.to_vec(), CandidateReason::UntypedReceiver),
        }
    }

    /// One resolved call per expansion whose type has a member `m`. A macro nothing expands in
    /// an `impl` binds none: its call stays a use no binding settles.
    pub(super) fn expanded_self_call(&self, f: usize, r: &Ref, out: &mut Vec<Binding>) {
        let Scope::Def(key) = &r.enclosing else {
            return;
        };
        let Some(&definition) = self.ix.macros.get(&key.name).and_then(|ids| {
            ids.iter().find(|d| {
                let rec = &self.ix.defs[**d];
                rec.file == f && rec.def.local == *key
            })
        }) else {
            return;
        };
        let mut members: Vec<usize> = Vec::new();
        for (g, invocation) in &self.ix.expansions {
            let Receiver::Typed(owner) = &invocation.receiver else {
                continue;
            };
            let site = self.site(*g, super::scope_path(&invocation.enclosing));
            if !matches!(self.macro_outcome(&site, invocation), Outcome::Found(m, _) if m == definition)
            {
                continue;
            }
            if let Typed::Def(owner) = self.typed(&site, owner.as_str(), 0) {
                members.extend(self.member_named(owner, &r.name));
            }
        }
        members.sort_unstable();
        members.dedup();
        for member in members.into_iter().filter(|m| self.reachable(f, *m)) {
            out.push(Binding {
                file: self.path_of(f).clone(),
                site: r.site.clone(),
                from: r.enclosing.clone(),
                relation: self.relation_of(r.use_kind, Some(member)),
                name: r.name.clone(),
                resolution: Resolution::Resolved {
                    target: self.ix.defs[member].key.clone(),
                    evidence: Evidence::new(EvidenceKind::SelfReceiver),
                },
                entry_guard: false,
            });
        }
    }
}
