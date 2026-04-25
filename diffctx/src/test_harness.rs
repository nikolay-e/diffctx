#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

use clap::Parser;
use rayon::prelude::*;
use rustc_hash::FxHashMap;
use serde::Deserialize;

use _diffctx::memory_pipeline::{MemoryRepo, build_diff_context_in_memory};
use _diffctx::mode::ScoringMode;
use _diffctx::render::FragmentEntry;
use _diffctx::tokenizer::count_tokens;

#[derive(Parser)]
#[command(name = "diffctx-test", about = "Run YAML diff test cases in-memory")]
struct Cli {
    #[arg(default_value = "../tests/cases/diff")]
    cases_dir: PathBuf,

    #[arg(short, long)]
    filter: Option<String>,

    #[arg(long)]
    tag: Option<String>,

    #[arg(long)]
    verbose: bool,

    #[arg(long)]
    xfail_only: bool,
}

#[derive(Deserialize, Clone)]
struct TestCase {
    name: String,
    #[serde(default)]
    tags: Vec<String>,
    #[serde(default)]
    repo: Repo,
    #[serde(default)]
    fixtures: Fixtures,
    #[serde(default)]
    fragments: Vec<DeclaredFragment>,
    #[serde(default)]
    oracle: Oracle,
    #[serde(default)]
    accept: Accept,
    #[serde(default)]
    xfail: Option<XFail>,
}

#[derive(Deserialize, Clone, Default)]
struct Repo {
    #[serde(default)]
    initial_files: FxHashMap<String, String>,
    #[serde(default)]
    changed_files: FxHashMap<String, String>,
    #[serde(default)]
    commit_message: Option<String>,
}

#[derive(Deserialize, Clone, Default)]
struct Fixtures {
    #[serde(default)]
    auto_garbage: bool,
    #[serde(default)]
    distractors: FxHashMap<String, String>,
}

#[derive(Deserialize, Clone)]
struct DeclaredFragment {
    id: String,
    #[serde(default)]
    selector: Selector,
}

#[derive(Deserialize, Clone, Default)]
struct Selector {
    path: Option<String>,
    symbol: Option<String>,
    kind: Option<String>,
    anchor: Option<String>,
    any_of: Option<Vec<Selector>>,
}

#[derive(Deserialize, Clone, Default)]
struct Oracle {
    #[serde(default)]
    required: Vec<String>,
    #[serde(default)]
    allowed: Vec<String>,
    #[serde(default)]
    forbidden: Vec<String>,
}

#[derive(Deserialize, Clone)]
struct Accept {
    #[serde(default = "default_symbol_match")]
    symbol_match: String,
    #[serde(default)]
    kind_must_match: bool,
    #[serde(default = "default_span_relation")]
    span_relation: String,
}

impl Default for Accept {
    fn default() -> Self {
        Self {
            symbol_match: default_symbol_match(),
            kind_must_match: false,
            span_relation: default_span_relation(),
        }
    }
}

fn default_symbol_match() -> String {
    "exact".to_string()
}
fn default_span_relation() -> String {
    "exact_or_enclosing".to_string()
}

#[derive(Deserialize, Clone)]
struct XFail {
    #[serde(default)]
    category: Option<String>,
    #[serde(default)]
    reason: Option<String>,
    #[serde(default)]
    issue: Option<String>,
}

#[derive(Debug)]
struct TestResult {
    name: String,
    passed: bool,
    xfail: bool,
    xfail_category: Option<String>,
    score: f64,
    missing_required: Vec<String>,
    hit_forbidden: Vec<String>,
    elapsed_secs: f64,
}

