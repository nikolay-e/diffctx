// JavaScript/TypeScript callers read off the syntax (#506): each gate pairs
// the true set with decoys — a same-named method on another class, the name
// in a string, a template, a comment or JSX text, a type-only import, a
// same-named export of another module, a parameter or local shadowing it —
// and a fully modelled control answers the exact set.

use std::collections::BTreeSet;
use std::path::Path;
use std::process::Command;

use tempfile::TempDir;

mod support;
use support::diffctx;

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
    let out = diffctx()
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
    listed(out, |c| c["path"] == path && c["symbol"] == name)
}

/// The callers of the definition at `path:lines`, for a name defined twice
/// in one file.
fn callers_at(out: &serde_json::Value, path: &str, lines: &str) -> BTreeSet<String> {
    listed(out, |c| c["path"] == path && c["lines"] == lines)
}

fn listed(
    out: &serde_json::Value,
    is_def: impl Fn(&serde_json::Value) -> bool,
) -> BTreeSet<String> {
    let changed = out["changed"].as_array().expect("changed");
    let def = changed
        .iter()
        .find(|c| is_def(c))
        .unwrap_or_else(|| panic!("definition not in {out:#}"));
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
            "call src/a/Field.ts::go",
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

/// #480, closed on its own shape: a call inside an arrow function passed as
/// an argument is a caller.
#[test]
fn a_call_inside_an_arrow_argument_is_a_caller() {
    let taste = "export function isTasteLine(l: string) {\n  return l.startsWith(\"#\");\n}\n";
    let tmp = repo(
        &[
            ("lib/taste.ts", taste),
            (
                "app/list.ts",
                "import { isTasteLine } from \"../lib/taste\";\n\nexport function visible(lines: string[]) {\n  return lines.filter(line => !isTasteLine(line));\n}\n",
            ),
            (
                "app/map.ts",
                "import { isTasteLine } from \"../lib/taste\";\n\nexport function flags(lines: string[]) {\n  return lines.map(l => isTasteLine(l));\n}\n",
            ),
        ],
        &[(
            "lib/taste.ts",
            &taste.replace("l.startsWith", "l.trim().startsWith"),
        )],
    );
    let doc = impact(tmp.path());
    assert_eq!(
        callers(&doc, "lib/taste.ts", "isTasteLine"),
        set(&["call app/list.ts::visible", "call app/map.ts::flags"]),
        "{doc:#}"
    );
}

/// #491, closed on its shapes and reported again across two packages: a
/// removed export's callers are bound through the import specifier — its
/// own package's importer is one — so a package importing its own
/// same-named export is no caller, and neither is Kotlin or Python code
/// calling the bare name.
#[test]
fn a_removed_export_has_no_callers_that_import_a_namesake_or_speak_another_language() {
    let store =
        "export const useIsAuthenticated = () => true;\n\nexport const useUser = () => null;\n";
    let tmp = repo(
        &[
            ("A/package.json", "{\"name\": \"a\"}\n"),
            (
                "A/apps/frontend/src/features/auth/stores/auth.store.ts",
                store,
            ),
            (
                "A/apps/frontend/src/App.tsx",
                "import { useIsAuthenticated } from \"./features/auth/stores/auth.store\";\n\nexport function App() {\n  return useIsAuthenticated() ? 1 : 0;\n}\n",
            ),
            ("B/package.json", "{\"name\": \"b\"}\n"),
            (
                "B/frontend/src/features/auth/store.ts",
                "export const useIsAuthenticated = () => false;\n",
            ),
            (
                "B/frontend/src/components/LanguageSwitcher.tsx",
                "import { useIsAuthenticated } from \"../features/auth/store\";\n\nexport function LanguageSwitcher() {\n  return useIsAuthenticated() ? 1 : 0;\n}\n",
            ),
            (
                "C/src/main/kotlin/Boot.kt",
                "fun useIsAuthenticated(): Boolean = true\n\nfun boot() {\n    useIsAuthenticated()\n}\n",
            ),
            (
                "scripts/check.py",
                "def useIsAuthenticated():\n    return True\n\n\ndef check():\n    return useIsAuthenticated()\n",
            ),
        ],
        &[(
            "A/apps/frontend/src/features/auth/stores/auth.store.ts",
            "export const useUser = () => null;\n",
        )],
    );
    let doc = impact(tmp.path());
    let named: Vec<&str> = doc["changed"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|c| c["callers"].as_array().into_iter().flatten())
        .filter_map(|c| c["path"].as_str())
        .collect();
    assert_eq!(named, vec!["A/apps/frontend/src/App.tsx"], "{doc:#}");
    assert!(
        doc["contracts"].as_array().is_some_and(|c| c
            .iter()
            .any(|c| { c["kind"] == "removed_api" && c["symbol"] == "useIsAuthenticated" })),
        "{doc:#}"
    );
}

/// The definitions an answer lists, as `path::symbol`.
fn definitions(out: &serde_json::Value) -> BTreeSet<String> {
    out["changed"]
        .as_array()
        .expect("changed")
        .iter()
        .map(|c| {
            format!(
                "{}::{}",
                c["path"].as_str().unwrap(),
                c["symbol"].as_str().unwrap()
            )
        })
        .collect()
}

const PAGE: &str = "<!doctype html>
<html><body>
<!-- record(4) -->
<p>record(6) and history</p>
<script type=\"application/json\">{\"call\": \"record(5)\"}</script>
<script>
const history = [];
function record(x) { history.push(x); }
function restore() { window.history.replaceState(null, '', '/'); }
record(1);
</script>
</body></html>
";
const APP: &str =
    "const history = [];\nexport function record(x) { history.push(x); }\nrecord(2);\n";
const PAGER: &str = "export const PAGE_SIZE = 20;\nexport function next(at: number) {\n  return at + PAGE_SIZE;\n}\n";

/// #420, #577: a module-level `const` — in a `.js`/`.ts` module or a page's
/// inline script — is the definition diff mode names, and `--symbol` finds
/// it with its readers; a call at the top level of an inline script is a
/// caller at its own line. A local `const`, `window.history`, an object
/// key, a JSON script, an HTML comment and page text are not.
#[test]
fn a_module_binding_is_a_definition_in_a_module_and_in_a_page() {
    let tmp = repo(
        &[
            ("page.html", PAGE),
            ("app.js", APP),
            ("lib/pager.ts", PAGER),
            (
                "use.ts",
                "import { PAGE_SIZE } from './lib/pager';\nexport function pages(n: number) {\n  return Math.ceil(n / PAGE_SIZE);\n}\n",
            ),
            ("other/pager.ts", "export const PAGE_SIZE = 50;\n"),
            (
                "other/use.ts",
                "import { PAGE_SIZE } from './pager';\nexport function more() {\n  return PAGE_SIZE * 2;\n}\n",
            ),
            (
                "decoys.ts",
                "// PAGE_SIZE\nexport function local() {\n  const PAGE_SIZE = 3;\n  return PAGE_SIZE;\n}\nexport const label = 'PAGE_SIZE';\nexport const o = { PAGE_SIZE: 1 };\n",
            ),
        ],
        &[],
    );
    let page_size = symbol(tmp.path(), "PAGE_SIZE");
    assert_eq!(
        definitions(&page_size),
        set(&["lib/pager.ts::PAGE_SIZE", "other/pager.ts::PAGE_SIZE"])
    );
    assert_eq!(
        callers(&page_size, "lib/pager.ts", "PAGE_SIZE"),
        set(&["reference lib/pager.ts::next", "reference use.ts::pages"])
    );
    let history = symbol(tmp.path(), "history");
    assert_eq!(
        definitions(&history),
        set(&["app.js::history", "page.html::history"])
    );
    assert_eq!(
        callers(&history, "page.html", "history"),
        set(&["reference page.html::record"])
    );
    assert_eq!(
        callers(&history, "app.js", "history"),
        set(&["reference app.js::record"])
    );
    let record = symbol(tmp.path(), "record");
    assert_eq!(
        callers(&record, "page.html", "record"),
        set(&["call page.html:10-10"])
    );
    assert_eq!(
        callers(&record, "app.js", "record"),
        set(&["call app.js:3-3"])
    );

    // Diff mode names the same definitions, with the same readers.
    let changed = repo(
        &[("page.html", PAGE), ("app.js", APP)],
        &[
            (
                "page.html",
                &PAGE.replace("const history = [];", "const history = [0];"),
            ),
            (
                "app.js",
                &APP.replace("const history = [];", "const history = [0];"),
            ),
        ],
    );
    let out = impact(changed.path());
    assert_eq!(
        callers(&out, "page.html", "history"),
        set(&["reference page.html::record"])
    );
    assert_eq!(
        callers(&out, "app.js", "history"),
        set(&["reference app.js::record"])
    );
}

const KIT: &str = "self.serveShell = ({ name, version }) => {\n  const cache = `${name}-${version}`;\n  return cache;\n};\n";

/// #426: `self.x = …` defines a property of the global object, and
/// `exports.x = …`/`module.exports.x = …` an export. A worker's
/// `importScripts` says which kit it loads: another kit's same-named
/// global, a local, an object's method, a comment and a string are not
/// callers; a worker that loads no script reaches it as a named unknown.
#[test]
fn a_global_or_exported_member_assignment_is_a_definition() {
    let sw = |kit: &str, call: &str| format!("importScripts('{kit}');\n{call}\n");
    let changed_kit = KIT.replace("return cache;", "return cache.trim();");
    let tmp = repo(
        &[
            ("kit/sw-kit.js", KIT),
            (
                "kit/cjs.js",
                "exports.helper = (x) => x + 1;\nmodule.exports.other = function other(x) {\n  return x - 1;\n};\n",
            ),
            (
                "apps/a/sw.js",
                &sw(
                    "/kit/sw-kit.js",
                    "self.serveShell({ name: 'a', version: 1 });",
                ),
            ),
            (
                "apps/b/sw.js",
                &sw(
                    "../../kit/sw-kit.js",
                    "serveShell({ name: 'b', version: 1 });",
                ),
            ),
            (
                "apps/c/sw.js",
                &sw(
                    "/kit/sw-kit.js",
                    "globalThis.serveShell({ name: 'c', version: 1 });",
                ),
            ),
            (
                "apps/f/sw.js",
                "self.serveShell({ name: 'f', version: 1 });\n",
            ),
            (
                "apps/node/main.js",
                "const kit = require('../../kit/cjs');\nconst { other } = require('../../kit/cjs');\nfunction main() {\n  return kit.helper(1) + other(2);\n}\nmodule.exports = { main };\n",
            ),
            // Decoys.
            ("other/sw-kit.js", "self.serveShell = () => 'other';\n"),
            (
                "apps/d/sw.js",
                &sw("/other/sw-kit.js", "self.serveShell();"),
            ),
            (
                "apps/e/sw.js",
                "importScripts('/kit/other.js');\nconst serveShell = () => 0;\nserveShell();\nconst api = { serveShell() { return 1; } };\napi.serveShell();\n// self.serveShell()\nconst s = 'self.serveShell()';\n",
            ),
            ("kit/other.js", "self.version = 1;\n"),
        ],
        &[("kit/sw-kit.js", &changed_kit)],
    );
    let kit = symbol(tmp.path(), "kit/sw-kit.js:serveShell");
    let expected = set(&[
        "call apps/a/sw.js:1-2",
        "call apps/b/sw.js:1-2",
        "call apps/c/sw.js:1-2",
        "call apps/f/sw.js:1-1",
    ]);
    assert_eq!(callers(&kit, "kit/sw-kit.js", "serveShell"), expected);
    assert_eq!(
        callers(&impact(tmp.path()), "kit/sw-kit.js", "serveShell"),
        expected
    );
    let unloaded: Vec<(String, String)> = kit["changed"][0]["callers"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| {
            (
                c["path"].as_str().unwrap().to_string(),
                format!("{} {}", c["evidence"], c["reason"]),
            )
        })
        .filter(|(_, e)| e != "\"resolved\" null")
        .collect();
    assert_eq!(
        unloaded,
        vec![(
            "apps/f/sw.js".to_string(),
            "\"candidate\" \"global\"".to_string()
        )]
    );
    let other = symbol(tmp.path(), "other/sw-kit.js:serveShell");
    assert_eq!(
        callers(&other, "other/sw-kit.js", "serveShell"),
        set(&["call apps/d/sw.js:1-2", "call apps/f/sw.js:1-1"])
    );
    assert_eq!(
        callers(&symbol(tmp.path(), "helper"), "kit/cjs.js", "helper"),
        set(&["call apps/node/main.js::main"])
    );
    assert_eq!(
        callers(
            &symbol(tmp.path(), "kit/cjs.js:other"),
            "kit/cjs.js",
            "other"
        ),
        set(&["call apps/node/main.js::main"])
    );
}

const PREFS: &str = "export interface UserPreferences {
  softPrefs: Record<string, unknown>;
  theme: string;
}

