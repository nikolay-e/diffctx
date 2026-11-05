// The budget is a contract (gh#721): at any `--budget` the impact answer is
// within it, or it says that the least it can say does not fit and names the
// tokens that least takes. Run on real git repositories through the shipped
// binary; "the least" of a shape is read off its own answer (a budget of one
// token), never written down.

use std::path::Path;

use regex::Regex;
use serde_json::Value;
use tempfile::TempDir;

mod support;
use support::{commit_all, diffctx, init_repo, write};

#[derive(Clone, Copy, Debug, PartialEq)]
enum Format {
    Markdown,
    Json,
}

struct Shape {
    name: &'static str,
    build: fn(&Path),
    env: &'static [(&'static str, &'static str)],
    /// A name the answer never loses, whatever the budget: listed or shed, a
    /// changed definition is named.
    named: Option<&'static str>,
}

/// The answer's text; `None` leaves the default cap.
fn run(repo: &Path, shape: &Shape, format: Format, budget: Option<u32>) -> String {
    let mut args = vec![
        "--diff".to_string(),
        "HEAD~1..HEAD".to_string(),
        "--mode".to_string(),
        "impact".to_string(),
        "-q".to_string(),
        // The contract counts rows, listed or shed: nothing is folded.
        "--unfold".to_string(),
        "-f".to_string(),
        match format {
            Format::Markdown => "md",
            Format::Json => "json",
        }
        .to_string(),
    ];
    if let Some(budget) = budget {
        args.push("--budget".to_string());
        args.push(budget.to_string());
    }
    let out = diffctx()
        .current_dir(repo)
        .envs(shape.env.iter().copied())
        .args(&args)
        .output()
        .expect("run diffctx");
    assert!(
        out.status.success(),
        "{}: {}",
        shape.name,
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).expect("utf8")
}

/// The tokens the answer says its least takes, when it says it does not fit.
fn declared_need(text: &str, format: Format) -> Option<u32> {
    match format {
        Format::Json => serde_json::from_str::<Value>(text)
            .ok()?
            .get("budget_needed")?
            .as_u64()
            .and_then(|n| u32::try_from(n).ok()),
        Format::Markdown => text.lines().find_map(|line| {
            line.strip_prefix("(budget too small: the least this answer can say takes ")?
                .split_whitespace()
                .next()?
                .parse()
                .ok()
        }),
    }
}

fn rows(value: &Value) -> &[Value] {
    value.as_array().map_or(&[], Vec::as_slice)
}

fn count(value: &Value) -> usize {
    usize::try_from(value.as_u64().unwrap_or(0)).expect("count")
}

/// What the whole answer holds, from a run with room for everything.
struct Whole {
    files: usize,
    definitions: usize,
    callers: usize,
    /// Word matches and unread files are counted apart in the header.
    counted_apart: bool,
}

fn callers_of(doc: &Value) -> usize {
    rows(&doc["changed"])
        .iter()
        .map(|c| rows(&c["callers"]).len() + count(&c["callers_omitted"]))
        .sum()
}

fn files_of(doc: &Value) -> usize {
    rows(&doc["changed_files"]).len()
        + count(&doc["changed_files_omitted"])
        + rows(&doc["deleted_files"]).len()
}

fn whole(repo: &Path, shape: &Shape) -> Whole {
    let text = run(repo, shape, Format::Json, Some(1_000_000));
    let doc: Value = serde_json::from_str(&text).expect("impact JSON");
    let changed = rows(&doc["changed"]);
    assert!(
        changed.iter().all(|c| c["kind"] != "omitted"),
        "{}: nothing is shed with room for everything: {doc:#}",
        shape.name
    );
    Whole {
        files: files_of(&doc),
        definitions: changed.len(),
        callers: callers_of(&doc),
        counted_apart: changed.iter().any(|c| {
            rows(&c["callers"]).iter().any(|k| k["evidence"] == "name")
                || !rows(&c["missed_in"]).is_empty()
        }),
    }
}

fn type_is(name: &str, value: &Value) -> bool {
    match name {
        "null" => value.is_null(),
        "string" => value.is_string(),
        "boolean" => value.is_boolean(),
        "integer" => value.is_u64() || value.is_i64(),
        "number" => value.is_number(),
        "array" => value.is_array(),
        "object" => value.is_object(),
        _ => false,
    }
}

