// The two class-level properties, over every family of the registry (relay
// arch, R28 and R32). The corpus version of the same properties is
// class_completeness_corpus.rs; support/classes.rs holds the verdicts.
//
// A  enumeration, two-sided. One definition, one changed file, nobody calls
//    it. The subject's name is written into one more file of the snapshot,
//    in turn as an unchanged file and as a file of the change:
//    - a use the engine may not read: `foo(x)` in a file with an unknown
//      extension or none, and the name as a string literal in a call of a
//      parsed file of the family. The answer names the file or stays
//      partial; "complete, nobody calls" is the false absence.
//    - no use: a comment in a parsed file (the family's own suffix, the
//      definition's own file before and after the definition) and
//      documentation. The answer discloses what the same file with a word
//      nobody uses in the name's place discloses (the file is as new in both
//      runs: a file the engine does not trace is disclosed for existing, not
//      for the name): a hedge over a comment is the other defect.
//
// B  proof before not-ours. A caller that really calls the definition (a free
//    function called by name, and a method called through a typed receiver) is
//    stripped of its imports, of its type annotations, of its package clause,
//    and of all three. The answer may weaken to a candidate or a gap, never to
//    a complete answer that names nobody.
//
// Neither property reads a case's own expectations. A failing variant prints
// its row, and `DIFFCTX_CLASS_REPORT=<file>` appends the rows to a file. No
// run is decided by a clock: a run that never ends is the engine's defect, to
// be caught by the job's own timeout.

use serde_json::Value;

mod support;
use support::classes::{
    self, Finding, TRANSFORMS, comment_of, complete_empty, conclude, disclosure, excerpt, lists,
    literal_call, mention, proof_missing, strip_annotations, strip_everything, strip_imports,
    unmoved_by_no_use, use_is_enumerated,
};
use support::diffctx_unclocked;
use support::families::{self, Family, Planted, Query, RANGE, Repo, Shape, queries};

// Files no reader parses, and documentation.
const UNREAD: [&str; 2] = ["xyz", ""];
const DOCUMENTATION: [&str; 4] = ["txt", "md", "rst", "adoc"];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Spelling {
    // The name may be a use: the answer may not call it complete and empty.
    Use,
    // The name is a comment or prose: the answer does not move.
    NoUse,
}

#[derive(Clone, Debug)]
enum Where {
    // A new file; whether the change adds it.
    New { path: String, changed: bool },
    // The definition's own file: before the definition, or after it.
    Own { before: bool },
}

#[derive(Clone, Debug)]
struct Place {
    label: String,
    spelling: Spelling,
    at: Where,
    // The name as a string literal in a call, in a parsed file.
    literal: bool,
}

fn unread_path(ext: &str) -> String {
    if ext.is_empty() {
        "zz/INSERTED".to_string()
    } else {
        format!("zz/inserted.{ext}")
    }
}

fn places(family: Family) -> Vec<Place> {
    let group = family.group()[0];
    let group_path = if group.starts_with('.') {
        format!("zz/inserted{group}")
    } else {
        format!("zz/{group}")
    };
    let mut all = Vec::new();
    for changed in [false, true] {
        let state = if changed { "changed" } else { "unchanged" };
        for ext in UNREAD {
            let shown = if ext.is_empty() {
                "without an extension".to_string()
            } else {
                format!(".{ext}")
            };
            all.push(Place {
                label: format!("{state} {shown} (unread)"),
                spelling: Spelling::Use,
                at: Where::New {
                    path: unread_path(ext),
                    changed,
                },
                literal: false,
            });
        }
        for ext in DOCUMENTATION {
            all.push(Place {
                label: format!("{state} .{ext} (documentation)"),
                spelling: Spelling::NoUse,
                at: Where::New {
                    path: unread_path(ext),
                    changed,
                },
                literal: false,
            });
        }
        all.push(Place {
            label: format!("{state}, the family's suffix, in a comment"),
            spelling: Spelling::NoUse,
            at: Where::New {
                path: group_path.clone(),
                changed,
            },
            literal: false,
        });
        if literal_call(&group_path, "x").is_some() {
            all.push(Place {
                label: format!("{state}, the family's suffix, as a string literal in a call"),
                spelling: Spelling::Use,
                at: Where::New {
                    path: group_path.clone(),
                    changed,
                },
                literal: true,
            });
        }
    }
    for before in [false, true] {
        all.push(Place {
            label: format!(
                "definition's file, {} the definition, in a comment",
                if before { "before" } else { "after" }
            ),
            spelling: Spelling::NoUse,
            at: Where::Own { before },
            literal: false,
        });
    }
    all
}

// A word nobody defines or calls, spelled in place of the subject's name in the
// run that a no-use insertion is compared with: the file is as present there
// as in the run it is compared with, so what differs is the name alone.
const NEUTRAL: &str = "zzneutral";