export type OtherPrefs = {
  softPrefs: string[];
};
";

/// #488: an interface member or method signature, an object type's member
/// and a class field are definitions; a read through a receiver whose type the syntax states
/// is a use of that type's member, a read through an untyped one is
/// counted, never listed. Another type's member, a method of the same
/// name, a comment and a string are not uses.
#[test]
fn a_member_of_an_interface_or_class_is_a_definition() {
    let tmp = repo(
        &[
            ("src/prefs.ts", PREFS),
            (
                "src/read.ts",
                "import { UserPreferences, OtherPrefs } from './prefs';\nimport { Cache } from './cache';\n\nexport function read(p: UserPreferences) {\n  return p.softPrefs;\n}\n\nexport function other(o: OtherPrefs) {\n  return o.softPrefs.length;\n}\n\nexport function untyped(x: any, c: Cache) {\n  // p.softPrefs in a comment\n  const s = 'p.softPrefs';\n  return [x.softPrefs, c.softPrefs(), s];\n}\n",
            ),
            (
                "src/cache.ts",
                "export class Cache {\n  softPrefs() {\n    return 1;\n  }\n}\n",
            ),
            (
                "src/engine.ts",
                "export interface EngineOptions {\n  loadTimeoutMs?: number;\n}\nexport class WebEngine {\n  private readonly loadTimeoutMs: number;\n  constructor(options: EngineOptions = {}) {\n    this.loadTimeoutMs = options.loadTimeoutMs ?? 10_000;\n  }\n  wait(): Promise<void> {\n    return new Promise((resolve) => setTimeout(resolve, this.loadTimeoutMs));\n  }\n}\nexport interface Waiter {\n  wait(): Promise<void>;\n}\n",
            ),
        ],
        &[],
    );
    let prefs = symbol(tmp.path(), "softPrefs");
    assert_eq!(
        callers_at(&prefs, "src/prefs.ts", "2-2"),
        set(&["reference src/read.ts::read"])
    );
    assert_eq!(
        callers_at(&prefs, "src/prefs.ts", "7-7"),
        set(&["reference src/read.ts::other"])
    );
    assert_eq!(
        callers(&prefs, "src/cache.ts", "softPrefs"),
        set(&["call src/read.ts::untyped"])
    );
    let unresolved = prefs["changed"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["lines"] == "2-2")
        .map(|c| c["unresolved"].clone());
    assert_eq!(
        unresolved,
        Some(serde_json::json!(
            "1 read(s) of `.softPrefs` through a receiver the binding model cannot type"
        ))
    );
    let timeout = symbol(tmp.path(), "loadTimeoutMs");
    assert_eq!(
        callers_at(&timeout, "src/engine.ts", "5-5"),
        set(&[
            "reference src/engine.ts::constructor",
            "reference src/engine.ts::wait"
        ])
    );
    assert_eq!(
        callers_at(&timeout, "src/engine.ts", "2-2"),
        set(&["reference src/engine.ts::constructor"])
    );
    let wait = symbol(tmp.path(), "wait");
    let lines: BTreeSet<&str> = wait["changed"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["lines"].as_str().unwrap())
        .collect();
    assert_eq!(lines, BTreeSet::from(["14-14", "9-11"]));
}