fn garbage_files() -> FxHashMap<String, String> {
    let mut m = FxHashMap::default();
    m.insert("unrelated_dir/garbage_utils.py".into(), "def completely_unrelated_garbage_function():\n    return \"garbage_marker_12345\"\n\ndef another_unused_helper():\n    return \"unused_marker_67890\"\n\nclass UnusedGarbageClass:\n    def useless_method(self):\n        return \"class_garbage_marker\"\n\ndef garbage_helper_alpha():\n    return \"garbage_helper_alpha_value\"\n\ndef garbage_helper_beta():\n    return \"garbage_helper_beta_value\"\n\ndef garbage_helper_gamma():\n    return \"garbage_helper_gamma_value\"\n\ndef garbage_helper_delta():\n    return \"garbage_helper_delta_value\"\n\ndef garbage_helper_epsilon():\n    return \"garbage_helper_epsilon_value\"\n".into());
    m.insert("unrelated_dir/garbage_constants.py".into(), "GARBAGE_CONSTANT_ALPHA = \"garbage_alpha_constant\"\nGARBAGE_CONSTANT_BETA = \"garbage_beta_constant\"\nUNUSED_CONFIG_VALUE = 99999\nGARBAGE_CONST_GAMMA = \"garbage_gamma_constant\"\nGARBAGE_CONST_DELTA = \"garbage_delta_constant\"\nGARBAGE_CONST_EPSILON = \"garbage_epsilon_constant\"\nGARBAGE_CONST_ZETA = \"garbage_zeta_constant\"\nGARBAGE_CONST_ETA = \"garbage_eta_constant\"\nGARBAGE_CONST_THETA = \"garbage_theta_constant\"\nGARBAGE_CONST_IOTA = \"garbage_iota_constant\"\nGARBAGE_CONST_KAPPA = \"garbage_kappa_constant\"\n".into());
    m.insert("unrelated_dir/garbage_module.js".into(), "export function unusedJsGarbage() {\n    return \"js_garbage_marker_abc\";\n}\n\nexport const GARBAGE_JS_CONST = \"js_const_garbage\";\n\nexport function garbageJsHelperAlpha() {\n    return \"js_garbage_alpha_marker\";\n}\n\nexport function garbageJsHelperBeta() {\n    return \"js_garbage_beta_marker\";\n}\n\nexport function garbageJsHelperGamma() {\n    return \"js_garbage_gamma_marker\";\n}\n\nexport const GARBAGE_JS_CONFIG = {\n    unusedKey1: \"garbage_config_value_1\",\n    unusedKey2: \"garbage_config_value_2\",\n    unusedKey3: \"garbage_config_value_3\",\n};\n".into());
    m.insert("unrelated_dir/garbage_types.py".into(), "class GarbageTypeAlpha:\n    def garbage_method_one(self):\n        return \"garbage_type_alpha_one\"\n\n    def garbage_method_two(self):\n        return \"garbage_type_alpha_two\"\n\n    def garbage_method_three(self):\n        return \"garbage_type_alpha_three\"\n\nclass GarbageTypeBeta:\n    def garbage_method_one(self):\n        return \"garbage_type_beta_one\"\n\n    def garbage_method_two(self):\n        return \"garbage_type_beta_two\"\n\nclass GarbageTypeGamma:\n    def garbage_method_one(self):\n        return \"garbage_type_gamma_one\"\n\n    def garbage_method_two(self):\n        return \"garbage_type_gamma_two\"\n\n    def garbage_method_three(self):\n        return \"garbage_type_gamma_three\"\n\n    def garbage_method_four(self):\n        return \"garbage_type_gamma_four\"\n".into());
    m.insert("unrelated_dir/garbage_services.py".into(), "class GarbageServiceAlpha:\n    def process_garbage_alpha(self, data):\n        return f\"garbage_service_alpha_{data}\"\n\n    def validate_garbage_alpha(self, item):\n        return \"garbage_validation_alpha\"\n\n    def transform_garbage_alpha(self, value):\n        return \"garbage_transform_alpha\"\n\nclass GarbageServiceBeta:\n    def process_garbage_beta(self, data):\n        return f\"garbage_service_beta_{data}\"\n\n    def validate_garbage_beta(self, item):\n        return \"garbage_validation_beta\"\n\nclass GarbageServiceGamma:\n    def process_garbage_gamma(self, data):\n        return f\"garbage_service_gamma_{data}\"\n\n    def execute_garbage_gamma(self, cmd):\n        return \"garbage_execution_gamma\"\n\ndef standalone_garbage_function_one():\n    return \"standalone_garbage_one\"\n\ndef standalone_garbage_function_two():\n    return \"standalone_garbage_two\"\n\ndef standalone_garbage_function_three():\n    return \"standalone_garbage_three\"\n".into());
    m.insert("unrelated_dir/garbage_handlers.py".into(), "class GarbageHandlerAlpha:\n    def handle_garbage_event_alpha(self, event):\n        return \"garbage_event_alpha_handled\"\n\n    def process_garbage_request_alpha(self, request):\n        return \"garbage_request_alpha_processed\"\n\nclass GarbageHandlerBeta:\n    def handle_garbage_event_beta(self, event):\n        return \"garbage_event_beta_handled\"\n\n    def process_garbage_request_beta(self, request):\n        return \"garbage_request_beta_processed\"\n\nclass GarbageHandlerGamma:\n    def handle_garbage_event_gamma(self, event):\n        return \"garbage_event_gamma_handled\"\n\ndef garbage_event_dispatcher(event_type):\n    return f\"garbage_dispatched_{event_type}\"\n\ndef garbage_request_router(path):\n    return f\"garbage_routed_{path}\"\n".into());
    m.insert("unrelated_dir/garbage_models.py".into(), "class GarbageModelAlpha:\n    garbage_field_one = \"garbage_model_alpha_field_one\"\n    garbage_field_two = \"garbage_model_alpha_field_two\"\n\n    def garbage_model_method_alpha(self):\n        return \"garbage_model_alpha_method\"\n\nclass GarbageModelBeta:\n    garbage_field_one = \"garbage_model_beta_field_one\"\n    garbage_field_two = \"garbage_model_beta_field_two\"\n    garbage_field_three = \"garbage_model_beta_field_three\"\n\n    def garbage_model_method_beta(self):\n        return \"garbage_model_beta_method\"\n\nclass GarbageModelGamma:\n    garbage_field_one = \"garbage_model_gamma_field_one\"\n\n    def garbage_model_method_gamma(self):\n        return \"garbage_model_gamma_method\"\n\n    def garbage_model_validate_gamma(self, data):\n        return \"garbage_model_gamma_validated\"\n".into());
    m.insert("unrelated_dir/garbage_api.py".into(), "def garbage_api_endpoint_alpha(request):\n    return {\"garbage_response\": \"alpha\"}\n\ndef garbage_api_endpoint_beta(request):\n    return {\"garbage_response\": \"beta\"}\n\ndef garbage_api_endpoint_gamma(request):\n    return {\"garbage_response\": \"gamma\"}\n\ndef garbage_api_endpoint_delta(request):\n    return {\"garbage_response\": \"delta\"}\n\nclass GarbageApiController:\n    def garbage_get_all(self):\n        return \"garbage_api_get_all\"\n\n    def garbage_get_one(self, id):\n        return f\"garbage_api_get_{id}\"\n\n    def garbage_create(self, data):\n        return \"garbage_api_created\"\n\n    def garbage_update(self, id, data):\n        return f\"garbage_api_updated_{id}\"\n\n    def garbage_delete(self, id):\n        return f\"garbage_api_deleted_{id}\"\n".into());
    m.insert("unrelated_dir/garbage_validators.py".into(), "def validate_garbage_input_alpha(value):\n    return \"garbage_input_alpha_valid\"\n\ndef validate_garbage_input_beta(value):\n    return \"garbage_input_beta_valid\"\n\ndef validate_garbage_input_gamma(value):\n    return \"garbage_input_gamma_valid\"\n\nclass GarbageValidatorAlpha:\n    def validate(self, data):\n        return \"garbage_validator_alpha_result\"\n\nclass GarbageValidatorBeta:\n    def validate(self, data):\n        return \"garbage_validator_beta_result\"\n\n    def validate_strict(self, data):\n        return \"garbage_validator_beta_strict\"\n".into());
    m.insert("unrelated_dir/garbage_unrelated.yaml".into(), "zxcvb_settings:\n  zxcvb_option_alpha: garbage_yaml_alpha_value\n  zxcvb_option_beta: garbage_yaml_beta_value\n  zxcvb_option_gamma: garbage_yaml_gamma_value\n\nzxcvb_features:\n  zxcvb_feature_one: true\n  zxcvb_feature_two: false\n".into());
    m
}

