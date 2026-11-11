// Metamorphic suite for impact and `--symbol` (arch migration, stage 1.4).
//
// A seed repository per Tier-1 language has one changed definition with two
// known callers and a test (support/seeds.rs). Each property builds the plain
// seed and a transformed one, asks the shipped binary about both, and holds
// the relation set (callers, the test that reaches each, stale references),
// the listed definitions and what the answer discloses to the relation the
// transformation must keep:
//
//   reformat_layout, reformat_parens, reformat_interpolation  same relations
//   unrelated_in_function, unrelated_module_level             old relations kept
//   homonym_decoys                      a same-named neighbour never appears
//   rename_in_range, rename_prior, move_helper    a move is not a deletion
//   order_and_repeat                    traversal order, repeats, threads
//   budget_growth                       a larger budget never loses a claim
//
// The seventh transformation of the plan, a failing git read, is the sweep in
// git_faults.rs, which fails each git call of a run in turn.
//
// The second half of the file is the absence accounting (arch migration,
// phase 1): a seed for each of the twelve families of the registry, one
// changed definition with one known caller (support/families.rs). A
// transformation hides the caller from the extractor or the resolver, and the
// answer then names the link or names a gap: `empty: true` is a claim that
// nothing calls the definition and is never made over a caller. Every family
// also keeps a control with no caller anywhere, which has to stay a clean
// absence, so that an answer that hedges everything does not pass.
//
//   deep_call_10, deep_call_300, deep_call_2000   the call a leaf of a left spine
//   syntax_error_near_declaration, syntax_error_near_call
//   caller_over_the_size_cap, caller_in_a_neighbour_language
//   one_letter_name (alone, with a broken and with an oversize caller)
//   absence_control, absence_control_with_a_broken_unrelated_file
//
// Expected answers come from reading the generated files. The properties
// compare two answers of the engine and need no oracle; `seed_names_its_callers`
// is the one absolute check that the plain seed is understood at all.
//
// Knobs for a local or nightly run:
//   DIFFCTX_METAMORPHIC_CASES  cases per property (default 4)
//   DIFFCTX_METAMORPHIC_SEED   decimal seed of the case stream (default fixed)
// Either one set turns off the known-red check below that a listed property
// still fails: another stream of cases may not reach the failing shape.
//
// No case reads the clock; a failure prints the seed and the minimal knobs.

use std::collections::BTreeSet;
use std::process::Output;

use proptest::sample::select;
use proptest::strategy::Strategy;
use proptest::test_runner::{Config, RngAlgorithm, TestCaseError, TestRng, TestRunner};
use serde_json::Value;
use tempfile::TempDir;

mod support;
use support::answer::{
    changed_rows, difference, disclosure, relations, removed_rows, section_sizes,
};
use support::diffctx;
use support::families::{
    self, Break, Family, Neighbour, Planted, Query, Shape, discloses, names_link, queries,
    says_empty,
};
use support::seeds::{Edit, Knobs, Lang, RANGE, Seed, seed};

const DEFAULT_CASES: u32 = 4;
const DEFAULT_SEED: u64 = 0x00D1_FFC7_0000_0001;
// Every shrink step asks the binary again; the failing knobs are small already.
const SHRINK_STEPS: u32 = 6;
const BUDGETS: [u32; 6] = [100, 200, 400, 800, 1600, 3200];

// Properties that fail today, each with the issue that tracks it (`gh#N` or
// `fj#N`), named `property/language`. Checked both ways at the default
// settings: a property that starts passing has to leave the list.
const KNOWN_RED: &[(&str, &str)] = &[];

struct Settings {
    cases: u32,
    seed: u64,
    default: bool,
}

fn number<T: std::str::FromStr>(name: &str) -> Option<T> {
    std::env::var(name).ok().map(|value| {
        value
            .parse()
            .unwrap_or_else(|_| panic!("{name}={value} is not a number"))
    })
}