const STORE: &str = "import { create } from 'zustand';
interface Store { play: () => Promise<void> }

export const useStore = create<Store>()((set, get) => {
  let current: string | null = null;

  function alpha(x: number): number { return x + 1; }

  function beta(x: number): number {
    current = String(alpha(x));
    return x * 2;
  }

  async function gamma(x: number): Promise<number> {
    await Promise.resolve();
    return beta(x) + alpha(x);
  }

  function delta(alpha: number): number {
    return alpha;
  }

  return { play: async () => { await gamma(1); set({}); }, delta };
});
";

const FLAT: &str = "export function alpha(x: number): number { return x + 1; }
export function beta(x: number): number {
  String(alpha(x));
  return x * 2;
}
export async function gamma(x: number): Promise<number> {
  await Promise.resolve();
  return beta(x) + alpha(x);
}
export function delta(alpha: number): number {
  return alpha;
}
export async function play() {
  await gamma(1);
}
";

/// #554: functions declared in a factory closure call each other, as at
/// module level; an indented function there read as a member of the
/// interface above it, and in a long closure one spanning a whole chunk of
/// its body had no definition at all. A parameter shadowing the name,
/// another module's function and a method of a class are not callers.
#[test]
fn functions_nested_in_a_factory_closure_call_each_other() {
    let pad = "    x = x + 1;\n".repeat(20);
    let long = STORE
        .replace("): number {\n", &format!("): number {{\n{pad}"))
        .replace(
            "): Promise<number> {\n",
            &format!("): Promise<number> {{\n{pad}"),
        );
    let tmp = repo(
        &[
            ("src/store.ts", STORE),
            ("src/long.ts", &long),
            ("src/flat.ts", FLAT),
            (
                "src/other.ts",
                "export function alpha(x: number) {\n  return -x;\n}\nexport class Box {\n  alpha() {\n    return 0;\n  }\n}\nexport function useOther(b: Box) {\n  return alpha(1) + b.alpha();\n}\n",
            ),
        ],
        &[(
            "src/long.ts",
            &long.replace("String(alpha(x))", "String(alpha(x + 1))"),
        )],
    );
    assert_eq!(
        callers(&impact(tmp.path()), "src/long.ts", "beta"),
        set(&["call src/long.ts::gamma"])
    );
    for path in ["src/store.ts", "src/long.ts", "src/flat.ts"] {
        let at = |name: &str| format!("{path}:{name}");
        assert_eq!(
            callers(&symbol(tmp.path(), &at("alpha")), path, "alpha"),
            set(&[
                &format!("call {path}::beta"),
                &format!("call {path}::gamma")
            ]),
            "{path}"
        );
        assert_eq!(
            callers(&symbol(tmp.path(), &at("beta")), path, "beta"),
            set(&[&format!("call {path}::gamma")]),
            "{path}"
        );
        assert_eq!(
            callers(&symbol(tmp.path(), &at("gamma")), path, "gamma"),
            set(&[&format!("call {path}::play")]),
            "{path}"
        );
    }
}