// Writes `symbol` into the repository; the path that holds it. None where the
// place does not exist for the family.
fn insert(repo: &mut Repo, place: &Place, symbol: &str) -> Option<String> {
    match &place.at {
        Where::New { path, changed } => {
            let text = if place.literal {
                literal_call(path, symbol)?
            } else {
                mention(path, symbol)
            };
            let files = if *changed {
                &mut repo.change
            } else {
                &mut repo.base
            };
            files.insert(path.clone(), text);
            Some(path.clone())
        }
        Where::Own { before } => {
            let path = repo.definition.clone();
            comment_of(&path)?;
            // A PHP file opens with its tag.
            if repo.family == Family::Php {
                return None;
            }
            let line = mention(&path, symbol);
            for files in [&mut repo.base, &mut repo.change] {
                let text = files.get_mut(&path)?;
                if *before {
                    *text = format!("{line}{text}");
                } else {
                    text.push_str(&line);
                }
            }
            Some(path)
        }
    }
}

// The JSON answer of a run that succeeded, under no wall-clock deadline.
fn ask(planted: &Planted, query: Query) -> Result<Value, String> {
    let work = planted.work();
    let mut command = diffctx_unclocked();
    command.current_dir(&work).arg(".");
    match query {
        Query::Impact => command.args(["--diff", RANGE, "--mode", "impact"]),
        Query::Symbol => command.args(["--symbol", planted.repo.symbol_arg().as_str()]),
    };
    let out = command
        .args(["-f", "json", "-q", "--unfold"])
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("DIFFCTX_NO_MARKER", "1")
        .env(
            "DIFFCTX_CACHE_DIR",
            work.parent().expect("a parent").join("cache"),
        )
        .output()
        .expect("spawn diffctx");
    if !out.status.success() {
        return Err(format!(
            "{query:?}: exit {:?}: {}",
            out.status.code(),
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    serde_json::from_slice(&out.stdout).map_err(|e| {
        format!(
            "{query:?}: not JSON ({e}): {}",
            String::from_utf8_lossy(&out.stdout)
        )
    })
}

fn finding(class: char, family: Family, variant: String, query: Query, doc: &Value) -> Finding {
    Finding {
        class,
        family: family.label().to_string(),
        case: "synthetic".to_string(),
        variant,
        query: format!("{query:?}"),
        complete_empty: complete_empty(doc),
        excerpt: excerpt(doc),
    }
}

fn run_error(class: char, family: Family, variant: String, query: Query, why: &str) -> Finding {
    Finding {
        class: if class == 'B' { 'B' } else { 'E' },
        family: family.label().to_string(),
        case: "synthetic".to_string(),
        variant,
        query: format!("{query:?}"),
        complete_empty: false,
        excerpt: why.chars().take(300).collect(),
    }
}

// The row of an A-no-use variant says what moved.
fn moved(doc: &Value, baseline: &Value) -> String {
    let (now, before) = (disclosure(doc), disclosure(baseline));
    let mut keys: Vec<&str> = Vec::new();
    for key in ["empty", "coverage", "spelled", "unlisted", "definitions"] {
        if now[key] != before[key] {
            keys.push(key);
        }
    }
    format!("moved: {}", keys.join(","))
}

fn enumeration(family: Family) {
    let mut findings = Vec::new();
    let mut checked = 0;
    let queries = queries(family);
    for place in places(family) {
        let mut repo = families::control(family, false);
        let symbol = repo.symbol.clone();
        let mut neutral = repo.clone();
        let Some(path) = insert(&mut repo, &place, &symbol) else {
            continue;
        };
        let planted = Planted::new(repo);
        let neutral = (place.spelling == Spelling::NoUse).then(|| {
            insert(&mut neutral, &place, NEUTRAL).expect("the same place");
            Planted::new(neutral)
        });
        for query in &queries {
            checked += 1;
            let variant = format!("{:?}: {}", place.spelling, place.label);
            let doc = match ask(&planted, *query) {
                Ok(doc) => doc,
                Err(why) => {
                    findings.push(run_error('A', family, variant, *query, &why));
                    continue;
                }
            };
            let Some(neutral) = &neutral else {
                if !use_is_enumerated(&doc, &path) {
                    findings.push(finding('A', family, variant, *query, &doc));
                }
                continue;
            };
            match ask(neutral, *query) {
                Ok(baseline) if !unmoved_by_no_use(&doc, &baseline) => {
                    let mut row = finding('H', family, variant, *query, &doc);
                    row.excerpt = format!("{} {}", moved(&doc, &baseline), row.excerpt);
                    findings.push(row);
                }
                Ok(_) => {}
                Err(why) => findings.push(run_error('H', family, variant, *query, &why)),
            }
        }
    }
    conclude(&format!("A/{}", family.label()), checked, &findings);
}

// The seed whose caller reaches the definition through a typed receiver: the
// declared type is what binds the call to the definition.
fn typed(family: Family) -> Option<Repo> {
    let mut repo = families::with_caller(family, &Shape::default());
    let caller = repo.caller.clone()?;
    let (definition, caller_text): (fn(&str) -> String, &str) = match family {
        Family::Python => (
            |factor| {
                format!(
                    "class Pricing:\n    def total(self, items):\n        return sum(items){factor}\n"
                )
            },
            "from shop.pricing import Pricing\n\n\ndef charge(pricing: Pricing, cart):\n    return pricing.total(cart)\n",
        ),
        Family::TypeScript => (
            |factor| {
                format!(
                    "export class Pricing {{\n  total(items: number[]): number {{\n    return items.reduce((a, b) => a + b, 0){factor};\n  }}\n}}\n"
                )
            },
            "import { Pricing } from './pricing';\n\nexport function charge(pricing: Pricing, cart: number[]): number {\n  return pricing.total(cart);\n}\n",
        ),
        Family::Rust => (
            |factor| {
                format!(
                    "pub struct Pricing;\n\nimpl Pricing {{\n    pub fn total(&self, items: &[i64]) -> i64 {{\n        items.iter().sum::<i64>(){factor}\n    }}\n}}\n"
                )
            },
            "use crate::pricing::Pricing;\n\npub fn charge(pricing: &Pricing, cart: &[i64]) -> i64 {\n    pricing.total(cart)\n}\n",
        ),
        Family::Kotlin => (
            |factor| {
                format!(
                    "package shop\n\nclass Pricing {{\n    fun total(items: List<Int>): Int {{\n        return items.sum(){factor}\n    }}\n}}\n"
                )
            },
            "package shop\n\nfun charge(pricing: Pricing, items: List<Int>): Int {\n    return pricing.total(items)\n}\n",
        ),
        Family::Java => (
            |factor| {
                format!(
                    "package shop;\n\npublic class Pricing {{\n    public int total(int[] items) {{\n        int sum = 0;\n        for (int n : items) sum += n;\n        return sum{factor};\n    }}\n}}\n"
                )
            },
            "package shop;\n\npublic class Checkout {\n    public int charge(Pricing pricing, int[] items) {\n        return pricing.total(items);\n    }\n}\n",
        ),
        Family::Go => (
            |factor| {
                format!(
                    "package pricing\n\ntype Pricing struct{{}}\n\nfunc (p Pricing) Total(items []int) int {{\n\tsum := 0\n\tfor _, n := range items {{\n\t\tsum += n\n\t}}\n\treturn sum{factor}\n}}\n"
                )
            },
            "package checkout\n\nimport \"example.com/app/pricing\"\n\nfunc Charge(p pricing.Pricing, items []int) int {\n\treturn p.Total(items)\n}\n",
        ),
        Family::Php => (
            |factor| {
                format!(
                    "<?php\n\nnamespace Shop;\n\nclass Pricing\n{{\n    public function total(array $items): int\n    {{\n        return array_sum($items){factor};\n    }}\n}}\n"
                )
            },
            "<?php\n\nnamespace Shop;\n\nclass Checkout\n{\n    public function charge(Pricing $pricing, array $items): int\n    {\n        return $pricing->total($items);\n    }\n}\n",
        ),
        Family::Swift => (
            |factor| {
                format!(
                    "struct Pricing {{\n    func total(_ items: [Int]) -> Int {{\n        return items.reduce(0, +){factor}\n    }}\n}}\n"
                )
            },
            "func charge(_ pricing: Pricing, _ items: [Int]) -> Int {\n    return pricing.total(items)\n}\n",
        ),
        _ => return None,
    };
    repo.base.insert(repo.definition.clone(), definition(""));
    repo.change
        .insert(repo.definition.clone(), definition(" * 2"));
    repo.base.insert(caller, caller_text.to_string());
    Some(repo)
}

fn proof(family: Family, typed_receiver: bool) {
    let repo = if typed_receiver {
        typed(family)
    } else {
        Some(families::with_caller(family, &Shape::default()))
    };
    let Some(repo) = repo else { return };
    let caller = repo.caller.clone().expect("a seed has a caller");
    // A Dockerfile is its own consumer.
    if caller == repo.definition {
        return;
    }
    let shape = if typed_receiver {
        "typed receiver"
    } else {
        "free call"
    };
    let queries = if typed_receiver {
        vec![Query::Impact]
    } else {
        queries(family)
    };
    let mut findings = Vec::new();
    let mut checked = 0;

    let plain = Planted::new(repo.clone());
    for query in &queries {
        checked += 1;
        match ask(&plain, *query) {
            Err(why) => findings.push(run_error(
                'B',
                family,
                format!("{shape}: baseline"),
                *query,
                &why,
            )),
            Ok(doc) if !lists(&doc, &caller) => {
                findings.push(finding(
                    'B',
                    family,
                    format!("{shape}: baseline"),
                    *query,
                    &doc,
                ));
            }
            Ok(_) => {}
        }
    }

    let mut variants: Vec<(&str, fn(&str, &str) -> Option<String>)> = TRANSFORMS.to_vec();
    variants.push(("strip_everything", strip_everything));
    for (name, transform) in variants {
        let text = &repo.base[&caller];
        let Some(stripped) = transform(&caller, text) else {
            continue;
        };
        let symbol = repo.symbol.as_str();
        if !stripped.contains(symbol) {
            continue;
        }
        let mut variant = repo.clone();
        variant.base.insert(caller.clone(), stripped);
        let planted = Planted::new(variant);
        for query in &queries {
            checked += 1;
            match ask(&planted, *query) {
                Err(why) => findings.push(run_error(
                    'B',
                    family,
                    format!("{shape}: {name}"),
                    *query,
                    &why,
                )),
                Ok(doc) if proof_missing(&doc, &caller) => {
                    findings.push(finding(
                        'B',
                        family,
                        format!("{shape}: {name}"),
                        *query,
                        &doc,
                    ));
                }
                Ok(_) => {}
            }
        }
    }
    conclude(&format!("B/{}/{shape}", family.label()), checked, &findings);
}

macro_rules! per_family {
    ($($name:ident => $family:expr),* $(,)?) => {
        mod a_enumeration {
            use super::*;
            $(#[test] fn $name() { enumeration($family); })*
        }
        mod b_free_call {
            use super::*;
            $(#[test] fn $name() { proof($family, false); })*
        }
        mod b_typed_receiver {
            use super::*;
            $(#[test] fn $name() { proof($family, true); })*
        }
    };
}

per_family! {
    python => Family::Python,
    typescript => Family::TypeScript,
    rust => Family::Rust,
    kotlin => Family::Kotlin,
    java => Family::Java,
    go => Family::Go,
    php => Family::Php,
    c => Family::C,
    config => Family::Config,
    terraform => Family::Terraform,
    dockerfile => Family::Dockerfile,
    shell => Family::Shell,
    swift => Family::Swift,
    sql => Family::Sql,
}

// The spelling changes are plain text surgery by extension: they are checked
// on their own, so that a variant that fails is not a variant that never ran.
#[test]
fn imports_and_clauses_are_taken_out_by_extension() {
    let java = "package shop;\n\nimport a.B;\n\nclass C {}\n";
    assert_eq!(
        strip_imports("A.java", java).as_deref(),
        Some("package shop;\n\n\nclass C {}\n")
    );
    assert!(
        classes::rename_clause("A.java", java)
            .expect("a package clause")
            .contains("zz.renamed")
    );
    let go = "package x\n\nimport (\n\t\"a\"\n\t\"b\"\n)\n\nfunc F() {}\n";
    assert_eq!(
        strip_imports("x.go", go).as_deref(),
        Some("package x\n\n\nfunc F() {}\n")
    );
    assert_eq!(strip_imports("notes.xyz", "import x\n"), None);
}

#[test]
fn annotations_are_taken_out_of_declarations_only() {
    let ts = "export function charge(cart: number[]): number {\n  const r: number = total(cart);\n  return r;\n}\n";
    let stripped = strip_annotations("a.ts", ts).expect("annotations");
    assert!(stripped.contains("charge(cart) {"), "{stripped}");
    assert!(stripped.contains("const r = total(cart);"), "{stripped}");
    let java = "int charge(Pricing pricing, int[] items) {\n    return pricing.total(items);\n}\n";
    let stripped = strip_annotations("A.java", java).expect("types");
    assert!(stripped.contains("charge(pricing, items)"), "{stripped}");
    assert!(
        stripped.contains("return pricing.total(items);"),
        "{stripped}"
    );
}

// Each typed seed is a real caller before it is transformed, and each
// transformation leaves its call.
#[test]
fn typed_seeds_keep_their_call_through_every_transformation() {
    for family in Family::ALL {
        let Some(repo) = typed(family) else { continue };
        let caller = repo.caller.clone().expect("caller");
        let text = &repo.base[&caller];
        assert!(text.contains(&repo.symbol), "{family:?}");
        let stripped = strip_annotations(&caller, text).expect("a typed caller changes");
        assert!(stripped.contains(&repo.symbol), "{family:?}: {stripped}");
    }
}