fn settings() -> Settings {
    let cases = number::<u32>("DIFFCTX_METAMORPHIC_CASES");
    let seed = number::<u64>("DIFFCTX_METAMORPHIC_SEED");
    Settings {
        cases: cases.unwrap_or(DEFAULT_CASES),
        seed: seed.unwrap_or(DEFAULT_SEED),
        default: cases.is_none() && seed.is_none(),
    }
}

// Each property draws from its own stream of the seed.
fn stream(seed: u64, id: &str) -> [u8; 32] {
    let named = id.bytes().fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3)
    });
    let mut state = seed ^ named;
    let mut bytes = [0u8; 32];
    for chunk in bytes.chunks_mut(8) {
        state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut mixed = state;
        mixed = (mixed ^ (mixed >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        mixed = (mixed ^ (mixed >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        mixed ^= mixed >> 31;
        chunk.copy_from_slice(&mixed.to_le_bytes());
    }
    bytes
}

// Applies the known-red list to what a property found.
fn settle(id: &str, guards: &str, settings: &Settings, outcome: Result<(), String>) {
    let known = KNOWN_RED.iter().find(|(name, _)| *name == id);
    match (outcome, known) {
        (Ok(()), None) | (Err(_), Some(_)) => {}
        (Ok(()), Some((_, issue))) => assert!(
            !settings.default,
            "{id} is listed as known red ({issue}) but passes: remove it from KNOWN_RED"
        ),
        (Err(failure), None) => panic!(
            "{id} (guards {guards}) failed: {failure}\nreplay: DIFFCTX_METAMORPHIC_SEED={} DIFFCTX_METAMORPHIC_CASES={} cargo test --release --test metamorphic {}",
            settings.seed,
            settings.cases,
            id.replace('/', "::")
        ),
    }
}

fn check<S: Strategy>(
    id: &str,
    guards: &str,
    strategy: &S,
    property: impl Fn(S::Value) -> Result<(), String>,
) {
    let settings = settings();
    let config = Config {
        cases: settings.cases,
        failure_persistence: None,
        max_shrink_iters: SHRINK_STEPS,
        ..Config::default()
    };
    let rng = TestRng::from_seed(RngAlgorithm::ChaCha, &stream(settings.seed, id));
    let mut runner = TestRunner::new_with_rng(config, rng);
    let outcome = runner
        .run(strategy, |value| {
            property(value).map_err(TestCaseError::fail)
        })
        .map_err(|failure| failure.to_string());
    settle(id, guards, &settings, outcome);
}

// A check with nothing to draw.
fn check_once(id: &str, guards: &str, property: impl Fn() -> Result<(), String>) {
    settle(id, guards, &settings(), property());
}

#[derive(Clone, Copy, Debug)]
enum Mode {
    Impact,
    Symbol,
}

// A seed as a repository.
struct Built {
    dir: TempDir,
    seed: Seed,
}

impl Built {
    fn new(seed: Seed) -> Self {
        let dir = TempDir::new().expect("tempdir");
        seed.commit_into(dir.path());
        Self { dir, seed }
    }

    fn ask(&self, mode: Mode, extra: &[&str], env: &[(&str, &str)]) -> Result<Value, String> {
        let symbol = format!("{}:{}", self.seed.target_path, self.seed.target);
        let mut command = diffctx();
        command.current_dir(self.dir.path()).arg(".");
        match mode {
            Mode::Impact => command.args(["--diff", RANGE, "--mode", "impact"]),
            Mode::Symbol => command.args(["--symbol", symbol.as_str()]),
        };
        command
            // The suite compares relations, whatever the answer folds.
            .args(["-f", "json", "-q", "--unfold"])
            .args(extra)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("DIFFCTX_NO_MARKER", "1");
        for (key, value) in env {
            command.env(key, value);
        }
        let out = command
            .output()
            .map_err(|e| format!("spawn diffctx: {e}"))?;
        parse(mode, &out)
    }
}

fn parse(mode: Mode, out: &Output) -> Result<Value, String> {
    if !out.status.success() {
        return Err(format!(
            "{mode:?}: exit {:?}: {}",
            out.status.code(),
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    serde_json::from_slice(&out.stdout).map_err(|e| {
        format!(
            "{mode:?}: not JSON ({e}): {}",
            String::from_utf8_lossy(&out.stdout)
        )
    })
}

// ---- comparing answers ----

fn mapped(lines: &BTreeSet<String>, renames: &[(String, String)]) -> BTreeSet<String> {
    lines
        .iter()
        .map(|line| {
            renames.iter().fold(line.clone(), |text, (old, new)| {
                text.replace(old.as_str(), new)
            })
        })
        .collect()
}

fn compare(
    label: &str,
    want: &Value,
    got: &Value,
    renames: &[(String, String)],
    with_changed: bool,
) -> Result<(), String> {
    let (wanted, found) = (mapped(&relations(want), renames), relations(got));
    if wanted != found {
        return Err(format!(
            "{label}: the relation set changed\n{}",
            difference(&wanted, &found)
        ));
    }
    if with_changed {
        let (wanted, found) = (mapped(&changed_rows(want), renames), changed_rows(got));
        if wanted != found {
            return Err(format!(
                "{label}: the listed definitions changed\n{}",
                difference(&wanted, &found)
            ));
        }
    }
    let (wanted, found) = (disclosure(want), disclosure(got));
    if wanted != found {
        return Err(format!(
            "{label}: the disclosure changed: {wanted} became {found}"
        ));
    }
    Ok(())
}

// The plain seed and the variant answer the same, for impact and for the
// qualified symbol. Returns the variant's impact answer for further checks.
fn same_answers(base: &Built, variant: &Built, with_changed: bool) -> Result<Value, String> {
    let mut impact = Value::Null;
    for mode in [Mode::Impact, Mode::Symbol] {
        let want = base.ask(mode, &[], &[])?;
        let got = variant.ask(mode, &[], &[])?;
        compare(
            &format!("{mode:?}"),
            &want,
            &got,
            &variant.seed.renames,
            with_changed,
        )?;
        if matches!(mode, Mode::Impact) {
            impact = got;
        }
    }
    Ok(impact)
}

// ---- strategies ----

fn edit() -> impl Strategy<Value = Edit> {
    select(vec![Edit::Signature, Edit::Body])
}

fn flag() -> proptest::bool::Any {
    proptest::bool::ANY
}

fn variants(knobs: impl Strategy<Value = Knobs>) -> impl Strategy<Value = (Edit, Knobs)> {
    (edit(), knobs)
}

// ---- the properties ----

fn transformed(
    lang: Lang,
    id: &str,
    guards: &str,
    knobs: impl Strategy<Value = Knobs>,
    with_changed: bool,
    extra: impl Fn(&Built, &Value) -> Result<(), String>,
) {
    check(id, guards, &variants(knobs), |(edit, knobs)| {
        let base = Built::new(seed(lang, edit, &Knobs::default()));
        let variant = Built::new(seed(lang, edit, &knobs));
        let impact = same_answers(&base, &variant, with_changed)?;
        extra(&variant, &impact)
    });
}

// Indentation, blank lines, a trailing comment and padding inside the call's
// parentheses change no relation.
fn reformat_layout(lang: Lang, id: &str) {
    let knobs = (select(vec![2usize, 4]), 0usize..=3, flag(), 0usize..=2).prop_map(
        |(indent, blank_lines, trailing_comment, spaces)| Knobs {
            indent,
            blank_lines,
            trailing_comment,
            spaces,
            ..Knobs::default()
        },
    );
    transformed(lang, id, "gh#442 gh#498", knobs, true, |_, _| Ok(()));
}

// A call wrapped in parentheses is the same call.
fn reformat_parens(lang: Lang, id: &str) {
    let knobs = (0usize..=2, 0usize..=1).prop_map(|(spaces, blank_lines)| Knobs {
        paren: true,
        spaces,
        blank_lines,
        ..Knobs::default()
    });
    transformed(lang, id, "gh#520", knobs, true, |_, _| Ok(()));
}

// A call inside a string interpolation is the same call.
fn reformat_interpolation(lang: Lang, id: &str) {
    let knobs = (flag(), 0usize..=2).prop_map(|(paren, spaces)| Knobs {
        interp: true,
        paren,
        spaces,
        ..Knobs::default()
    });
    transformed(lang, id, "gh#460 gh#475", knobs, true, |_, _| Ok(()));
}

// Unrelated statements on both sides of the call in a caller keep the edge.
fn unrelated_in_function(lang: Lang, id: &str) {
    let knobs = (1usize..=6).prop_map(|extra_in_fn| Knobs {
        extra_in_fn,
        ..Knobs::default()
    });
    transformed(lang, id, "gh#476", knobs, true, |_, _| Ok(()));
}

// Unrelated definitions, constants and files keep every relation.
fn unrelated_module_level(lang: Lang, id: &str) {
    let knobs = (1usize..=4, 0usize..=3).prop_map(|(extra_defs, extra_files)| Knobs {
        extra_defs,
        extra_files,
        ..Knobs::default()
    });
    transformed(lang, id, "gh#449 gh#476", knobs, true, |_, _| Ok(()));
}

fn decoys_absent(built: &Built, impact: &Value) -> Result<(), String> {
    let text = impact.to_string();
    match built
        .seed
        .decoys
        .iter()
        .find(|path| text.contains(path.as_str()))
    {
        Some(path) => Err(format!("the decoy {path} appears in the answer")),
        None => Ok(()),
    }
}

// A same-named definition in a sibling package, in another project root, and
// in a string and a comment never reaches the answer.
fn homonym_decoys(lang: Lang, id: &str) {
    let knobs = (flag(), flag(), flag()).prop_map(|(package, root, text)| {
        let none = !(package || root || text);
        Knobs {
            decoy_package: package || none,
            decoy_root: root,
            decoy_text: text,
            ..Knobs::default()
        }
    });
    transformed(lang, id, "gh#649 gh#666", knobs, true, decoys_absent);
}

fn moves_quietly(built: &Built, impact: &Value) -> Result<(), String> {
    if let Some(gone) = removed_rows(impact).first() {
        return Err(format!("{gone} reads as removed"));
    }
    let rows = changed_rows(impact);
    match built
        .seed
        .moved
        .iter()
        .find(|path| rows.iter().any(|row| row.starts_with(&format!("{path}::"))))
    {
        Some(path) => Err(format!("the pure move of {path} lists a definition")),
        None => Ok(()),
    }
}

// An unrelated file renamed byte for byte in the range is not a change.
fn rename_in_range(lang: Lang, id: &str) {
    let knobs = (1usize..=3, flag()).prop_map(|(history_split, reverse_write_order)| Knobs {
        rename_in_range: true,
        history_split,
        reverse_write_order,
        ..Knobs::default()
    });
    transformed(lang, id, "gh#593 gh#648", knobs, true, moves_quietly);
}

// A caller's file renamed before the range keeps its relations under the new
// name.
fn rename_prior(lang: Lang, id: &str) {
    let knobs = (1usize..=3, flag()).prop_map(|(history_split, reverse_write_order)| Knobs {
        rename_prior: true,
        history_split,
        reverse_write_order,
        ..Knobs::default()
    });
    transformed(lang, id, "gh#540", knobs, true, |_, _| Ok(()));
}

// A definition moved verbatim to a new file in the range is not removed.
fn move_helper(lang: Lang, id: &str) {
    let knobs = (1usize..=3, flag()).prop_map(|(history_split, reverse_write_order)| Knobs {
        move_helper: true,
        history_split,
        reverse_write_order,
        ..Knobs::default()
    });
    transformed(lang, id, "gh#540", knobs, false, moves_quietly);
}

// The order files were written and committed in, a repeat, concurrent runs
// and one thread give one answer.
fn order_and_repeat(lang: Lang, id: &str) {
    let draw = (edit(), 0usize..=40, 1usize..=4, flag());
    check(
        id,
        "gh#447",
        &draw,
        |(edit, extra_callers, history_split, reverse)| {
            let plain = Knobs {
                extra_callers,
                ..Knobs::default()
            };
            let shuffled = Knobs {
                history_split,
                reverse_write_order: reverse,
                ..plain
            };
            let (a, b) = (
                Built::new(seed(lang, edit, &plain)),
                Built::new(seed(lang, edit, &shuffled)),
            );
            for mode in [Mode::Impact, Mode::Symbol] {
                compare(
                    &format!("{mode:?} after another write order"),
                    &a.ask(mode, &[], &[])?,
                    &b.ask(mode, &[], &[])?,
                    &[],
                    true,
                )?;
            }
            let first = b.ask(Mode::Impact, &[], &[])?;
            let again = b.ask(Mode::Impact, &[], &[])?;
            if first != again {
                return Err("a repeated run gave another answer".to_string());
            }
            let single = b.ask(Mode::Impact, &[], &[("RAYON_NUM_THREADS", "1")])?;
            if first != single {
                return Err("a run on one thread gave another answer".to_string());
            }
            let concurrent: Vec<Result<Value, String>> = std::thread::scope(|scope| {
                // Every run starts before the first is joined.
                #[allow(clippy::needless_collect)]
                let runs: Vec<_> = (0..3)
                    .map(|_| scope.spawn(|| b.ask(Mode::Impact, &[], &[])))
                    .collect();
                runs.into_iter()
                    .map(|run| run.join().expect("concurrent run"))
                    .collect()
            });
            for answer in concurrent {
                if answer? != first {
                    return Err("a concurrent run gave another answer".to_string());
                }
            }
            Ok(())
        },
    );
}

// A larger budget keeps every claim a smaller one made and never narrows a
// section, with stale references and many callers in play.
fn budget_growth(lang: Lang, id: &str) {
    check(
        id,
        "gh#663 gh#691",
        &(edit(), 6usize..=60),
        |(edit, extra_callers)| {
            let knobs = Knobs {
                extra_callers,
                stale: true,
                ..Knobs::default()
            };
            let built = Built::new(seed(lang, edit, &knobs));
            let mut answers = Vec::new();
            for budget in BUDGETS {
                let arg = budget.to_string();
                answers.push((
                    budget,
                    built.ask(Mode::Impact, &["--budget", arg.as_str()], &[])?,
                ));
            }
            for pair in answers.windows(2) {
                let ((small, narrow), (large, wide)) = (&pair[0], &pair[1]);
                let (kept, wider) = (relations(narrow), relations(wide));
                if !kept.is_subset(&wider) {
                    return Err(format!(
                        "budget {large} lost claims budget {small} made\n{}",
                        difference(&kept, &wider)
                    ));
                }
                for ((name, before), (_, after)) in
                    section_sizes(narrow).iter().zip(section_sizes(wide))
                {
                    if *before > after {
                        return Err(format!(
                            "section {name} narrowed from {before} to {after} between budgets {small} and {large}"
                        ));
                    }
                }
                let shed = |doc: &Value| doc["truncated"].as_u64().unwrap_or(0);
                if shed(wide) > shed(narrow) {
                    return Err(format!(
                        "budget {large} shed {} callers, budget {small} only {}",
                        shed(wide),
                        shed(narrow)
                    ));
                }
            }
            Ok(())
        },
    );
}

// The plain seed is understood: exactly its callers, each resolved, the test
// that reaches the first, and nothing else. Without this, a property that
// compares two equally blind answers would pass.
fn seed_names_its_callers(lang: Lang, id: &str) {
    check_once(id, "gh#505", || {
        for edit in [Edit::Signature, Edit::Body] {
            let built = Built::new(seed(lang, edit, &Knobs::default()));
            let s = &built.seed;
            let target = format!("{}::{}", s.target_path, s.target);
            let mut want: BTreeSet<String> = s
                .callers
                .iter()
                .map(|(path, symbol)| format!("{target} <- {path}::{symbol} [call, resolved]"))
                .collect();
            want.extend(
                s.tested
                    .iter()
                    .map(|(caller, test)| format!("{caller}::charge is tested by {test}")),
            );
            for mode in [Mode::Impact, Mode::Symbol] {
                let got = relations(&built.ask(mode, &[], &[])?);
                if got != want {
                    return Err(format!(
                        "{edit:?} {mode:?}: the seed is not understood\n{}",
                        difference(&want, &got)
                    ));
                }
            }
        }
        Ok(())
    });
}

macro_rules! property_tests {
    ($property:ident, $function:ident: $($test:ident => $lang:ident),+) => {
        mod $property {
            use super::Lang;

            $(
                #[test]
                fn $test() {
                    super::$function(
                        Lang::$lang,
                        concat!(stringify!($property), "/", stringify!($test)),
                    );
                }
            )+
        }
    };
}

macro_rules! every_language {
    ($property:ident, $function:ident) => {
        property_tests!($property, $function: python => Python, typescript => TypeScript, rust => Rust, kotlin => Kotlin);
    };
}

every_language!(seed_understood, seed_names_its_callers);
every_language!(reformat_layout, reformat_layout);
every_language!(reformat_parens, reformat_parens);
every_language!(unrelated_in_function, unrelated_in_function);
every_language!(unrelated_module_level, unrelated_module_level);
every_language!(homonym_decoys, homonym_decoys);
every_language!(rename_in_range, rename_in_range);
every_language!(rename_prior, rename_prior);
every_language!(move_helper, move_helper);
every_language!(order_and_repeat, order_and_repeat);
every_language!(budget_growth, budget_growth);
// Rust has no string interpolation of a call.
property_tests!(reformat_interpolation, reformat_interpolation: python => Python, typescript => TypeScript, kotlin => Kotlin);

// The known-red entries name properties that exist.
#[test]
fn known_red_names_a_property_and_an_issue() {
    let properties = [
        "seed_understood",
        "reformat_layout",
        "reformat_parens",
        "reformat_interpolation",
        "unrelated_in_function",
        "unrelated_module_level",
        "homonym_decoys",
        "rename_in_range",
        "rename_prior",
        "move_helper",
        "order_and_repeat",
        "budget_growth",
    ];
    let languages = ["python", "typescript", "rust", "kotlin"];
    for (id, issue) in KNOWN_RED {
        let (property, language) = id.split_once('/').expect("property/language");
        assert!(properties.contains(&property), "{id}: no such property");
        assert!(languages.contains(&language), "{id}: no such language");
        assert!(
            issue.starts_with("gh#") || issue.starts_with("fj#"),
            "{id}: {issue} is not gh#N or fj#N"
        );
    }
}

// ---- absence accounting: a seed per family (arch migration, phase 1) ----

// The answer to every question about a seed that has a caller: the link is
// named or a gap is, and the change is never said to reach nothing.
fn link_or_gap(family: Family, shape: &Shape, id: &str) {
    answers(family, shape, id, false);
}

// The same question where the link exists: a gap no longer stands for it.
fn link(family: Family, shape: &Shape, id: &str) {
    answers(family, shape, id, true);
}

fn answers(family: Family, shape: &Shape, id: &str, link_only: bool) {
    check_once(id, "arch/p1 fj#113", || {
        let planted = Planted::new(families::with_caller(family, shape));
        let caller = planted.repo.caller.clone().expect("a seed has a caller");
        for query in queries(family) {
            let doc = planted.ask(query, &[])?;
            if says_empty(&doc) {
                return Err(format!(
                    "{query:?}: the answer says the change reaches nothing, and {caller} calls the definition"
                ));
            }
            if !names_link(&doc, &caller) && (link_only || !discloses(&doc)) {
                return Err(format!(
                    "{query:?}: the answer names neither the link to {caller} nor a gap\n{doc:#}"
                ));
            }
        }
        Ok(())
    });
}

// The control: nothing calls the definition, in any file, so the answer is a
// clean absence: no limit, no unanalysed file, no search that says it was
// partial, and for the families that say it so, `empty`.
fn clean_absence(family: Family, broken_unrelated: bool, id: &str) {
    check_once(id, "arch/p1", || {
        let planted = Planted::new(families::control(family, broken_unrelated));
        for query in queries(family) {
            let doc = planted.ask(query, &[])?;
            if discloses(&doc) {
                return Err(format!(
                    "{query:?}: a definition nothing calls got a gap, which hedges everything\n{doc:#}"
                ));
            }
            if query == Query::Impact && family.proves_empty() && !says_empty(&doc) {
                return Err(format!(
                    "{query:?}: a definition nothing calls is not stated as reaching nothing\n{doc:#}"
                ));
            }
        }
        Ok(())
    });
}

fn deep_call(family: Family, depth: usize) {
    let shape = Shape {
        depth,
        ..Shape::default()
    };
    let id = format!("deep_call_{depth}/{}", family.label());
    link_or_gap(family, &shape, &id);
}

fn syntax_error(family: Family, near: Break) {
    let property = match near {
        Break::Declaration => "syntax_error_near_declaration",
        Break::Call => "syntax_error_near_call",
    };
    let shape = Shape {
        broken: Some(near),
        ..Shape::default()
    };
    link_or_gap(family, &shape, &format!("{property}/{}", family.label()));
}

fn over_the_size_cap(family: Family) {
    let shape = Shape {
        oversize: true,
        ..Shape::default()
    };
    let id = format!("caller_over_the_size_cap/{}", family.label());
    link_or_gap(family, &shape, &id);
}

fn in_a_neighbour(family: Family, neighbour: Neighbour) {
    let shape = Shape {
        neighbour: Some(neighbour),
        ..Shape::default()
    };
    let id = format!(
        "caller_in_a_neighbour_language/{}_{}",
        family.label(),
        neighbour.label()
    );
    // Kotlin and Java read each other's names (arch/p3 stream 1): the caller is a
    // link, never only a gap.
    let jvm = matches!(
        (family, neighbour),
        (Family::Kotlin, Neighbour::Java) | (Family::Java, Neighbour::Kotlin)
    );
    if jvm {
        link(family, &shape, &id);
    } else {
        link_or_gap(family, &shape, &id);
    }
}

// A package, module or directory named by one letter, alone and with a caller
// no extractor reads whole.
fn one_letter(family: Family, broken: Option<Break>, oversize: bool) {
    let shape = Shape {
        one_letter: true,
        broken,
        oversize,
        ..Shape::default()
    };
    let property = match (broken, oversize) {
        (Some(_), _) => "one_letter_name_and_a_broken_caller",
        (None, true) => "one_letter_name_and_an_oversize_caller",
        (None, false) => "one_letter_name",
    };
    link_or_gap(family, &shape, &format!("{property}/{}", family.label()));
}

// One test per family under `module`, each calling `$call` with its family.
macro_rules! family_tests {
    ($module:ident, $call:expr, [$($test:ident: $family:ident),+ $(,)?]) => {
        mod $module {
            use super::Family;

            $(
                #[test]
                fn $test() {
                    ($call)(Family::$family);
                }
            )+
        }
    };
}

macro_rules! every_family {
    ($module:ident, $call:expr) => {
        family_tests!(
            $module,
            $call,
            [
                python: Python,
                typescript: TypeScript,
                rust: Rust,
                kotlin: Kotlin,
                java: Java,
                go: Go,
                php: Php,
                c: C,
                config: Config,
                terraform: Terraform,
                dockerfile: Dockerfile,
                shell: Shell,
            ]
        );
    };
}

// A Dockerfile stage is read by a stage of the same file: nothing nests.
macro_rules! every_nesting_family {
    ($module:ident, $call:expr) => {
        family_tests!(
            $module,
            $call,
            [
                python: Python,
                typescript: TypeScript,
                rust: Rust,
                kotlin: Kotlin,
                java: Java,
                go: Go,
                php: Php,
                c: C,
                config: Config,
                terraform: Terraform,
                shell: Shell,
            ]
        );
    };
}

every_nesting_family!(deep_call_10, |family| super::deep_call(family, 10));
every_nesting_family!(deep_call_300, |family| super::deep_call(family, 300));
every_nesting_family!(deep_call_2000, |family| super::deep_call(family, 2000));
every_family!(syntax_error_near_declaration, |family| {
    super::syntax_error(family, super::Break::Declaration)
});
every_family!(syntax_error_near_call, |family| {
    super::syntax_error(family, super::Break::Call)
});
every_family!(caller_over_the_size_cap, |family| {
    super::over_the_size_cap(family)
});
every_family!(one_letter_name, |family| {
    super::one_letter(family, None, false)
});
every_family!(one_letter_name_and_a_broken_caller, |family| {
    super::one_letter(family, Some(super::Break::Call), false)
});
every_family!(one_letter_name_and_an_oversize_caller, |family| {
    super::one_letter(family, None, true)
});
every_family!(absence_control, |family: super::Family| {
    super::clean_absence(
        family,
        false,
        &format!("absence_control/{}", family.label()),
    )
});
every_family!(
    absence_control_with_a_broken_unrelated_file,
    |family: super::Family| {
        super::clean_absence(
            family,
            true,
            &format!(
                "absence_control_with_a_broken_unrelated_file/{}",
                family.label()
            ),
        )
    }
);

macro_rules! neighbours {
    ($($test:ident: $family:ident, $neighbour:ident;)+) => {
        mod caller_in_a_neighbour_language {
            use super::{Family, Neighbour};

            $(
                #[test]
                fn $test() {
                    super::in_a_neighbour(Family::$family, Neighbour::$neighbour);
                }
            )+
        }
    };
}

neighbours! {
    kotlin_java: Kotlin, Java;
    kotlin_scala: Kotlin, Scala;
    java_kotlin: Java, Kotlin;
    java_groovy: Java, Groovy;
    c_cpp: C, Cpp;
    c_cpp_definition: C, CppDefinition;
    typescript_js: TypeScript, Js;
    typescript_vue: TypeScript, Vue;
    typescript_svelte: TypeScript, Svelte;
    typescript_html: TypeScript, Html;
    typescript_astro: TypeScript, Astro;
}

// Every family has a seed whose caller is a file of the repository and whose
// change touches the definition, and a control without a caller.
#[test]
fn every_family_has_a_seed_with_a_caller_and_a_control_without() {
    for family in Family::ALL {
        let plain = families::with_caller(family, &Shape::default());
        let caller = plain.caller.as_deref().expect("a caller");
        assert!(
            plain.base.contains_key(caller),
            "{}: the caller {caller} is not a file of the seed",
            family.label()
        );
        assert!(
            plain.change.contains_key(&plain.definition),
            "{}: the change does not touch {}",
            family.label(),
            plain.definition
        );
        let control = families::control(family, false);
        assert!(control.caller.is_none(), "{}", family.label());
        assert!(
            control.change.contains_key(&control.definition),
            "{}: the control's change does not touch {}",
            family.label(),
            control.definition
        );
    }
}
