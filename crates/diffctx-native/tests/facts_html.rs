//! The page extractors: the scripts of a page are read where they stand, so a
//! fact's lines are the page's own, and nothing outside code is read.

use _diffctx::facts::lang::html::{AstroExtractor, HtmlExtractor};
use _diffctx::facts::{
    BlobFacts, DefKind, Extractor, Fact, Import, ImportKind, Language, Ref, UseKind,
};
use _diffctx::snapshot::RunBudget;

fn page(source: &str) -> BlobFacts {
    HtmlExtractor.extract(source.as_bytes(), &RunBudget::default())
}

fn astro(source: &str) -> BlobFacts {
    AstroExtractor.extract(source.as_bytes(), &RunBudget::default())
}

fn defs(blob: &BlobFacts) -> Vec<(String, (u32, u32))> {
    blob.defs()
        .map(|d| (d.local.name.as_str().to_string(), d.span.lines))
        .collect()
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

fn imports(blob: &BlobFacts) -> Vec<&Import> {
    blob.facts
        .iter()
        .filter_map(|f| match f {
            Fact::Import(i) => Some(i),
            _ => None,
        })
        .collect()
}

const PAGE: &str = "<!doctype html>
<html><body>
<script>
function format(n) {
  return n.toFixed(2);
}

function render(n) {
  document.body.textContent = format(n);
}

render(3);
</script>
</body></html>
";

#[test]
fn an_inline_script_is_read_at_its_lines_of_the_page() {
    let blob = page(PAGE);
    assert_eq!(blob.language, Language::Html);
    assert_eq!(
        defs(&blob),
        vec![
            ("format".to_string(), (4, 6)),
            ("render".to_string(), (8, 10))
        ]
    );
    let call = refs(&blob)
        .into_iter()
        .find(|r| r.name.as_str() == "format" && r.use_kind == UseKind::Call)
        .expect("render calls format");
    assert_eq!(call.site.lines, (9, 9));
    assert!(blob.losses.is_empty());
}

#[test]
fn a_definition_of_a_page_is_a_function() {
    let blob = page(PAGE);
    assert!(blob.defs().all(|d| d.kind == DefKind::Function));
}

#[test]
fn markup_text_and_comments_state_nothing() {
    let blob = page(
        "<!-- <script>function hidden() {}</script> -->\n<p>function shown() {}</p>\n<style>function css() {}</style>\n",
    );
    assert!(blob.facts.is_empty());
    assert!(blob.losses.is_empty());
}

#[test]
fn a_script_of_another_type_is_no_code() {
    let blob = page(
        "<script type=\"application/ld+json\">{\"a\": 1}</script>\n<script type=\"importmap\">{}</script>\n<script type=\"module\">export const x = 1;</script>\n",
    );
    assert_eq!(defs(&blob), vec![("x".to_string(), (3, 3))]);
    assert!(blob.losses.is_empty());
}

#[test]
fn a_classic_script_src_is_a_side_effect_import_of_the_file_it_names() {
    let blob = page(
        "<script src=\"lib.js\"></script>\n<script src=\"https://cdn.example/x.js\"></script>\n<script type=\"module\" src=\"app.js\"></script>\n<script src='../shared/util.js'></script>\n",
    );
    let loaded: Vec<(&str, u32, ImportKind)> = imports(&blob)
        .into_iter()
        .map(|i| (i.module.as_str(), i.relative, i.kind))
        .collect();
    assert_eq!(
        loaded,
        vec![
            ("lib.js", 0, ImportKind::SideEffect),
            ("../shared/util.js", 2, ImportKind::SideEffect),
        ]
    );
}

#[test]
fn a_module_script_keeps_its_imports() {
    let blob = page(
        "<script type=\"module\">\nimport { installTelemetry } from \"../shared/telemetry.js\";\ninstallTelemetry({});\n</script>\n",
    );
    let imported = imports(&blob);
    assert_eq!(imported.len(), 1);
    assert_eq!(imported[0].module.as_str(), "../shared/telemetry.js");
    assert_eq!(imported[0].site.lines, (2, 2));
    assert!(
        refs(&blob)
            .iter()
            .any(|r| r.name.as_str() == "installTelemetry" && r.site.lines == (3, 3))
    );
}

#[test]
fn an_astro_file_reads_its_frontmatter_and_its_scripts() {
    let blob = astro(
        "---\nimport Layout from '../layouts/Layout.astro';\nconst title: string = 'x';\nfunction shout(s: string) { return s; }\n---\n<Layout title={title} />\n<script>\n  function client() { shout('a'); }\n</script>\n",
    );
    assert_eq!(blob.language, Language::Astro);
    let names: Vec<(String, (u32, u32))> = defs(&blob);
    assert!(names.contains(&("shout".to_string(), (4, 4))), "{names:?}");
    assert!(names.contains(&("client".to_string(), (8, 8))), "{names:?}");
    assert_eq!(imports(&blob).len(), 1);
}