/// The keywords the impact schema uses, enough to hold an answer to it: a key
/// the schema does not declare is an error, as is a required one missing.
fn conforms(root: &Value, schema: &Value, value: &Value, at: &str, errors: &mut Vec<String>) {
    if let Some(target) = schema.get("$ref").and_then(Value::as_str) {
        let name = target.strip_prefix("#/$defs/").expect("a local reference");
        return conforms(root, &root["$defs"][name], value, at, errors);
    }
    if let Some(alternatives) = schema
        .get("anyOf")
        .or_else(|| schema.get("oneOf"))
        .and_then(Value::as_array)
    {
        let any = alternatives.iter().any(|alternative| {
            let mut inner = Vec::new();
            conforms(root, alternative, value, at, &mut inner);
            inner.is_empty()
        });
        if !any {
            errors.push(format!("{at}: matches none of the alternatives"));
        }
        return;
    }
    if let Some(constant) = schema.get("const") {
        if constant != value {
            errors.push(format!("{at}: not {constant}"));
        }
        return;
    }
    if let Some(allowed) = schema.get("enum").and_then(Value::as_array) {
        if !allowed.contains(value) {
            errors.push(format!("{at}: {value} is not one of {allowed:?}"));
        }
    }
    if let Some(declared) = schema.get("type") {
        let names: Vec<&str> = match declared {
            Value::String(name) => vec![name.as_str()],
            Value::Array(names) => names.iter().filter_map(Value::as_str).collect(),
            _ => Vec::new(),
        };
        if !names.iter().any(|name| type_is(name, value)) {
            errors.push(format!("{at}: {value} is not of type {names:?}"));
            return;
        }
    }
    if let Some(object) = value.as_object() {
        let properties = schema.get("properties").and_then(Value::as_object);
        if let Some(properties) = properties {
            for (key, member) in object {
                match properties.get(key) {
                    Some(sub) => conforms(root, sub, member, &format!("{at}.{key}"), errors),
                    None => errors.push(format!("{at}.{key}: the schema does not declare it")),
                }
            }
        }
        for key in schema
            .get("required")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
        {
            if !object.contains_key(key) {
                errors.push(format!("{at}: {key} is required"));
            }
        }
    }
    if let (Some(items), Some(array)) = (schema.get("items"), value.as_array()) {
        for (i, item) in array.iter().enumerate() {
            conforms(root, items, item, &format!("{at}[{i}]"), errors);
        }
    }
    if let (Some(minimum), Some(number)) = (
        schema.get("minimum").and_then(Value::as_f64),
        value.as_f64(),
    ) {
        if number < minimum {
            errors.push(format!("{at}: {number} is below {minimum}"));
        }
    }
}

fn first_number(re: &str, text: &str) -> Option<usize> {
    Regex::new(re)
        .expect("regex")
        .captures(text)?
        .get(1)?
        .as_str()
        .parse()
        .ok()
}

fn check_json(shape: &Shape, whole: &Whole, schema: &Value, text: &str, at: &str) {
    let doc: Value = serde_json::from_str(text).unwrap_or_else(|e| panic!("{at}: {e}: {text}"));
    let mut errors = Vec::new();
    conforms(schema, schema, &doc, "$", &mut errors);
    assert!(errors.is_empty(), "{at}: {errors:?}\n{text}");
    assert_eq!(files_of(&doc), whole.files, "{at}: changed files\n{text}");
    assert_eq!(
        callers_of(&doc),
        whole.callers,
        "{at}: every caller is listed or counted once\n{text}"
    );
    let shed: usize = rows(&doc["changed"])
        .iter()
        .map(|c| count(&c["callers_omitted"]))
        .sum();
    assert_eq!(
        count(&doc["truncated"]),
        shed,
        "{at}: one count of callers\n{text}"
    );
    let entries: Vec<&Value> = rows(&doc["changed"])
        .iter()
        .filter(|c| c["kind"] == "omitted")
        .collect();
    assert!(entries.len() <= 1, "{at}: one omitted entry\n{text}");
    let listed = rows(&doc["changed"]).len() - entries.len();
    let Some(entry) = entries.first() else {
        if shape.env.is_empty() {
            assert_eq!(
                listed, whole.definitions,
                "{at}: a definition went missing\n{text}"
            );
        }
        return;
    };
    let counted: usize = entry["unresolved"]
        .as_str()
        .and_then(|note| note.split_whitespace().next())
        .and_then(|n| n.parse().ok())
        .unwrap_or_else(|| panic!("{at}: the entry counts its definitions\n{text}"));
    if shape.env.is_empty() {
        assert_eq!(
            listed + counted,
            whole.definitions,
            "{at}: a definition is counted twice or not at all\n{text}"
        );
    }
    let named = rows(&entry["omitted"]);
    assert!(
        !named.is_empty(),
        "{at}: the first omitted name stays\n{text}"
    );
    assert!(
        named.len() <= counted,
        "{at}: more names than definitions\n{text}"
    );
    let named_callers: usize = named.iter().map(|d| count(&d["callers_omitted"])).sum();
    if named.len() == counted {
        assert_eq!(
            count(&entry["callers_omitted"]),
            named_callers,
            "{at}\n{text}"
        );
    } else {
        assert!(
            count(&entry["callers_omitted"]) >= named_callers,
            "{at}\n{text}"
        );
    }
    assert!(
        rows(&entry["callers"]).is_empty(),
        "{at}: a stub lists no caller\n{text}"
    );
}