fn matches_selector(frag: &FragmentEntry, sel: &Selector, accept: &Accept) -> bool {
    if let Some(ref any_of) = sel.any_of {
        return any_of.iter().any(|s| matches_selector(frag, s, accept));
    }
    if let Some(ref path) = sel.path {
        if !frag.path.ends_with(path) && frag.path != *path {
            return false;
        }
    }
    if let Some(ref symbol) = sel.symbol {
        let frag_sym = frag.symbol.as_deref().unwrap_or("");
        match accept.symbol_match.as_str() {
            "prefix" => {
                if !frag_sym.starts_with(symbol.as_str()) {
                    return false;
                }
            }
            "substring" => {
                if !frag_sym.contains(symbol.as_str()) {
                    return false;
                }
            }
            _ => {
                if frag_sym != symbol {
                    return false;
                }
            }
        }
    }
    if let Some(ref kind) = sel.kind {
        if accept.kind_must_match && frag.kind != *kind {
            return false;
        }
    }
    if let Some(ref anchor) = sel.anchor {
        let in_content = frag
            .content
            .as_deref()
            .unwrap_or("")
            .contains(anchor.as_str());
        let in_path = frag.path.contains(anchor.as_str());
        if !in_content && !in_path {
            return false;
        }
    }
    true
}

