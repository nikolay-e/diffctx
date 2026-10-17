// JavaScript/TypeScript callers read off the syntax (#506): each gate pairs
// the true set with decoys — a same-named method on another class, the name
// in a string, a template, a comment or JSX text, a type-only import, a
// same-named export of another module, a parameter or local shadowing it —
// and a fully modelled control answers the exact set.

use std::collections::BTreeSet;
use std::path::Path;
use std::process::Command;

use tempfile::TempDir;

static BIN: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| {
    std::env::var("DIFFCTX_NATIVE_BIN")
        .unwrap_or_else(|_| env!("CARGO_BIN_EXE_diffctx").to_string())
});

fn git(repo: &Path, args: &[&str]) {
    let status = Command::new("git")
        .current_dir(repo)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .args(args)
        .status()
        .expect("run git");
    assert!(status.success(), "git {args:?} failed");
}

/// A repository whose last commit applies `change` on top of `base`.
fn repo(base: &[(&str, &str)], change: &[(&str, &str)]) -> TempDir {
    let tmp = TempDir::new().expect("tempdir");
    let root = tmp.path();
    git(root, &["init", "-q", "."]);
    git(root, &["config", "user.email", "test@example.com"]);
    git(root, &["config", "user.name", "diffctx tests"]);
    for (step, files) in [base, change]
        .into_iter()
        .filter(|f| !f.is_empty())
        .enumerate()
    {
        for (rel, content) in files {
            let path = root.join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, content).unwrap();
        }
        git(root, &["add", "-A"]);
        git(root, &["commit", "-q", "-m", &format!("step {step}")]);
    }
    tmp
}

fn run(repo: &Path, args: &[&str]) -> serde_json::Value {
    let out = Command::new(&*BIN)
        .current_dir(repo)
        .args(args)
        .args(["-q", "-f", "json"])
        .output()
        .expect("run diffctx");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).expect("impact JSON")
}

fn impact(repo: &Path) -> serde_json::Value {
    run(repo, &["--diff", "HEAD~1..HEAD", "--mode", "impact"])
}

fn symbol(repo: &Path, name: &str) -> serde_json::Value {
    run(repo, &[".", "--symbol", name])
}

/// The callers listed under `path::name`, as `relation path::symbol` or
/// `relation path:lines` for a caller without a name.
fn callers(out: &serde_json::Value, path: &str, name: &str) -> BTreeSet<String> {
    let changed = out["changed"].as_array().expect("changed");
    let def = changed
        .iter()
        .find(|c| c["path"] == path && c["symbol"] == name)
        .unwrap_or_else(|| panic!("{path}::{name} not in {out:#}"));
    def["callers"]
        .as_array()
        .expect("callers")
        .iter()
        .map(|c| {
            let at = match c["symbol"].as_str() {
                Some(s) => format!("{}::{s}", c["path"].as_str().unwrap()),
                None => format!(
                    "{}:{}",
                    c["path"].as_str().unwrap(),
                    c["lines"].as_str().unwrap()
                ),
            };
            format!("{} {at}", c["relation"].as_str().unwrap_or("call"))
        })
        .collect()
}

fn set(items: &[&str]) -> BTreeSet<String> {
    items.iter().map(|s| (*s).to_string()).collect()
}

const BOOK: &str = "export async function downloadBook(id: number) {\n  return id;\n}\n";
const IMPORT: &str = "import { downloadBook } from '../lib/book';\n";