/// What a line of the Markdown answer says about the same fact twice.
fn check_markdown(shape: &Shape, whole: &Whole, text: &str, at: &str) {
    let header = text.lines().next().unwrap_or("");
    if shape.env.is_empty() {
        assert_eq!(
            first_number(r"(\d+) changed file\(s\)", header),
            Some(whole.files),
            "{at}: the header counts the changed files\n{text}"
        );
        if !whole.counted_apart {
            if let Some(callers) = first_number(r"(\d+) caller\(s\)", header) {
                let possible = first_number(r"(\d+) possible", header).unwrap_or(0);
                assert_eq!(
                    callers + possible,
                    whole.callers,
                    "{at}: the header counts every caller, listed or shed\n{text}"
                );
            }
        }
    }
    let line_numbers = |re: &str| -> usize {
        let re = Regex::new(re).expect("regex");
        text.lines()
            .filter_map(|l| re.captures(l)?.get(1)?.as_str().parse::<usize>().ok())
            .sum()
    };
    let trailer = line_numbers(r"^\((\d+) weaker caller\(s\) omitted");
    let listed_shed = line_numbers(r"^  - (\d+) more caller\(s\) omitted to fit the cap$");
    assert_eq!(
        trailer, listed_shed,
        "{at}: the trailer counts what the listed definitions shed\n{text}"
    );
    let Some(entry) = text
        .lines()
        .find(|l| l.contains(" changed definition(s) omitted: the answer cap"))
    else {
        return;
    };
    let carried = first_number(r"\(their (\d+) caller\(s\) are not listed\)", entry).unwrap_or(0);
    assert!(
        carried + trailer <= whole.callers,
        "{at}: the entry's callers are counted again in the trailer\n{text}"
    );
    assert!(
        entry.contains("Finding no callers here does not mean there are none."),
        "{at}\n{text}"
    );
    let (_, names) = entry
        .split_once(" Omitted: ")
        .unwrap_or_else(|| panic!("{at}: the first omitted name stays\n{text}"));
    let items: Vec<&str> = names.split(", ").collect();
    let more = items
        .last()
        .and_then(|last| {
            last.strip_prefix("and ")?
                .strip_suffix(" more")?
                .parse()
                .ok()
        })
        .unwrap_or(0);
    let counted = first_number(r"^- (\d+) changed definition", entry).expect("the entry's count");
    assert_eq!(
        items.len() - usize::from(more > 0) + more,
        counted,
        "{at}: the entry's count and its names agree\n{text}"
    );
    for phrase in [
        "nothing calls",
        "nothing references it",
        "No resolved static callers",
    ] {
        assert!(
            !text.contains(phrase),
            "{at}: an omitted definition followed by `{phrase}`\n{text}"
        );
    }
}