fn fragment_matched(
    output_frags: &[FragmentEntry],
    decl: &DeclaredFragment,
    accept: &Accept,
) -> bool {
    output_frags
        .iter()
        .any(|f| matches_selector(f, &decl.selector, accept))
}

fn calculate_budget(case: &TestCase) -> u32 {
    let overhead: u32 = _diffctx::config::limits::LIMITS.overhead_per_fragment;
    let mut test_files: FxHashMap<String, String> = case.repo.initial_files.clone();
    for (k, v) in &case.repo.changed_files {
        test_files.insert(k.clone(), v.clone());
    }
    for (k, v) in &case.fixtures.distractors {
        test_files.insert(k.clone(), v.clone());
    }
    let content_tokens: u32 = test_files.values().map(|c| count_tokens(c)).sum();
    let estimated_fragments = (test_files.len().max(2)) as u32;
    let budget = (content_tokens + estimated_fragments * overhead) * 5 / 2;
    budget.max(500)
}

fn build_memory_repo(case: &TestCase) -> MemoryRepo {
    let mut initial = case.repo.initial_files.clone();
    let mut changed = case.repo.changed_files.clone();

    for (k, v) in &initial {
        if !changed.contains_key(k) {
            changed.insert(k.clone(), v.clone());
        }
    }

    for (k, v) in &case.fixtures.distractors {
        initial.insert(k.clone(), v.clone());
        changed.insert(k.clone(), v.clone());
    }

    if case.fixtures.auto_garbage {
        let garbage = garbage_files();
        for (k, v) in &garbage {
            initial.insert(k.clone(), v.clone());
            changed.insert(k.clone(), v.clone());
        }
    }

    MemoryRepo {
        name: case.name.clone(),
        initial_files: initial,
        changed_files: changed,
    }
}