/// The paths a definition's answer names as not fully analysed.
fn missed_in(out: &serde_json::Value, path: &str, name: &str) -> BTreeSet<String> {
    out["changed"]
        .as_array()
        .expect("changed")
        .iter()
        .find(|c| c["path"] == path && c["symbol"] == name)
        .and_then(|c| c["missed_in"].as_array())
        .map(|m| {
            m.iter()
                .map(|f| f["path"].as_str().unwrap().to_string())
                .collect()
        })
        .unwrap_or_default()
}

const API: &str =
    "export function load() {\n  return 1;\n}\nexport function apps() {\n  return 2;\n}\n";

/// #531: a bare `&` in JSX text is text, as JSX has it: the file is not
/// "parsed with errors", and the word after it is no use of a function of
/// that name. A file the grammar really cannot read still says so, and an
/// entity and a bitwise `&` in code are as they were.
#[test]
fn a_bare_ampersand_in_jsx_text_is_text() {
    let tmp = repo(
        &[
            ("api.ts", API),
            (
                "Panel.tsx",
                "import { load, apps } from './api';\nexport function Panel() {\n  const v = load() & 1;\n  return (\n    <h2>Devices & apps &amp; more {v}</h2>\n  );\n}\n",
            ),
            (
                "Broken.tsx",
                "import { load } from './api';\nexport function Broken() {\n  return <p>{load(</p>;\n}\n",
            ),
        ],
        &[("api.ts", &API.replace("return 1;", "return 3;"))],
    );
    let out = impact(tmp.path());
    assert!(callers(&out, "api.ts", "load").contains("call Panel.tsx::Panel"));
    assert_eq!(missed_in(&out, "api.ts", "load"), set(&["Broken.tsx"]));
    let apps = symbol(tmp.path(), "apps");
    assert_eq!(callers(&apps, "api.ts", "apps"), set(&[]));
}