fn check(shape: &Shape) {
    let tmp = TempDir::new().expect("tempdir");
    let repo = tmp.path();
    init_repo(repo);
    (shape.build)(repo);
    let whole = whole(repo, shape);
    let schema = _diffctx::impact::impact_schema();
    let mut listed: Vec<(u32, std::collections::BTreeSet<String>)> = Vec::new();
    for format in [Format::Markdown, Format::Json] {
        let least = declared_need(&run(repo, shape, format, Some(1)), format)
            .unwrap_or_else(|| panic!("{} {format:?}: one token is below any answer", shape.name));
        let budgets = [
            Some(least.saturating_sub(1).max(1)),
            Some(least),
            Some(least + 1),
            Some(150),
            Some(200),
            Some(300),
            Some(1_000),
            None,
        ];
        for budget in budgets {
            let at = format!(
                "{} {format:?} budget {budget:?} (least {least})",
                shape.name
            );
            let text = run(repo, shape, format, budget);
            let cap = budget.unwrap_or(2_000);
            let tokens = _diffctx::tokenizer::count_tokens(&text);
            match declared_need(&text, format) {
                Some(needed) => {
                    assert!(
                        cap < least,
                        "{at}: declared too small at a budget that fits\n{text}"
                    );
                    assert_eq!(needed, least, "{at}: the least is one number\n{text}");
                }
                None => {
                    assert!(
                        cap >= least,
                        "{at}: over the budget without saying so\n{text}"
                    );
                    assert!(tokens <= cap, "{at}: {tokens} tokens\n{text}");
                }
            }
            if let Some(name) = shape.named {
                assert!(
                    text.contains(name),
                    "{at}: {name} is neither listed nor named\n{text}"
                );
            }
            match format {
                Format::Json => {
                    check_json(shape, &whole, &schema, &text, &at);
                    listed.push((cap, listed_rows(&text)));
                }
                Format::Markdown => check_markdown(shape, &whole, &text, &at),
            }
        }
    }
    listed.sort_by_key(|(cap, _)| *cap);
    for pair in listed.windows(2) {
        let ((smaller, rows), (larger, more)) = (&pair[0], &pair[1]);
        let missing: Vec<&String> = rows.difference(more).collect();
        assert!(
            missing.is_empty(),
            "{}: budget {larger} dropped what budget {smaller} listed: {missing:?}",
            shape.name
        );
    }
}

/// Every listed definition and every caller listed under one: a larger budget
/// lists a superset of these, since the answer is a prefix of one fixed order.
fn listed_rows(text: &str) -> std::collections::BTreeSet<String> {
    let doc: Value = serde_json::from_str(text).expect("impact JSON");
    let mut out = std::collections::BTreeSet::new();
    for changed in rows(&doc["changed"])
        .iter()
        .filter(|c| c["kind"] != "omitted")
    {
        let owner = format!("{}::{}", changed["path"], changed["symbol"]);
        out.insert(owner.clone());
        for caller in rows(&changed["callers"]) {
            out.insert(format!(
                "{owner} <- {} {}",
                caller["path"], caller["symbol"]
            ));
        }
    }
    out
}

/// Forty changed classes with three callers each: at a tight budget nothing
/// but the one omitted entry is left.
fn everything_shed(repo: &Path) {
    for i in 0..40 {
        write(
            repo,
            &format!("pkg/m{i:02}.py"),
            &format!("class C{i:02}:\n    pass\n"),
        );
        for k in 0..3 {
            write(
                repo,
                &format!("use/u{i:02}_{k}.py"),
                &format!(
                    "from pkg.m{i:02} import C{i:02}\n\n\ndef make_{i:02}_{k}():\n    return C{i:02}()\n"
                ),
            );
        }
    }
    commit_all(repo, "base");
    for i in 0..40 {
        write(
            repo,
            &format!("pkg/m{i:02}.py"),
            &format!("class C{i:02}(object):\n    pass\n"),
        );
    }
    commit_all(repo, "change");
}

/// A module's forty functions removed, each used by one file.
fn many_removals(repo: &Path) {
    let api: String = (0..40)
        .map(|i| format!("def op_{i:02}(x):\n    return x + {i}\n\n\n"))
        .collect();
    write(repo, "lib/api.py", &api);
    for i in 0..40 {
        write(
            repo,
            &format!("client/c_{i:02}.py"),
            &format!(
                "from lib.api import op_{i:02}\n\n\ndef run_{i:02}():\n    return op_{i:02}(1)\n"
            ),
        );
    }
    commit_all(repo, "base");
    write(repo, "lib/api.py", "# the operations moved out\n");
    commit_all(repo, "remove the API");
}

/// One method reached only through receivers nothing types, and a migration:
/// twelve possible callers, no resolved one.
fn candidates_only(repo: &Path) {
    write(
        repo,
        "ledger.py",
        "class Ledger:\n    def reconcile_all(self):\n        return 1\n",
    );
    for i in 0..12 {
        write(
            repo,
            &format!("jobs/job_{i}.py"),
            &format!("import ledger\n\n\ndef nightly_{i}(w):\n    return w.reconcile_all()\n"),
        );
    }
    write(
        repo,
        "migrations/0002_add.sql",
        "ALTER TABLE a ADD b int;\n",
    );
    commit_all(repo, "base");
    write(
        repo,
        "ledger.py",
        "class Ledger:\n    def reconcile_all(self):\n        return 2\n",
    );
    write(
        repo,
        "migrations/0002_add.sql",
        "ALTER TABLE a ADD c int;\n",
    );
    commit_all(repo, "change");
}