/// #476: whether a TS call is a caller depended on the ranking graph, so
/// a renamed import, a value reference and unrelated code beside the call
/// decided it. Uses are now read off the syntax of every file.
#[test]
fn ts_callers_come_from_the_syntax_not_the_ranking_graph() {
    let files: Vec<(&str, String)> = vec![
        ("src/lib/book.ts", BOOK.to_string()),
        // Shapes the control must find.
        (
            "src/a/Jsx.tsx",
            format!("{IMPORT}export function Jsx() {{\n  return <button onClick={{() => downloadBook(1)}}>x</button>;\n}}\n"),
        ),
        (
            "src/a/Field.ts",
            format!("{IMPORT}export class Field {{\n  go = () => downloadBook(2);\n}}\n"),
        ),
        (
            "src/a/Tpl.ts",
            format!("{IMPORT}export async function tpl() {{\n  return `${{await downloadBook(3)}}`;\n}}\n"),
        ),
        (
            "src/a/Obj.ts",
            format!("{IMPORT}export const api = {{\n  run() {{\n    return downloadBook(4);\n  }},\n}};\n"),
        ),
        (
            "src/a/Alias.ts",
            "import { downloadBook as dl } from '../lib/book';\nexport function alias() {\n  return dl(5);\n}\n".to_string(),
        ),
        (
            "src/a/Ns.ts",
            "import * as book from '../lib/book';\nexport function ns() {\n  return book.downloadBook(6);\n}\n".to_string(),
        ),
        ("src/a/Ref.ts", format!("{IMPORT}export const handlers = [downloadBook];\n")),
        // Decoys.
        ("src/other/book.ts", "export function downloadBook(id: number) {\n  return -id;\n}\n".to_string()),
        (
            "src/d/OtherModule.ts",
            "import { downloadBook } from '../other/book';\nexport function otherModule() {\n  return downloadBook(20);\n}\n".to_string(),
        ),
        (
            "src/d/Method.ts",
            format!("{IMPORT}export class Store {{\n  downloadBook(id: number) {{\n    return id;\n  }}\n}}\nexport function viaStore(s: Store) {{\n  return s.downloadBook(21);\n}}\n"),
        ),
        (
            "src/d/Text.tsx",
            format!("{IMPORT}// downloadBook(22) in a comment\nexport function text(id: number) {{\n  const a = 'downloadBook(23)';\n  const b = `downloadBook(${{id}})`;\n  const o = {{ downloadBook: 24 }};\n  return <p title=\"downloadBook\">downloadBook {{a}}{{b}}{{o.downloadBook}}</p>;\n}}\n"),
        ),
        (
            "src/d/Shadow.ts",
            format!("{IMPORT}export function shadow(downloadBook: (n: number) => number) {{\n  return downloadBook(25);\n}}\nexport function local() {{\n  const downloadBook = (n: number) => n;\n  return downloadBook(26);\n}}\n"),
        ),
        (
            "src/d/TypeOnly.ts",
            "import type { downloadBook as Dl } from '../lib/book';\nexport function typeOnly() {\n  return 'Dl';\n}\n".to_string(),
        ),
    ];
    let base: Vec<(&str, &str)> = files.iter().map(|(p, c)| (*p, c.as_str())).collect();
    let tmp = repo(&base, &[]);
    let out = symbol(tmp.path(), "src/lib/book.ts:downloadBook");
    assert_eq!(
        callers(&out, "src/lib/book.ts", "downloadBook"),
        set(&[
            "call src/a/Alias.ts::alias",
            "call src/a/Field.ts:3-3",
            "call src/a/Jsx.tsx::Jsx",
            "call src/a/Ns.ts::ns",
            "call src/a/Obj.ts::run",
            "call src/a/Tpl.ts::tpl",
            "reference src/a/Ref.ts::handlers",
        ])
    );
}

const CARD: &str = "import { memo } from 'react';

export function Panel({ title }: { title: string }) {
  return <section>{title}</section>;
}

function Badge({ n }: { n: number }) {
  return <span>{n}</span>;
}

export default memo(Badge);
";

/// #504: `<Ui.Panel />` through `export * as Ui from './Card'`, and a
/// default export wrapped in `memo`, rendered as `<Badge />`.
#[test]
fn jsx_reaches_namespaced_and_wrapped_components() {
    let changed = CARD
        .replace(
            "<section>{title}</section>",
            "<section className=\"p\">{title}</section>",
        )
        .replace("<span>{n}</span>", "<span className=\"b\">{n}</span>");
    let tmp = repo(
        &[
            ("src/ui/Card.tsx", CARD),
            ("src/ui/index.ts", "export * as Ui from './Card';\n"),
            (
                "src/other/Panel.tsx",
                "export function Panel() {\n  return <div>other</div>;\n}\n",
            ),
            (
                "src/other/Badge.tsx",
                "export default function Badge() {\n  return <b>other</b>;\n}\n",
            ),
            (
                "src/Page.tsx",
                "import { Ui } from './ui';\nimport Badge from './ui/Card';\n\nexport function Page() {\n  return (\n    <main>\n      <Ui.Panel title=\"t\" />\n      <Badge n={1} />\n    </main>\n  );\n}\n",
            ),
            (
                "src/Decoys.tsx",
                "import type { Panel as PanelType } from './ui/Card';\nimport { Panel } from './other/Panel';\nimport Badge from './other/Badge';\n\nclass Widget {\n  Panel() { return 1; }\n  Badge() { return 2; }\n}\n\n// <Ui.Panel /> and <Badge /> in a comment\nexport function Decoys(w: Widget) {\n  const s = '<Ui.Panel title=\"x\" /> <Badge n={2} />';\n  const t = `Badge ${s} Ui.Panel`;\n  w.Panel();\n  w.Badge();\n  return <p>Panel Badge {t}<Panel /><Badge /></p>;\n}\n",
            ),
            (
                "tests/page.test.ts",
                "import { Page } from '../src/Page';\ntest('page', () => { Page(); });\n",
            ),
        ],
        &[("src/ui/Card.tsx", &changed)],
    );
    let out = impact(tmp.path());
    assert_eq!(
        callers(&out, "src/ui/Card.tsx", "Panel"),
        set(&["reference src/Page.tsx::Page"])
    );
    assert_eq!(
        callers(&out, "src/ui/Card.tsx", "Badge"),
        set(&[
            "reference src/Page.tsx::Page",
            "reference src/ui/Card.tsx:11-11"
        ])
    );
}