/// #558: a default import names its module's default export, nothing else:
/// one from a module the resolver cannot place is no candidate for every
/// named definition, and a bundler query (`?raw`) names the file it
/// queries. A named import through an unresolved alias or a subpath import
/// (`#types/api`) stays a candidate.
#[test]
fn a_default_import_names_only_a_default_export() {
    let tmp = repo(
        &[
            (
                "web/tsconfig.json",
                "{ \"extends\": \"@tsconfig/strictest/tsconfig.json\" }\n",
            ),
            (
                "web/src/types/api.ts",
                "export type ClinicalAlertEvent = { id: string };\nexport default function Banner() {\n  return 1;\n}\n",
            ),
            (
                "web/src/pdf/pdf-service.ts",
                "import AutoTableImport from 'jspdf-autotable';\nimport swSource from '../../public/sw.js?raw';\n\ntype AutoTableFn = typeof AutoTableImport;\nexport function render() {\n  return new Function('globalThis', `${swSource}\\nreturn parse;`);\n}\n",
            ),
            (
                "web/src/pages/Alerts.ts",
                "import type { ClinicalAlertEvent } from '@pkg/types';\nimport Banner from '@pkg/banner';\nexport function show(e: ClinicalAlertEvent) {\n  return [e.id, Banner()];\n}\n",
            ),
            (
                "app/sub.ts",
                "import type { ClinicalAlertEvent as Ev } from '#types/api';\nexport function sub(e: Ev) {\n  return e.id;\n}\n",
            ),
            ("web/public/sw.js", "self.parse = (r) => r;\n"),
        ],
        &[],
    );
    assert_eq!(
        callers(
            &symbol(tmp.path(), "ClinicalAlertEvent"),
            "web/src/types/api.ts",
            "ClinicalAlertEvent"
        ),
        set(&[
            "reference web/src/pages/Alerts.ts::show",
            "reference app/sub.ts::sub"
        ])
    );
    assert_eq!(
        callers(
            &symbol(tmp.path(), "Banner"),
            "web/src/types/api.ts",
            "Banner"
        ),
        set(&[
            "call web/src/pages/Alerts.ts::show",
            "reference web/src/pdf/pdf-service.ts::AutoTableFn"
        ])
    );
}