/// Eight changed classes under a run whose deadline has passed: the callers
/// are not determined, and the answer lists none of them.
fn stopped_run(repo: &Path) {
    for i in 0..8 {
        write(
            repo,
            &format!("pkg/m{i}.py"),
            &format!("class C{i}:\n    pass\n"),
        );
        for k in 0..2 {
            write(
                repo,
                &format!("use/u{i}_{k}.py"),
                &format!("from pkg.m{i} import C{i}\n\n\ndef make_{i}_{k}():\n    return C{i}()\n"),
            );
        }
    }
    commit_all(repo, "base");
    for i in 0..8 {
        write(
            repo,
            &format!("pkg/m{i}.py"),
            &format!("class C{i}(object):\n    pass\n"),
        );
    }
    commit_all(repo, "change");
}

/// Definitions and callers under a path long in tokens, within the 260
/// characters a Windows runner's temporary directory leaves: three long
/// directories, the rest of the length in the file names.
fn long_path(repo: &Path) {
    let dir: Vec<String> = (0..3)
        .map(|d| format!("component_{d}_long_directory_name"))
        .collect();
    let (slashed, dotted) = (dir.join("/"), dir.join("."));
    let body = |i: usize, extra: &str| {
        format!(
            "def reconcile_outstanding_account_balances_{i:02}(ledger):\n    return ledger{extra}\n"
        )
    };
    for i in 0..10 {
        write(
            repo,
            &format!("{slashed}/billing_module_with_a_long_file_name_{i:02}.py"),
            &body(i, ""),
        );
        for k in 0..3 {
            write(
                repo,
                &format!("{slashed}/callers/call_outstanding_account_balances_{i:02}_{k}.py"),
                &format!(
                    "from {dotted}.billing_module_with_a_long_file_name_{i:02} import reconcile_outstanding_account_balances_{i:02}\n\n\ndef run_{i:02}_{k}(l):\n    return reconcile_outstanding_account_balances_{i:02}(l)\n"
                ),
            );
        }
    }
    commit_all(repo, "base");
    for i in 0..10 {
        write(
            repo,
            &format!("{slashed}/billing_module_with_a_long_file_name_{i:02}.py"),
            &body(i, " or None"),
        );
    }
    commit_all(repo, "change");
}

/// Names, module paths and identifiers outside ASCII.
fn non_ascii(repo: &Path) {
    for i in 0..14 {
        write(
            repo,
            &format!("модули/расчёт_{i:02}.py"),
            &format!("class Счёт{i:02}:\n    pass\n"),
        );
        for k in 0..3 {
            write(
                repo,
                &format!("вызовы/вызов_{i:02}_{k}.py"),
                &format!(
                    "from модули.расчёт_{i:02} import Счёт{i:02}\n\n\ndef вызвать_{i:02}_{k}():\n    return Счёт{i:02}()\n"
                ),
            );
        }
    }
    write(repo, "文档/说明.md", "# 说明\n");
    commit_all(repo, "base");
    for i in 0..14 {
        write(
            repo,
            &format!("модули/расчёт_{i:02}.py"),
            &format!("class Счёт{i:02}(object):\n    pass\n"),
        );
    }
    write(repo, "文档/说明.md", "# 说明\n\n改了。\n");
    commit_all(repo, "change");
}

#[test]
fn the_budget_holds_when_everything_is_shed() {
    check(&Shape {
        name: "everything shed",
        build: everything_shed,
        env: &[],
        named: None,
    });
}

#[test]
fn the_budget_holds_for_many_removals() {
    check(&Shape {
        name: "many removals",
        build: many_removals,
        env: &[],
        named: None,
    });
}

#[test]
fn the_budget_holds_for_candidates_only() {
    check(&Shape {
        name: "candidates only",
        build: candidates_only,
        env: &[],
        named: Some("reconcile_all"),
    });
}

#[test]
fn the_budget_holds_in_a_partial_run() {
    check(&Shape {
        name: "partial run",
        build: stopped_run,
        env: &[("DIFFCTX_TEST_DEADLINE_EXPIRED", "1")],
        named: None,
    });
}

#[test]
fn the_budget_holds_for_a_long_path() {
    check(&Shape {
        name: "long path",
        build: long_path,
        env: &[],
        named: None,
    });
}

#[test]
fn the_budget_holds_for_non_ascii_names() {
    check(&Shape {
        name: "non-ASCII names",
        build: non_ascii,
        env: &[],
        named: None,
    });
}
