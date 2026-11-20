//! Who reaches whom between files: links, users and the pointers to a target.

use diffctx_resolve::resolve::{EvidenceKind, Resolution};

use diffctx_resolve::vocabulary::family::Family;

use super::Cell;
use super::within;
use super::within_scope;

impl Cell {
    /// Whether a line range of `user_file` holds a resolved use of the
    /// definition called `name` at `lines` of `used_file`: the link a test has to
    /// a function it calls.
    pub(crate) fn links(
        &self,
        user_file: &str,
        user_lines: (u32, u32),
        used_file: &str,
        name: &str,
        used_lines: (u32, u32),
    ) -> bool {
        let Some(def) = self.definition_at(used_file, name, used_lines) else {
            return false;
        };
        self.landed.get(&def.key).is_some_and(|uses| {
            uses.iter()
                .any(|(file, line)| file == user_file && within(*line, user_lines))
        })
    }

    /// Whether anything in `user_file` is bound to something in `used_file`.
    pub(crate) fn reaches(&self, user_file: &str, used_file: &str) -> bool {
        self.reaches
            .contains(&(user_file.to_string(), used_file.to_string()))
    }

    /// The function or method of `file` that holds `line`, the innermost: the test
    /// function a use inside a test file belongs to.
    pub(crate) fn function_at(&self, file: &str, line: u32) -> Option<&str> {
        self.defs_by_file
            .get(file)?
            .iter()
            .map(|at| &self.head.definitions()[*at])
            .filter(|d| {
                matches!(
                    d.kind,
                    diffctx_facts::facts::DefKind::Function
                        | diffctx_facts::facts::DefKind::Method
                        | diffctx_facts::facts::DefKind::Constructor
                ) && d.span.lines.0 <= line
                    && line <= d.span.lines.1
            })
            .min_by_key(|d| d.span.lines.1 - d.span.lines.0)
            .map(|d| d.key.local.name.as_str())
    }

    /// The file and line of each resolved use of the definition called `name`
    /// at `lines` of `file`: who a test has to be, or to reach, to reach it.
    pub(crate) fn users(&self, file: &str, name: &str, lines: (u32, u32)) -> Vec<(String, u32)> {
        let Some(def) = self.definition_at(file, name, lines) else {
            return Vec::new();
        };
        self.landed
            .get(&def.key)
            .map_or_else(Vec::new, |uses| uses.clone())
    }

    /// Whether the family's files name other files by a path they write (a
    /// `COPY`, a `source`, a kustomization's `resources`, a module's `source`),
    /// which its resolver settles from the file's own directory.
    fn points_by_path(&self) -> bool {
        matches!(
            self.family,
            Family::Config | Family::Terraform | Family::Dockerfile | Family::Shell
        )
    }

    /// The file and line of each pointer the family settled on `target`. An entry
    /// of a list (`Listed`) is no reader of the file's content.
    pub(crate) fn pointers_to(&self, target: &str) -> Vec<(String, u32)> {
        if !self.points_by_path() {
            return Vec::new();
        }
        self.head
            .bindings()
            .iter()
            .filter(|b| {
                matches!(&b.resolution, Resolution::Module { file, evidence }
                    if file.as_str() == target && evidence.kind != EvidenceKind::Listed)
                    && within_scope(&self.scope, b.file.as_str())
            })
            .map(|b| (b.file.as_str().to_string(), b.site.lines.0))
            .collect()
    }

    /// Whether the family settled a pointer written at `file:line` on a file
    /// other than `target`, or as an entry of a list, which names no content.
    pub(crate) fn points_elsewhere(&self, file: &str, line: u32, target: &str) -> bool {
        self.points_by_path()
            && self.bindings_by_file.get(file).is_some_and(|at| {
                at.iter().map(|i| &self.head.bindings()[*i]).any(|b| {
                    b.site.lines.0 <= line
                        && line <= b.site.lines.1
                        && matches!(&b.resolution, Resolution::Module { file: found, evidence }
                                if found.as_str() != target || evidence.kind == EvidenceKind::Listed)
                })
            })
    }

    /// The files whose bindings reach something in `used_file`.
    pub(crate) fn reached_from<'a>(
        &'a self,
        used_file: &'a str,
    ) -> impl Iterator<Item = &'a str> + 'a {
        self.reaches
            .iter()
            .filter(move |(_, used)| used == used_file)
            .map(|(user, _)| user.as_str())
    }
}