/// #503: a module-level `let` read and written by its own file's
/// functions, beside a constant read the same way.
#[test]
fn a_module_binding_is_used_by_the_functions_that_read_or_write_it() {
    let contract = "let executing = false;
const PANEL_KEYS = new Set(['tempo', 'energy']);

export const setExecuting = (value: boolean) => { executing = value; };
export const isBusy = () => executing;
export function shadowed(executing: boolean) {
  return executing;
}
export function local() {
  let executing = 1;
  executing = 2;
  return executing;
}
// executing
export const label = 'executing';
export const keys = () => PANEL_KEYS.size;
";
    let changed = contract
        .replace("let executing = false;", "let executing = true;")
        .replace("'tempo', 'energy'", "'tempo', 'energy', 'mood'");
    let tmp = repo(
        &[
            ("src/contract.ts", contract),
            (
                "src/App.ts",
                "import { setExecuting } from './contract';\nexport function App() {\n  setExecuting(true);\n}\n",
            ),
            (
                "tests/app.test.ts",
                "import { App } from '../src/App';\ntest('app', () => { App(); });\n",
            ),
        ],
        &[("src/contract.ts", &changed)],
    );
    let out = impact(tmp.path());
    assert_eq!(
        callers(&out, "src/contract.ts", "executing"),
        set(&[
            "reference src/contract.ts::isBusy",
            "reference src/contract.ts::setExecuting",
        ])
    );
    assert_eq!(
        callers(&out, "src/contract.ts", "PANEL_KEYS"),
        set(&["reference src/contract.ts::keys"])
    );
}

/// #357: `import.meta.glob` and `require.context` load the modules their
/// pattern matches; the member a loaded module is asked for reaches the
/// definition, named as loaded by pattern. A negated pattern, a
/// non-recursive context and another directory's glob load nothing.
#[test]
fn a_glob_import_reaches_the_modules_its_pattern_matches() {
    let mount = |v: &str| format!("export function mountA(el: Element) {{\n  return {v};\n}}\n");
    let tmp = repo(
        &[
            ("src/lib/live/a.ts", &mount("el")),
            ("src/lib/live/engine/core.ts", &mount("null")),
            ("src/lib/other/b.ts", &mount("0")),
            (
                "src/pages/Cards.ts",
                "const modules = import.meta.glob(['../lib/live/**/*.ts', '!../lib/live/engine/**']);\nexport async function mountAll(el: Element) {\n  for (const load of Object.values(modules)) {\n    const mod: any = await load();\n    mod.mountA(el);\n  }\n}\n",
            ),
            (
                "src/pages/Ctx.js",
                "const ctx = require.context('../lib/live', false, /\\.ts$/);\nexport function viaContext(el) {\n  return ctx.keys().map((k) => ctx(k).mountA(el));\n}\n",
            ),
            (
                "src/pages/Decoy.ts",
                "const others = import.meta.glob('../lib/other/*.ts');\nexport async function decoy(el: Element) {\n  for (const load of Object.values(others)) {\n    const mod: any = await load();\n    mod.mountA(el);\n  }\n  // mod.mountA(el)\n  return 'mod.mountA';\n}\n",
            ),
        ],
        &[
            ("src/lib/live/a.ts", &mount("el.parentElement")),
            ("src/lib/live/engine/core.ts", &mount("undefined")),
            ("src/lib/other/b.ts", &mount("1")),
        ],
    );
    let out = impact(tmp.path());
    assert_eq!(
        callers(&out, "src/lib/live/a.ts", "mountA"),
        set(&[
            "call src/pages/Cards.ts::mountAll",
            "call src/pages/Ctx.js::viaContext"
        ])
    );
    assert_eq!(
        callers(&out, "src/lib/other/b.ts", "mountA"),
        set(&["call src/pages/Decoy.ts::decoy"])
    );
    let engine = out["changed"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["path"] == "src/lib/live/engine/core.ts");
    assert!(
        engine.is_none_or(|c| c["callers"].as_array().is_none_or(Vec::is_empty)),
        "{out:#}"
    );
    let candidates: Vec<&serde_json::Value> = out["changed"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|c| c["callers"].as_array().unwrap())
        .collect();
    assert!(
        candidates
            .iter()
            .all(|c| c["evidence"] == "candidate" && c["reason"] == "import_glob"),
        "{out:#}"
    );
}

