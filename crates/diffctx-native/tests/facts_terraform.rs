//! The Terraform extractor, fact kind by fact kind, on source small enough to
//! read the right answer off. Nothing here runs diffctx or terraform to learn an
//! answer.

use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use _diffctx::facts::lang::terraform::TerraformExtractor;
use _diffctx::facts::{
    BlobFacts, Def, DefKind, Extractor, Fact, LocalKey, Receiver, Ref, Scope, Visibility,
};
use _diffctx::snapshot::{Loss, RunBudget, Stage};

fn extract(source: &str) -> BlobFacts {
    TerraformExtractor.extract(source.as_bytes(), &RunBudget::default())
}

fn refs(blob: &BlobFacts) -> Vec<&Ref> {
    blob.facts
        .iter()
        .filter_map(|f| match f {
            Fact::Reference(r) => Some(r),
            _ => None,
        })
        .collect()
}

/// `aws_s3_bucket.logs`: the container, then the name, `#` after a twin.
fn key(local: &LocalKey) -> String {
    let path: Vec<&str> = local
        .container
        .iter()
        .map(|c| c.as_str())
        .chain([local.name.as_str()])
        .collect();
    let twin = if local.disambig.is_some() { "#" } else { "" };
    format!("{}{twin}", path.join("."))
}

fn scope(scope: &Scope) -> String {
    match scope {
        Scope::Module => "module".to_string(),
        Scope::Def(local) => format!("def:{}", key(local)),
        Scope::Callback { .. } => "callback".to_string(),
    }
}

fn lines_of_defs(source: &str) -> Vec<String> {
    extract(source)
        .defs()
        .map(|d| format!("{:?} {}", d.kind, key(&d.local)))
        .collect()
}

fn def<'a>(blob: &'a BlobFacts, wanted: &str) -> &'a Def {
    blob.defs()
        .find(|d| key(&d.local) == wanted)
        .unwrap_or_else(|| panic!("no definition {wanted}"))
}

/// `Read module.net.id in def:aws_instance.web`.
fn ref_line(r: &Ref) -> String {
    let qualifier: Vec<&str> = r.qualifier.iter().map(|q| q.as_str()).collect();
    let written = if qualifier.is_empty() {
        r.name.as_str().to_string()
    } else {
        format!("{}.{}", qualifier.join("."), r.name.as_str())
    };
    format!("{:?} {written} in {}", r.use_kind, scope(&r.enclosing))
}

fn ref_lines(source: &str) -> Vec<String> {
    refs(&extract(source)).into_iter().map(ref_line).collect()
}

const MODULE: &str = "variable \"region\" {\n  type    = string\n  default = \"eu\"\n}\n\nlocals {\n  a = 1\n  b = local.a\n}\n\noutput \"id\" {\n  value = aws_s3_bucket.logs.id\n}\n\nresource \"aws_s3_bucket\" \"logs\" {\n  bucket = \"${var.region}-logs\"\n}\n\ndata \"aws_ami\" \"base\" {\n  most_recent = true\n}\n\nmodule \"net\" {\n  source = \"./net\"\n  cidr   = var.region\n}\n";

#[test]
fn every_address_of_a_module_is_a_definition_under_its_container() {
    assert_eq!(
        lines_of_defs(MODULE),
        [
            "Variable var.region",
            "Constant local.a",
            "Constant local.b",
            "Variable output.id",
            "Variable aws_s3_bucket.logs",
            "Variable data.aws_ami.base",
            "Variable module.net",
        ]
    );
}

#[test]
fn the_interface_of_a_module_is_public_and_the_rest_is_not() {
    let blob = extract(MODULE);
    for (wanted, visibility) in [
        ("var.region", Visibility::Public),
        ("output.id", Visibility::Public),
        ("local.a", Visibility::Private),
        ("aws_s3_bucket.logs", Visibility::Private),
        ("module.net", Visibility::Private),
    ] {
        assert_eq!(def(&blob, wanted).visibility, visibility, "{wanted}");
    }
}

#[test]
fn a_reference_is_owned_by_the_definition_that_holds_it() {
    assert_eq!(
        ref_lines(MODULE),
        [
            "Read local.a in def:local.b",
            "Read aws_s3_bucket.logs in def:output.id",
            "Read var.region in def:aws_s3_bucket.logs",
            "Write module.net.cidr in def:module.net",
            "Read var.region in def:module.net",
        ]
    );
}

#[test]
fn the_declaration_without_its_body_is_the_signature() {
    let blob = extract(
        "variable \"n\" {\n  type = list(string)\n  default = []\n}\n\nvariable \"m\" {}\n\nmodule \"net\" {\n  source  = \"./net\"\n  version = \"1.2\"\n}\n\nmodule \"dyn\" {\n  source = \"${var.root}/x\"\n}\n\nmodule \"reg\" {\n  source = \"hashicorp/consul/aws\"\n}\n",
    );
    let signature = |wanted: &str| def(&blob, wanted).signature.clone().unwrap_or_default();
    assert_eq!(signature("var.n"), "variable n type list ( string )");
    assert_eq!(signature("var.m"), "variable m");
    assert_eq!(
        signature("module.net"),
        "module net source ./net version 1.2"
    );
    assert_eq!(signature("module.dyn"), "module dyn source <computed>");
    assert_eq!(
        signature("module.reg"),
        "module reg source hashicorp/consul/aws"
    );
}