/// #372: a page's `<script type="module">` imports like a module does, so
/// its call is a caller; the same import in page text or a comment is not.
#[test]
fn a_module_script_in_a_page_imports_like_a_module() {
    let telemetry = "export function installClientTelemetry(app) {\n  return app;\n}\n";
    let tmp = repo(
        &[
            ("shared/pkg/client-telemetry.js", telemetry),
            (
                "apps/b/index.html",
                "<!doctype html>\n<html><body>\n<script type=\"module\">\nimport { installClientTelemetry } from \"../../shared/pkg/client-telemetry.js\";\ninstallClientTelemetry(\"b\");\n</script>\n</body></html>\n",
            ),
            (
                "apps/c/index.html",
                "<!doctype html>\n<html><body>\n<!-- installClientTelemetry(\"c\") -->\n<p>import { installClientTelemetry } from \"../../shared/pkg/client-telemetry.js\"; installClientTelemetry(\"c\");</p>\n</body></html>\n",
            ),
        ],
        &[(
            "shared/pkg/client-telemetry.js",
            &telemetry.replace("return app;", "return app.trim();"),
        )],
    );
    assert_eq!(
        callers(
            &impact(tmp.path()),
            "shared/pkg/client-telemetry.js",
            "installClientTelemetry"
        ),
        set(&["call apps/b/index.html:5-5"])
    );
}