fn evaluate_oracle(case: &TestCase, output_frags: &[FragmentEntry]) -> TestResult {
    let decl_by_id: FxHashMap<&str, &DeclaredFragment> =
        case.fragments.iter().map(|d| (d.id.as_str(), d)).collect();

    let mut required_hits = 0usize;
    let mut missing_required = Vec::new();
    for req_id in &case.oracle.required {
        if let Some(decl) = decl_by_id.get(req_id.as_str()) {
            if fragment_matched(output_frags, decl, &case.accept) {
                required_hits += 1;
            } else {
                missing_required.push(req_id.clone());
            }
        } else {
            missing_required.push(req_id.clone());
        }
    }

    let mut forbidden_hits = Vec::new();
    for forb_id in &case.oracle.forbidden {
        if let Some(decl) = decl_by_id.get(forb_id.as_str()) {
            if fragment_matched(output_frags, decl, &case.accept) {
                forbidden_hits.push(forb_id.clone());
            }
        }
    }

    let required_total = case.oracle.required.len();
    let forbidden_total = case.oracle.forbidden.len();

    let recall = if required_total == 0 {
        1.0
    } else {
        required_hits as f64 / required_total as f64
    };

    let forbidden_rate = if forbidden_total == 0 {
        0.0
    } else {
        forbidden_hits.len() as f64 / forbidden_total as f64
    };

    let score = 100.0 * recall * (1.0 - forbidden_rate);
    let passed = missing_required.is_empty() && forbidden_hits.is_empty();

    let is_xfail = case.xfail.is_some();
    let xfail_category = case.xfail.as_ref().and_then(|x| x.category.clone());

    TestResult {
        name: case.name.clone(),
        passed,
        xfail: is_xfail && !passed,
        xfail_category,
        score,
        missing_required,
        hit_forbidden: forbidden_hits,
        elapsed_secs: 0.0,
    }
}

fn run_single_test(case: &TestCase) -> TestResult {
    let t0 = Instant::now();
    let repo = build_memory_repo(case);
    let budget = calculate_budget(case);

    let output =
        build_diff_context_in_memory(&repo, Some(budget), 0.60, 0.05, false, ScoringMode::Hybrid);

    let mut result = evaluate_oracle(case, &output.fragments);
    result.elapsed_secs = t0.elapsed().as_secs_f64();

    if case.xfail.is_some() && !result.passed {
        result.xfail = true;
    }

    result
}

fn load_all_cases(dir: &Path) -> Vec<(PathBuf, TestCase)> {
    let pattern = format!("{}/**/*.yaml", dir.display());
    let mut cases = Vec::new();

    for entry in glob::glob(&pattern).expect("invalid glob pattern") {
        let path = match entry {
            Ok(p) => p,
            Err(_) => continue,
        };

        if path
            .file_name()
            .map_or(false, |n| n.to_string_lossy().starts_with('.'))
        {
            continue;
        }
        if path
            .file_name()
            .map_or(false, |n| n.to_string_lossy() == "SCHEMA.md")
        {
            continue;
        }

        let content = match std::fs::read_to_string(&path) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("WARN: cannot read {}: {}", path.display(), e);
                continue;
            }
        };

        if let Ok(case) = serde_yaml::from_str::<TestCase>(&content) {
            cases.push((path, case));
            continue;
        }

        if let Ok(list) = serde_yaml::from_str::<Vec<TestCase>>(&content) {
            for case in list {
                cases.push((path.clone(), case));
            }
            continue;
        }

        #[derive(Deserialize)]
        struct TestsWrapper {
            tests: Vec<TestCase>,
        }
        if let Ok(wrapper) = serde_yaml::from_str::<TestsWrapper>(&content) {
            for case in wrapper.tests {
                cases.push((path.clone(), case));
            }
            continue;
        }

        eprintln!("WARN: cannot parse {}", path.display());
    }

    cases.sort_by(|a, b| a.1.name.cmp(&b.1.name));
    cases
}

