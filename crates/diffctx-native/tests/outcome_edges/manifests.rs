//! Whether a view reads the workspace manifests whole: a manifest the snapshot could not read is a
//! loss, and a view over it says so.

use std::rc::Rc;

use diffctx_occurrence::tokens::occurrence::{Index, View};
use diffctx_snapshot::snapshot::{AnalysisContext, RangeSpec, RunBudget, Side};

use crate::snapshot_fixture::Repo;

fn whole(manifest: &str) -> bool {
    let repo = Repo::init();
    repo.write("svc/pyproject.toml", manifest);
    repo.write("svc/app.py", "def run():\n    return 1\n");
    let range = RangeSpec::At(Side::Tree(repo.commit("snapshot")));
    let ctx =
        AnalysisContext::new(repo.root, range, RunBudget::default()).expect("an analysis context");
    let index = Index::new(&[], &[], &[], &ctx.workspace);
    View::over(Rc::new(index), &[], &[]).manifests_whole()
}

#[test]
fn a_view_over_an_unreadable_manifest_is_not_whole_and_over_a_readable_one_it_is() {
    assert!(whole("[project]\nname = \"svc\"\nversion = \"1\"\n"));
    assert!(!whole("[project\nname = \n"));
}
