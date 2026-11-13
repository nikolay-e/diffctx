//! A resolver binds the references of the files it read: on a large range that is
//! the longest single step of an impact run, and it ran on past the run's deadline
//! (a 566-file range, ten seconds late). The deadline is raised here between the
//! last file's extraction and the first reference, so the answer is what the
//! engine did after it, counted in bindings and never timed.

mod snapshot_fixture;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use _diffctx::facts::{BlobFacts, Extractor, FactsCache, Language};
use _diffctx::resolve::{GoResolver, PythonResolver, RustResolver, TypeScriptResolver};
use _diffctx::snapshot::{AnalysisContext, Loss, RangeSpec, RunBudget, Side, SideTag, Stage};
use snapshot_fixture::Repo;

/// The shipped extractor of a language, and the run's cancellation raised once
/// it has read `left` files.
struct RaisesAfter {
    inner: Arc<dyn Extractor>,
    left: AtomicUsize,
    flag: Arc<AtomicBool>,
}

impl Extractor for RaisesAfter {
    fn language(&self) -> Language {
        self.inner.language()
    }

    fn grammar(&self) -> u64 {
        self.inner.grammar()
    }

    fn extract(&self, text: &[u8], budget: &RunBudget) -> BlobFacts {
        let blob = self.inner.extract(text, budget);
        if self.left.fetch_sub(1, Ordering::Relaxed) == 1 {
            self.flag.store(true, Ordering::Relaxed);
        }
        blob
    }
}

/// How many bindings the resolver made of `files`, and whether it says it was
/// cut short, when the run's deadline comes after `raised_after` extractions.
fn bound(
    language: Language,
    files: &[(&str, &str)],
    raised_after: usize,
    open: impl Fn(&AnalysisContext, &FactsCache) -> (usize, bool),
) -> (usize, bool) {
    let repo = Repo::init();
    for (path, text) in files {
        repo.write(path, text);
    }
    let head = repo.commit("snapshot");
    let flag = Arc::new(AtomicBool::new(false));
    let budget = RunBudget {
        cancel: Some(Arc::clone(&flag)),
        ..RunBudget::default()
    };
    let ctx = AnalysisContext::new(repo.root.clone(), RangeSpec::At(Side::Tree(head)), budget)
        .expect("an analysis context");
    let cache = FactsCache::disabled();
    let mut registry = cache.registry().clone();
    let inner = Arc::clone(registry.get(language).expect("the language is shipped"));
    registry.register(Arc::new(RaisesAfter {
        inner,
        left: AtomicUsize::new(raised_after),
        flag,
    }));
    open(&ctx, &cache.with_registry(registry))
}

fn cut_short(losses: &[Loss]) -> bool {
    losses.contains(&Loss::Deadline {
        stage: Stage::Search,
    })
}

macro_rules! stops_binding_at_the_deadline {
    ($name:ident, $language:expr, $resolver:ident, $files:expr) => {
        #[test]
        fn $name() {
            let files: &[(&str, &str)] = $files;
            let open = |ctx: &AnalysisContext, cache: &FactsCache| {
                let resolved =
                    $resolver::open(ctx, SideTag::Head, cache).expect("the head resolves");
                (resolved.bindings().len(), cut_short(resolved.losses()))
            };
            let (whole, stopped) = bound($language, files, files.len() + 1, open);
            assert!(
                whole > 0 && !stopped,
                "the control binds: {whole} {stopped}"
            );
            let (after, stopped) = bound($language, files, files.len(), open);
            assert_eq!(after, 0, "bindings made after the deadline");
            assert!(stopped, "a run cut short says so");
        }
    };
}

stops_binding_at_the_deadline!(
    rust_binds_nothing_after_the_deadline,
    Language::Rust,
    RustResolver,
    &[(
        "src/lib.rs",
        "pub fn run() {\n    helper();\n}\n\npub fn helper() {}\n"
    )]
);

stops_binding_at_the_deadline!(
    python_binds_nothing_after_the_deadline,
    Language::Python,
    PythonResolver,
    &[(
        "shop/pricing.py",
        "def run():\n    helper()\n\n\ndef helper():\n    return 1\n"
    )]
);

stops_binding_at_the_deadline!(
    typescript_binds_nothing_after_the_deadline,
    Language::TypeScript,
    TypeScriptResolver,
    &[(
        "src/pricing.ts",
        "export function run() {\n  helper();\n}\n\nexport function helper() {}\n"
    )]
);

stops_binding_at_the_deadline!(
    go_binds_nothing_after_the_deadline,
    Language::Go,
    GoResolver,
    &[(
        "pricing.go",
        "package shop\n\nfunc Run() {\n\thelper()\n}\n\nfunc helper() {}\n"
    )]
);