/// #372: a Vite `resolve.alias` with no tsconfig `paths` twin resolves
/// like a relative import; it used to read as an npm package, and its
/// importers vanished. An alias the syntax cannot read (a regex `find`)
/// leaves its app's bare imports unresolved, never external.
#[test]
fn a_vite_alias_resolves_to_the_file_it_names() {
    let telemetry = "export function installClientTelemetry(app: string) {\n  return app;\n}\n";
    let changed = telemetry.replace("return app;", "return app.trim();");
    let start = |app: &str, spec: &str| {
        format!(
            "import {{ installClientTelemetry }} from '{spec}';\nexport function start{app}() {{\n  return installClientTelemetry('{app}');\n}}\n"
        )
    };
    let (a, a_other, a_npm, b, d) = (
        start("A", "@pkg/client-telemetry"),
        start("Other", "@other/client-telemetry"),
        start("Npm", "telemetry-lib"),
        start("B", "@pkg/client-telemetry"),
        start("D", "@pkg/client-telemetry"),
    );
    let tmp = repo(
        &[
            ("shared/pkg/client-telemetry.ts", telemetry),
            ("shared/other/client-telemetry.ts", telemetry),
            ("apps/a/package.json", "{\"name\": \"a\"}\n"),
            (
                "apps/a/vite.config.ts",
                "import { defineConfig } from 'vite';\nimport path from 'node:path';\nimport { fileURLToPath } from 'node:url';\nexport default defineConfig({\n  resolve: {\n    alias: {\n      '@pkg': path.resolve(__dirname, '../../shared/pkg'),\n      '@other': fileURLToPath(new URL('../../shared/other', import.meta.url)),\n    },\n  },\n});\n",
            ),
            ("apps/a/src/main.ts", &a),
            ("apps/a/src/other.ts", &a_other),
            ("apps/a/src/npm.ts", &a_npm),
            (
                "apps/b/tsconfig.json",
                "{\"files\": [], \"references\": [{\"path\": \"./tsconfig.app.json\"}]}\n",
            ),
            (
                "apps/b/tsconfig.app.json",
                "{\"compilerOptions\": {\"paths\": {\"@pkg/*\": [\"../../shared/pkg/*\"]}}}\n",
            ),
            ("apps/b/src/main.ts", &b),
            (
                "apps/d/vite.config.ts",
                "export default {\n  resolve: { alias: [{ find: /^@pkg\\//, replacement: '/shared/pkg/' }] },\n};\n",
            ),
            ("apps/d/src/main.ts", &d),
        ],
        &[("shared/pkg/client-telemetry.ts", &changed)],
    );
    let out = impact(tmp.path());
    assert_eq!(
        callers(
            &out,
            "shared/pkg/client-telemetry.ts",
            "installClientTelemetry"
        ),
        set(&[
            "call apps/a/src/main.ts::startA",
            "call apps/b/src/main.ts::startB",
            "call apps/d/src/main.ts::startD",
        ])
    );
    let evidence: Vec<(String, String)> = out["changed"][0]["callers"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| {
            (
                c["path"].as_str().unwrap().to_string(),
                format!("{} {}", c["evidence"], c["reason"]),
            )
        })
        .collect();
    assert!(
        evidence.contains(&("apps/a/src/main.ts".into(), "\"resolved\" null".into()))
            && evidence.contains(&(
                "apps/d/src/main.ts".into(),
                "\"candidate\" \"import_unresolved\"".into()
            )),
        "{evidence:?}"
    );
}