#[test]
fn every_form_of_traversal_names_what_it_reads() {
    let source = "resource \"aws_instance\" \"web\" {\n  ami       = data.aws_ami.base.id\n  subnet_id = module.net.subnet_ids[0]\n  tags      = merge(local.tags, { Name = var.name, (var.key) = module.net })\n  count     = length(var.zones)\n  zone      = var.zones[count.index]\n  ids       = [for z in var.zones : z.id if z.up]\n  cidr      = path.module\n  bucket    = aws_s3_bucket.logs[*].id\n\n  dynamic \"rule\" {\n    for_each = var.rules\n    content {\n      port = rule.value.port\n    }\n  }\n}\n";
    let expected: Vec<String> = [
        "Read data.aws_ami.base",
        "Read module.net.subnet_ids",
        "Read local.tags",
        "Read var.name",
        "Read var.key",
        "Read module.net",
        "Read var.zones",
        "Read var.zones",
        "Read var.zones",
        "Read aws_s3_bucket.logs",
        "Read var.rules",
    ]
    .iter()
    .map(|line| format!("{line} in def:aws_instance.web"))
    .collect();
    assert_eq!(ref_lines(source), expected);
}

#[test]
fn a_name_bound_by_the_expression_around_it_is_no_reference() {
    assert_eq!(
        ref_lines("output \"o\" {\n  value = { for k, v in var.map : k => v.id }\n}\n"),
        ["Read var.map in def:output.o"]
    );
}

#[test]
fn an_argument_of_a_module_call_is_a_write_of_the_variable_it_names() {
    assert_eq!(
        ref_lines(
            "module \"m\" {\n  source     = \"./m\"\n  count      = 2\n  depends_on = [aws_s3_bucket.b]\n  size       = var.n\n}\n"
        ),
        [
            "Read aws_s3_bucket.b in def:module.m",
            "Write module.m.size in def:module.m",
            "Read var.n in def:module.m",
        ]
    );
}

#[test]
fn an_assignment_of_a_variable_file_is_a_write_of_the_variable() {
    let blob = extract("region = \"eu\"\nzones  = [\"a\", \"b\"]\n");
    assert_eq!(
        refs(&blob).into_iter().map(ref_line).collect::<Vec<_>>(),
        ["Write var.region in module", "Write var.zones in module"]
    );
    assert_eq!(blob.defs().count(), 0);
    assert!(refs(&blob).iter().all(|r| r.receiver == Receiver::None));
}

#[test]
fn what_a_provider_or_terraform_block_reads_belongs_to_the_module() {
    assert_eq!(
        ref_lines("provider \"aws\" {\n  region = var.region\n}\n"),
        ["Read var.region in module"]
    );
}

#[test]
fn twins_are_told_apart_by_order() {
    let found = lines_of_defs("variable \"x\" {}\n\nvariable \"x\" {}\n");
    assert_eq!(found, ["Variable var.x", "Variable var.x#"]);
}

#[test]
fn the_hashes_tell_a_comment_from_a_change() {
    let variable = |default: &str, comment: &str| {
        extract(&format!(
            "variable \"n\" {{\n{comment}  default = {default}\n}}\n"
        ))
    };
    let base = variable("1", "");
    let edited = variable("2", "");
    let commented = variable("1", "  # how many\n");
    let (base_def, edited_def, commented_def) = (
        def(&base, "var.n"),
        def(&edited, "var.n"),
        def(&commented, "var.n"),
    );
    assert_ne!(base_def.body_hash, edited_def.body_hash);
    assert_eq!(base_def.signature, edited_def.signature);
    assert_eq!(base_def.body_hash, commented_def.body_hash);
    assert_ne!(base_def.text_hash, commented_def.text_hash);
    assert_ne!(base_def.body_hash, 0);
}

#[test]
fn a_local_is_a_definition_of_its_own_with_its_own_hashes() {
    let locals = |b: &str| extract(&format!("locals {{\n  a = 1\n  b = {b}\n}}\n"));
    let (before, after) = (locals("2"), locals("3"));
    assert_eq!(
        def(&before, "local.a").body_hash,
        def(&after, "local.a").body_hash
    );
    assert_ne!(
        def(&before, "local.b").body_hash,
        def(&after, "local.b").body_hash
    );
}

#[test]
fn a_parse_error_is_a_loss_and_the_rest_is_read() {
    let blob = extract("variable \"a\" {}\n\nvariable \"b\" {\n");
    assert!(
        blob.losses
            .iter()
            .any(|l| matches!(l, Loss::ParseError { .. })),
        "{:?}",
        blob.losses
    );
    assert!(blob.defs().any(|d| key(&d.local) == "var.a"));
}

#[test]
fn an_expired_budget_is_a_deadline_loss() {
    let budget = RunBudget {
        cancel: Some(Arc::new(AtomicBool::new(true))),
        ..RunBudget::default()
    };
    let blob = TerraformExtractor.extract(b"variable \"a\" {}\n", &budget);
    assert!(
        blob.losses.contains(&Loss::Deadline {
            stage: Stage::Extraction
        }),
        "{:?}",
        blob.losses
    );
}

#[test]
fn definitions_are_in_source_order_and_kinds_are_what_the_block_says() {
    let blob = extract(MODULE);
    let kinds: Vec<DefKind> = blob.defs().map(|d| d.kind).collect();
    // A resource, a data source and a module call hold an object: `Variable`.
    assert_eq!(
        kinds,
        [
            DefKind::Variable,
            DefKind::Constant,
            DefKind::Constant,
            DefKind::Variable,
            DefKind::Variable,
            DefKind::Variable,
            DefKind::Variable,
        ]
    );
}