fn main() {
    let cli = Cli::parse();

    let all_cases = load_all_cases(&cli.cases_dir);
    let cases: Vec<&(PathBuf, TestCase)> = all_cases
        .iter()
        .filter(|(_, case)| {
            if let Some(ref f) = cli.filter {
                if !case.name.contains(f.as_str()) {
                    return false;
                }
            }
            if let Some(ref t) = cli.tag {
                if !case.tags.iter().any(|tag| tag.contains(t.as_str())) {
                    return false;
                }
            }
            if cli.xfail_only && case.xfail.is_none() {
                return false;
            }
            true
        })
        .collect();

    let total = cases.len();
    if total == 0 {
        eprintln!("No test cases found in {}", cli.cases_dir.display());
        std::process::exit(1);
    }

    let n_workers = rayon::current_num_threads();
    eprintln!("Running {} tests ({} workers)...", total, n_workers);

    let wall_start = Instant::now();
    let completed = AtomicUsize::new(0);

    let results: Vec<TestResult> = cases
        .par_iter()
        .map(|(_, case)| {
            let result = run_single_test(case);
            let done = completed.fetch_add(1, Ordering::Relaxed) + 1;
            if cli.verbose || !result.passed {
                let icon = if result.xfail {
                    "\u{2298}"
                } else if result.passed {
                    "\u{2713}"
                } else {
                    "\u{2717}"
                };
                let mut line = format!(
                    "[{:5.1}s] {} {} ({:.1}%)",
                    result.elapsed_secs, icon, result.name, result.score
                );
                if !result.missing_required.is_empty() {
                    line.push_str(&format!(
                        " [missing: {}]",
                        result.missing_required.join(", ")
                    ));
                }
                if !result.hit_forbidden.is_empty() {
                    line.push_str(&format!(
                        " [forbidden: {}]",
                        result.hit_forbidden.join(", ")
                    ));
                }
                if result.xfail {
                    if let Some(ref cat) = result.xfail_category {
                        line.push_str(&format!(" (xfail: {})", cat));
                    } else {
                        line.push_str(" (xfail)");
                    }
                }
                eprintln!("{}", line);
            }
            if cli.verbose && done % 500 == 0 {
                eprintln!("  ... {}/{} completed", done, total);
            }
            result
        })
        .collect();

    let wall_time = wall_start.elapsed().as_secs_f64();

    let passed = results.iter().filter(|r| r.passed && !r.xfail).count();
    let failed = results.iter().filter(|r| !r.passed && !r.xfail).count();
    let xfailed = results.iter().filter(|r| r.xfail).count();
    let xpassed = results
        .iter()
        .filter(|r| r.passed && r.xfail_category.is_some())
        .count();

    let score_100 = results
        .iter()
        .filter(|r| r.score >= 100.0 - f64::EPSILON)
        .count();
    let score_90 = results
        .iter()
        .filter(|r| r.score >= 90.0 && r.score < 100.0 - f64::EPSILON)
        .count();
    let score_70 = results
        .iter()
        .filter(|r| r.score >= 70.0 && r.score < 90.0)
        .count();
    let score_50 = results
        .iter()
        .filter(|r| r.score >= 50.0 && r.score < 70.0)
        .count();
    let score_below = results.iter().filter(|r| r.score < 50.0).count();

    eprintln!();
    eprintln!("{}", "=".repeat(50));
    eprintln!(
        "Results: {} passed, {} failed, {} xfailed{}",
        passed,
        failed,
        xfailed,
        if xpassed > 0 {
            format!(", {} xpassed", xpassed)
        } else {
            String::new()
        }
    );
    eprintln!("Score distribution:");
    eprintln!("  100%:  {}", score_100);
    eprintln!("  >=90%: {}", score_90);
    eprintln!("  >=70%: {}", score_70);
    eprintln!("  >=50%: {}", score_50);
    eprintln!("  <50%:  {}", score_below);
    eprintln!("Wall time: {:.1}s", wall_time);
    eprintln!("{}", "=".repeat(50));

    if failed > 0 {
        std::process::exit(1);
    }
}