/// #372: a symlink of the working tree holds the module it names, as Node
/// and Vite load it: an import of the link reaches the definitions there.
/// A real copy of the module elsewhere, and a link to that copy, are not
/// this module.
#[cfg(unix)]
#[test]
fn an_import_of_a_symlink_reaches_the_module_it_names() {
    let telemetry = "export function installClientTelemetry(app) {\n  return app;\n}\n";
    let call = |from: &str| {
        format!(
            "import {{ installClientTelemetry }} from '{from}';\nexport function boot() {{\n  return installClientTelemetry(1);\n}}\n"
        )
    };
    let tmp = repo(
        &[
            ("shared/pkg/client-telemetry.js", telemetry),
            (
                "apps/a/index.html",
                "<!doctype html>\n<html><body>\n<script type=\"module\">\nimport { installClientTelemetry } from \"./public/client-telemetry.js\";\ninstallClientTelemetry(\"a\");\n</script>\n</body></html>\n",
            ),
            ("apps/d/main.js", &call("../a/public/client-telemetry.js")),
            // Decoys: a copy of the module, imported directly and through a
            // link of its own.
            ("other/client-telemetry.js", telemetry),
            ("apps/e/main.js", &call("../../other/client-telemetry.js")),
            ("apps/f/main.js", &call("./t.js")),
        ],
        &[],
    );
    let root = tmp.path();
    for (link, target) in [
        (
            "apps/a/public/client-telemetry.js",
            "../../../shared/pkg/client-telemetry.js",
        ),
        ("apps/f/t.js", "../../other/client-telemetry.js"),
    ] {
        let link = root.join(link);
        std::fs::create_dir_all(link.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(target, link).expect("symlink");
    }
    git(root, &["add", "-A"]);
    git(root, &["commit", "-q", "-m", "links"]);
    let out = symbol(
        root,
        "shared/pkg/client-telemetry.js:installClientTelemetry",
    );
    assert_eq!(
        callers(
            &out,
            "shared/pkg/client-telemetry.js",
            "installClientTelemetry"
        ),
        set(&["call apps/a/index.html:5-5", "call apps/d/main.js::boot"])
    );
    let other = symbol(root, "other/client-telemetry.js:installClientTelemetry");
    assert_eq!(
        callers(
            &other,
            "other/client-telemetry.js",
            "installClientTelemetry"
        ),
        set(&["call apps/e/main.js::boot", "call apps/f/main.js::boot"])
    );
}
