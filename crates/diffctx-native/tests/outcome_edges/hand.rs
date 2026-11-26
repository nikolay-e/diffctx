//! Bindings and facts written by hand where no real file makes a resolver state them: each
//! test that uses them names what it stands in for.

use std::sync::Arc;

use diffctx_facts::facts::{DefKey, Fact, Name, Scope, Span};

use crate::{
    Binding, Evidence, EvidenceKind, FileFacts, Relation, Resolution, World, find, repo_path,
};

/// The `nth` spelling of `needle` in `file`.
pub(super) fn span(world: &World, file: &str, needle: &str, nth: usize) -> Span {
    let text = world.text(file);
    let (at, _) = text
        .match_indices(needle)
        .nth(nth)
        .unwrap_or_else(|| panic!("no spelling {nth} of {needle:?} in {file}"));
    let line = |upto: usize| 1 + text[..upto].matches('\n').count() as u32;
    let last = (at + needle.len()).saturating_sub(1);
    Span::new(
        at as u32..(at + needle.len()) as u32,
        (line(at), line(last)),
    )
}

/// The first spelling of `needle` inside the first spelling of `context` in `file`.
pub(super) fn within(world: &World, file: &str, context: &str, needle: &str) -> Span {
    let outer = span(world, file, context, 0);
    let inner = context.find(needle).expect("the needle in its context") as u32;
    let start = outer.bytes.start + inner;
    Span::new(start..start + needle.len() as u32, outer.lines)
}

pub(super) fn resolved(target: &DefKey) -> Resolution {
    Resolution::Resolved {
        target: target.clone(),
        evidence: Evidence {
            kind: EvidenceKind::SameScope,
            through: Vec::new(),
        },
    }
}

pub(super) fn key_of(world: &World, file: &str, dotted: &str) -> DefKey {
    find(&world.definitions, file, dotted).key.clone()
}

/// The scope of the definition `dotted` of `file`.
pub(super) fn scope_of_def(world: &World, file: &str, dotted: &str) -> Scope {
    Scope::Def(find(&world.definitions, file, dotted).key.local.clone())
}

/// Drops every binding of `name` in `file`.
pub(super) fn unbind(world: &mut World, file: &str, name: &str) {
    world
        .bindings
        .retain(|b| !(b.file.as_str() == file && b.name.as_str() == name));
}

/// Where a binding of `name` on `line` of `file` is written from.
pub(super) fn scope_of(world: &World, file: &str, line: u32, name: &str) -> Scope {
    binding_at(world, file, line, name).from.clone()
}

fn binding_at<'a>(world: &'a World, file: &str, line: u32, name: &str) -> &'a Binding {
    world
        .bindings
        .iter()
        .find(|b| b.file.as_str() == file && b.site.lines.0 == line && b.name.as_str() == name)
        .unwrap_or_else(|| panic!("no binding of {name} at {file}:{line}"))
}

fn binding_mut<'a>(world: &'a mut World, file: &str, line: u32, name: &str) -> &'a mut Binding {
    world
        .bindings
        .iter_mut()
        .find(|b| b.file.as_str() == file && b.site.lines.0 == line && b.name.as_str() == name)
        .unwrap_or_else(|| panic!("no binding of {name} at {file}:{line}"))
}

/// Sets what the resolver said of the binding of `name` on `line`.
pub(super) fn rebind(world: &mut World, file: &str, line: u32, name: &str, to: Resolution) {
    binding_mut(world, file, line, name).resolution = to;
}

/// Sets who the binding of `name` on `line` is written from.
pub(super) fn written_from(world: &mut World, file: &str, line: u32, name: &str, from: Scope) {
    binding_mut(world, file, line, name).from = from;
}

pub(super) fn binding(
    file: &str,
    site: Span,
    from: Scope,
    relation: Relation,
    name: &str,
    resolution: Resolution,
) -> Binding {
    Binding {
        file: repo_path(file),
        site,
        from,
        relation,
        name: Name::new(name),
        resolution,
        entry_guard: false,
    }
}

/// The facts of `file` with `extra` among them.
pub(super) fn with_facts(world: &World, file: &str, extra: Vec<Fact>) -> FileFacts {
    let facts = world.facts(file);
    let mut blob = (*facts.blob).clone();
    blob.facts.extend(extra);
    blob.in_source_order();
    FileFacts {
        blob: Arc::new(blob),
        ..facts.clone()
    }
}
