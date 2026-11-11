// The workspace model, read through the snapshot reader of a real git
// repository: what the manifests of each Tier-1 language and Go declare, and
// the one predicate the engine asks of it. Expected answers come from reading
// the fixtures, never from running diffctx.

use _diffctx::snapshot::workspace::{RootOrigin, WorkspaceModel};
use _diffctx::snapshot::{AnalysisContext, RunBudget, open, resolve};
use tempfile::TempDir;

mod support;

fn build(repo: &std::path::Path) -> WorkspaceModel {
    let root = open(repo).expect("open");
    let range = resolve(&root, "HEAD~1..HEAD").expect("range");
    AnalysisContext::new(root, range, RunBudget::default())
        .expect("context")
        .workspace
}

fn model(files: &[(&str, &str)]) -> (TempDir, WorkspaceModel) {
    let repo = support::repo(&[("README.md", "base\n")], files);
    let model = build(repo.path());
    (repo, model)
}

#[test]
fn python_projects_are_roots_and_a_dependency_is_declared() {
    let (_repo, m) = model(&[
        ("a/pyproject.toml", "[project]\nname = 'a'\n"),
        ("a/src/shop/__init__.py", ""),
        ("a/src/shop/pricing.py", "def total(): ...\n"),
        ("b/pyproject.toml", "[project]\nname = 'b'\n"),
        ("b/src/shop/__init__.py", ""),
        ("b/src/shop/report.py", "def build(x): return x.total()\n"),
        (
            "c/pyproject.toml",
            "[project]\nname = 'c'\ndependencies = ['A>=1']\n[tool.uv.sources]\nA = { path = '../a' }\n",
        ),
        (
            "d/pyproject.toml",
            "[project]\nname = 'd'\n[tool.pytest.ini_options]\npythonpath = ['src', '.']\n",
        ),
        ("tools/script.py", "x = 1\n"),
    ]);
    let (a, b, c, d) = (
        m.project_of("a/src/shop/pricing.py").expect("a"),
        m.project_of("b/src/shop/report.py").expect("b"),
        m.project_of("c/x.py").expect("c"),
        m.project_of("d/y.py").expect("d"),
    );
    assert!(m.project_of("tools/script.py").is_none());
    assert!(
        !m.depends(b, a),
        "sibling projects declare nothing between them"
    );
    assert!(!m.may_reference("b/src/shop/report.py", "a/src/shop/pricing.py"));
    assert!(m.depends(c, a), "a uv path source is a declared dependency");
    assert!(m.may_reference("c/x.py", "a/src/shop/pricing.py"));
    assert!(
        !m.may_reference("a/src/shop/pricing.py", "c/x.py"),
        "a dependency has a direction"
    );
    assert!(
        m.may_reference("tools/script.py", "a/src/shop/pricing.py"),
        "a path under no manifest belongs to every project"
    );
    let has = |id, dir: &str, origin| {
        m.import_roots(id)
            .iter()
            .any(|r| r.dir == dir && r.origin == origin)
    };
    assert!(has(a, "a/src", RootOrigin::Layout), "src layout");
    assert!(has(d, "d/src", RootOrigin::Configured), "pythonpath");
}

#[test]
fn python_literal_sys_path_edit_is_a_dependency_and_an_init_is_a_package() {
    let (_repo, m) = model(&[
        ("a/pyproject.toml", "[project]\nname = 'a'\n"),
        (
            "a/src/shop/__init__.py",
            "from .pricing import total, Cart as Basket\n",
        ),
        ("a/src/shop/pricing.py", "def total(): ...\n"),
        ("b/pyproject.toml", "[project]\nname = 'b'\n"),
        (
            "b/run.py",
            "import sys\nsys.path.insert(0, '../a/src')\nimport shop\n",
        ),
        ("b/tools/lone.py", "x = 1\n"),
        ("b/tools/other.py", "y = 2\n"),
    ]);
    let (a, b) = (
        m.project_of("a/src/shop/pricing.py").expect("a"),
        m.project_of("b/run.py").expect("b"),
    );
    assert!(m.depends(b, a));
    assert!(
        m.import_roots(b)
            .iter()
            .any(|r| r.dir == "a/src" && r.origin == RootOrigin::SysPath)
    );
    let exported: Vec<&str> = m
        .project(a)
        .reexports
        .iter()
        .map(|r| r.name.as_str())
        .collect();
    assert_eq!(exported, ["total", "Basket"]);
    assert!(m.same_package("a/src/shop/__init__.py", "a/src/shop/pricing.py"));
    assert!(
        !m.same_package("b/tools/lone.py", "b/tools/other.py"),
        "a directory without an __init__.py is not a package (gh#666)"
    );
}

#[test]
fn node_workspaces_tsconfig_paths_and_exports() {
    let (_repo, m) = model(&[
        (
            "package.json",
            r#"{"name": "root", "workspaces": ["packages/*"]}"#,
        ),
        (
            "tsconfig.base.json",
            r#"{"compilerOptions": {"baseUrl": ".", "paths": {"@/*": ["packages/app/src/*"]}}}"#,
        ),
        (
            "packages/core/package.json",
            r#"{"name": "@acme/core", "exports": {".": {"import": "./src/index.ts"}, "./util": "./src/util.ts"}}"#,
        ),
        ("packages/core/src/index.ts", "export const total = 1;\n"),
        (
            "packages/app/package.json",
            r#"{"name": "@acme/app", "dependencies": {"@acme/core": "workspace:*"}}"#,
        ),
        (
            "packages/app/tsconfig.json",
            "{\n  // comments and trailing commas are what a tsconfig is\n  \"extends\": \"../../tsconfig.base\",\n  \"compilerOptions\": {\"paths\": {\"@x/*\": [\"packages/other/src/*\"],},},\n}\n",
        ),
        ("packages/app/src/a.ts", "export {};\n"),
        ("packages/other/package.json", r#"{"name": "other"}"#),
        ("packages/other/src/a.ts", "export {};\n"),
        ("packages/lone/package.json", r#"{"name": "lone"}"#),
        ("packages/lone/src/a.ts", "export {};\n"),
    ]);
    let (app, core, other, lone, root) = (
        m.project_of("packages/app/src/a.ts").expect("app"),
        m.project_of("packages/core/src/index.ts").expect("core"),
        m.project_of("packages/other/src/a.ts").expect("other"),
        m.project_of("packages/lone/src/a.ts").expect("lone"),
        m.project_of("scripts/x.js").expect("root"),
    );
    assert!(m.depends(app, core), "package.json dependency by name");
    assert!(m.depends(app, other), "a tsconfig paths target");
    assert!(
        !m.depends(app, lone),
        "an inherited baseUrl at the root does not make the root's members visible"
    );
    assert!(!m.depends(core, app));
    assert!(m.depends(root, lone), "a workspace root sees its members");
    assert!(
        m.project(core)
            .exports
            .iter()
            .any(|e| e.subpath == "./util" && e.target == "packages/core/src/util.ts")
    );
    assert!(
        m.project(app)
            .aliases
            .iter()
            .any(|a| a.pattern == "@x/*" && a.targets == ["packages/other/src/*"])
    );
}

#[test]
fn a_tsconfig_with_no_manifest_roots_a_project_that_allows_every_link() {
    let (_repo, m) = model(&[
        (
            "app-b/tsconfig.json",
            r#"{"compilerOptions": {"baseUrl": ".", "paths": {"@pkg/*": ["../shared/pkg/*"]}}}"#,
        ),
        ("app-b/src/main.ts", "export {};\n"),
        ("shared/pkg/a.js", "export {};\n"),
    ]);
    let app = m
        .project_of("app-b/src/main.ts")
        .expect("the tsconfig's project");
    assert!(
        m.project(app)
            .aliases
            .iter()
            .any(|a| a.pattern == "@pkg/*" && a.targets == ["shared/pkg/*"])
    );
    assert!(!m.project(app).dependencies_known);
    assert!(m.may_reference("app-b/src/main.ts", "shared/pkg/a.js"));
}

#[test]
fn a_bundler_alias_is_read_as_text_and_what_it_cannot_read_is_said() {
    let (_repo, m) = model(&[
        ("apps/a/package.json", r#"{"name": "a"}"#),
        (
            "apps/a/vite.config.ts",
            "export default defineConfig({ resolve: { alias: { '@pkg': path.resolve(__dirname, '../../shared/pkg'), '@o': fileURLToPath(new URL('../../shared/o', import.meta.url)) } } });\n",
        ),
        ("apps/a/src/main.ts", "export {};\n"),
        (
            "apps/d/vite.config.js",
            "export default { resolve: { alias: [{ find: /^@pkg\\//, replacement: '/shared/pkg/' }, { find: '@ok', replacement: './src' }] } };\n",
        ),
        ("apps/d/src/main.ts", "export {};\n"),
    ]);
    let a = m.project_of("apps/a/src/main.ts").expect("a");
    let patterns: Vec<(&str, &[String])> = m
        .project(a)
        .aliases
        .iter()
        .map(|x| (x.pattern.as_str(), x.targets.as_slice()))
        .collect();
    assert!(patterns.contains(&("@pkg", &["shared/pkg".to_string()][..])));
    assert!(patterns.contains(&("@pkg/*", &["shared/pkg/*".to_string()][..])));
    assert!(patterns.contains(&("@o/*", &["shared/o/*".to_string()][..])));
    assert!(!m.project(a).paths_incomplete);
    let d = m.project_of("apps/d/src/main.ts").expect("d");
    assert!(m.project(d).paths_incomplete, "a regex find is not read");
    assert!(
        m.project(d)
            .aliases
            .iter()
            .any(|x| x.pattern == "@ok/*" && x.targets == ["apps/d/src/*"])
    );
}

#[test]
fn a_base_url_and_an_unread_extends_are_stated() {
    let (_repo, m) = model(&[
        ("package.json", r#"{"name": "root"}"#),
        (
            "tsconfig.json",
            r#"{"extends": "@tsconfig/strictest/tsconfig.json", "compilerOptions": {"baseUrl": "."}}"#,
        ),
        ("src/a.ts", "export {};\n"),
    ]);
    let p = m.project_of("src/a.ts").expect("root");
    assert_eq!(m.project(p).base_urls, [""]);
    assert!(m.project(p).paths_incomplete);
}

#[test]
fn cargo_path_dependencies_and_the_mod_tree() {
    let (_repo, m) = model(&[
        (
            "Cargo.toml",
            "[workspace]\nmembers = ['crates/*']\n[workspace.dependencies]\ncore = { path = 'crates/core' }\n",
        ),
        ("crates/core/Cargo.toml", "[package]\nname = 'shop-core'\n"),
        (
            "crates/core/src/lib.rs",
            "pub mod a;\nmod b;\n#[path = \"weird.rs\"]\npub(crate) mod c;\n",
        ),
        ("crates/core/src/a.rs", "pub mod deep;\n"),
        ("crates/core/src/a/deep.rs", ""),
        (
            "crates/core/src/b/mod.rs",
            "// mod nope;\npub(super) mod x;\n",
        ),
        ("crates/core/src/b/x.rs", ""),
        ("crates/core/src/weird.rs", ""),
        ("crates/core/tests/it.rs", "mod common;\n"),
        ("crates/core/tests/common/mod.rs", ""),
        (
            "crates/app/Cargo.toml",
            "[package]\nname = 'app'\n[dependencies]\ncore.workspace = true\n",
        ),
        ("crates/app/src/main.rs", "fn main() {}\n"),
        ("crates/alone/Cargo.toml", "[package]\nname = 'alone'\n"),
        ("crates/alone/src/lib.rs", ""),
    ]);
    let (app, core, alone) = (
        m.project_of("crates/app/src/main.rs").expect("app"),
        m.project_of("crates/core/src/lib.rs").expect("core"),
        m.project_of("crates/alone/src/lib.rs").expect("alone"),
    );
    assert!(m.depends(app, core), "workspace = true resolves to a path");
    assert!(!m.depends(app, alone));
    assert!(!m.depends(core, app));
    let (krate, module) = m
        .rust_module("crates/core/src/b/x.rs")
        .expect("module of b/x.rs");
    assert_eq!(krate.name, "shop_core");
    assert_eq!(module.path, "b::x");
    let (_, weird) = m
        .rust_module("crates/core/src/weird.rs")
        .expect("#[path] module");
    assert_eq!(weird.path, "c");
    let (it, _) = m
        .rust_module("crates/core/tests/common/mod.rs")
        .expect("test helper module");
    assert_eq!(it.name, "it");
}

#[test]
fn gradle_modules_source_sets_and_packages_and_maven_modules() {
    let (_repo, m) = model(&[
        (
            "settings.gradle.kts",
            "rootProject.name = \"shop\"\ninclude(\":core\", \":app\")\ninclude(\n    \":libs:util\"\n)\nproject(\":core\").projectDir = file(\"modules/core\")\n",
        ),
        ("build.gradle.kts", "plugins {}\n"),
        ("modules/core/build.gradle.kts", ""),
        (
            "modules/core/src/main/kotlin/shop/Pricing.kt",
            "package shop\n",
        ),
        (
            "app/build.gradle.kts",
            "dependencies {\n    implementation(project(\":core\"))\n    testImplementation(projects.libs.util)\n}\n",
        ),
        ("app/src/main/kotlin/shop/Cli.kt", "package shop\n"),
        ("app/src/test/kotlin/shop/CliTest.kt", "package shop\n"),
        ("libs/util/build.gradle.kts", ""),
        ("other/build.gradle.kts", ""),
        ("other/src/main/kotlin/shop/Checkout.kt", "package shop\n"),
        (
            "mvn/pom.xml",
            "<project><modules><module>a</module><module>b/pom.xml</module></modules></project>",
        ),
        (
            "mvn/a/pom.xml",
            "<project><parent><artifactId>par</artifactId></parent><artifactId>a</artifactId><dependencies><dependency><artifactId>b</artifactId><exclusions><exclusion><artifactId>zz</artifactId></exclusion></exclusions></dependency></dependencies></project>",
        ),
        (
            "mvn/b/pom.xml",
            "<project><artifactId>b</artifactId></project>",
        ),
    ]);
    let (app, core, other, root, util) = (
        m.project_of("app/src/main/kotlin/shop/Cli.kt")
            .expect("app"),
        m.project_of("modules/core/src/main/kotlin/shop/Pricing.kt")
            .expect("core"),
        m.project_of("other/src/main/kotlin/shop/Checkout.kt")
            .expect("other"),
        m.project_of("src/main/kotlin/shop/X.kt").expect("root"),
        m.project_of("libs/util/x").expect("util"),
    );
    assert!(m.depends(app, core), "project(\":core\") with a projectDir");
    assert!(m.depends(app, util), "a typesafe project accessor");
    assert!(
        !m.depends(other, root),
        "a build no settings file includes is a project of its own (gh#649)"
    );
    assert!(!m.may_reference(
        "other/src/main/kotlin/shop/Checkout.kt",
        "modules/core/src/main/kotlin/shop/Pricing.kt"
    ));
    assert!(
        m.project(app)
            .source_sets
            .iter()
            .any(|s| s.name == "test" && s.test)
    );
    assert!(m.same_package(
        "app/src/main/kotlin/shop/Cli.kt",
        "app/src/test/kotlin/shop/CliTest.kt"
    ));
    assert!(!m.same_package(
        "app/src/main/kotlin/shop/Cli.kt",
        "other/src/main/kotlin/shop/Checkout.kt"
    ));
    let (a, b) = (
        m.project_of("mvn/a/x").expect("a"),
        m.project_of("mvn/b/x").expect("b"),
    );
    assert!(m.depends(a, b), "a pom dependency naming a module");
}

#[test]
fn go_replace_directive_and_go_work() {
    let (_repo, m) = model(&[
        ("go.mod", "module example.com/app\n\ngo 1.22\n"),
        ("cart/cart.go", "package cart\n"),
        (
            "tools/go.mod",
            "module example.com/tools\nrequire (\n\texample.com/app v0.0.0\n)\nreplace example.com/app => ../\n",
        ),
        ("tools/main.go", "package main\n"),
        ("other/go.mod", "module example.com/other\n"),
        ("other/a.go", "package other\n"),
        ("w/go.work", "use (\n\t./x\n\t./y\n)\n"),
        ("w/x/go.mod", "module x\n"),
        ("w/x/a.go", "package x\n"),
        ("w/y/go.mod", "module y\n"),
        ("w/y/a.go", "package y\n"),
    ]);
    let (app, tools, other) = (
        m.project_of("cart/cart.go").expect("app"),
        m.project_of("tools/main.go").expect("tools"),
        m.project_of("other/a.go").expect("other"),
    );
    assert!(m.depends(tools, app), "require plus a local replace");
    assert!(!m.depends(other, app));
    assert!(!m.depends(app, other));
    let (x, y) = (
        m.project_of("w/x/a.go").expect("x"),
        m.project_of("w/y/a.go").expect("y"),
    );
    assert!(m.depends(x, y) && m.depends(y, x), "go.work use");
    assert_eq!(
        m.project(tools).go_module.as_deref(),
        Some("example.com/tools")
    );
    assert!(m.same_package("cart/cart.go", "cart/other.go"));
    assert!(!m.same_package("cart/cart.go", "tools/main.go"));
}

#[test]
fn a_manifest_the_model_cannot_read_leaves_dependencies_open() {
    let (_repo, m) = model(&[
        ("a/pyproject.toml", "[project\nname ="),
        ("b/pyproject.toml", "[project]\nname = 'b'\n"),
        ("c/package.json", "{nope"),
        ("d/setup.py", "from setuptools import setup\n"),
        ("e/Gemfile", ""),
    ]);
    for unreadable in ["a/x.py", "c/x.js", "d/x.py", "e/x.rb"] {
        assert!(
            m.may_reference(unreadable, "b/y.py"),
            "{unreadable}: unknown dependencies allow every link"
        );
    }
    assert!(
        !m.may_reference("b/y.py", "a/x.py"),
        "a readable project still cannot reach into a project it does not declare"
    );
    assert_eq!(
        m.losses().len(),
        2,
        "each unreadable manifest is named; one read by design as not modelled (`setup.py`, `Gemfile`) lost nothing"
    );
}

#[cfg(unix)]
#[test]
fn a_symlinked_package_is_a_dependency() {
    let repo = support::repo(
        &[("README.md", "base\n")],
        &[
            ("packages/shared/package.json", r#"{"name": "shared"}"#),
            ("packages/shared/src/a.ts", "export {};\n"),
            ("packages/app/package.json", r#"{"name": "app"}"#),
            ("packages/app/src/b.ts", "export {};\n"),
            ("packages/lone/package.json", r#"{"name": "lone"}"#),
        ],
    );
    std::os::unix::fs::symlink("../shared", repo.path().join("packages/app/linked"))
        .expect("symlink");
    support::commit_all(repo.path(), "link the shared package");
    let root = open(repo.path()).expect("open");
    let range = resolve(&root, "HEAD~1..HEAD").expect("range");
    let m = AnalysisContext::new(root, range, RunBudget::default())
        .expect("context")
        .workspace;
    let (app, shared, lone) = (
        m.project_of("packages/app/src/b.ts").expect("app"),
        m.project_of("packages/shared/src/a.ts").expect("shared"),
        m.project_of("packages/lone/x").expect("lone"),
    );
    assert!(m.depends(app, shared));
    assert!(!m.depends(app, lone));
}

#[test]
fn the_model_reads_the_snapshot_not_the_working_tree() {
    let repo = support::repo(
        &[("README.md", "base\n")],
        &[
            ("a/pyproject.toml", "[project]\nname = 'a'\n"),
            ("b/pyproject.toml", "[project]\nname = 'b'\n"),
        ],
    );
    let before = build(repo.path());
    std::fs::remove_file(repo.path().join("a/pyproject.toml")).expect("remove");
    support::write(
        repo.path(),
        "b/pyproject.toml",
        "[project]\nname = 'b'\ndependencies = ['a']\n",
    );
    support::write(repo.path(), "junk/package.json", "{}");
    let after = build(repo.path());
    for model in [&before, &after] {
        assert_eq!(model.projects().len(), 2);
        let (a, b) = (
            model.project_of("a/x.py").expect("a"),
            model.project_of("b/x.py").expect("b"),
        );
        assert!(!model.depends(b, a));
        assert!(model.project_of("junk/x.js").is_none());
    }
}
