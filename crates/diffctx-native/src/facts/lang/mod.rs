//! The extractors the crate ships, registered by language in [`shipped`]: a
//! language file provides its extractor (the TypeScript file one per language it
//! reads, since it serves JavaScript too) and gains a line there; nothing else
//! changes when a language joins.

use std::sync::{Arc, OnceLock};

use crate::facts::extractor::{Extractor, Registry};
use crate::facts::hash::Hasher;
use crate::facts::language::Language;

pub mod c;
pub mod config;
pub mod dockerfile;
pub mod go;
pub mod helm;
pub mod html;
pub mod java;
pub mod kotlin;
pub mod php;
pub mod python;
pub mod rust;
pub mod shell;
pub mod sql;
pub mod swift;
pub mod terraform;
pub mod typescript;

fn shipped() -> Vec<Arc<dyn Extractor>> {
    let mut all: Vec<Arc<dyn Extractor>> = vec![
        Arc::new(python::PythonExtractor),
        Arc::new(java::JavaExtractor),
        Arc::new(go::GoExtractor),
        Arc::new(kotlin::KotlinExtractor),
        Arc::new(rust::RustExtractor),
        Arc::new(php::PhpExtractor),
        Arc::new(c::CExtractor),
        Arc::new(c::CppExtractor),
        Arc::new(config::ConfigExtractor),
        Arc::new(helm::HelmTemplateExtractor),
        Arc::new(helm::HelmValuesExtractor),
        Arc::new(terraform::TerraformExtractor),
        Arc::new(dockerfile::DockerfileExtractor),
        Arc::new(shell::ShellExtractor),
        Arc::new(shell::UnreadShellExtractor),
        Arc::new(html::HtmlExtractor),
        Arc::new(html::AstroExtractor),
        Arc::new(swift::SwiftExtractor),
        Arc::new(sql::SqlExtractor),
    ];
    all.extend(typescript::extractors());
    all
}

/// Every extractor the crate ships.
pub fn builtin() -> Registry {
    let mut registry = Registry::new();
    for extractor in shipped() {
        registry.register(extractor);
    }
    registry
}

static DIGESTS: [OnceLock<u64>; 21] = [const { OnceLock::new() }; 17];

const fn slot(language: Language) -> usize {
    match language {
        Language::Python => 0,
        Language::JavaScript => 1,
        Language::TypeScript => 2,
        Language::Rust => 3,
        Language::Kotlin => 4,
        Language::Go => 5,
        Language::Java => 6,
        Language::Php => 7,
        Language::C => 8,
        Language::Cpp => 9,
        Language::Config => 10,
        Language::HelmTemplate => 11,
        Language::HelmValues => 12,
        Language::Terraform => 13,
        Language::Dockerfile => 14,
        Language::Shell => 15,
        Language::ShellUnread => 16,
        Language::Html => 17,
        Language::Astro => 18,
        Language::Swift => 19,
        Language::Sql => 20,
    }
}

/// A fingerprint of the grammars a language is parsed with: the ABI version, the
/// sizes of the parse tables and every node kind name. Any grammar release that
/// changes what a parse can say changes it, so a cached fact never outlives the
/// grammar it was read with. `0` for a language whose grammar this build lacks.
pub fn grammar_digest(language: Language) -> u64 {
    *DIGESTS[slot(language)].get_or_init(|| {
        let mut hasher = Hasher::new();
        hasher.token(language.name().as_bytes());
        for grammar in grammars(language) {
            hasher.token(&(grammar.abi_version() as u64).to_le_bytes());
            hasher.token(&(grammar.parse_state_count() as u64).to_le_bytes());
            hasher.token(&(grammar.field_count() as u64).to_le_bytes());
            for id in 0..grammar.node_kind_count() {
                #[allow(
                    clippy::disallowed_methods,
                    reason = "node kind ids are u16 in the grammar ABI, so the conversion cannot fail; an unnamed kind hashes as the empty name"
                )]
                let kind = u16::try_from(id)
                    .ok()
                    .and_then(|id| grammar.node_kind_for_id(id))
                    .unwrap_or("");
                hasher.token(kind.as_bytes());
            }
        }
        hasher.finish().unwrap_or(0)
    })
}

#[allow(
    unused_mut,
    clippy::vec_init_then_push,
    reason = "each grammar is behind its own feature"
)]
fn grammars(language: Language) -> Vec<tree_sitter::Language> {
    let mut found: Vec<tree_sitter::Language> = Vec::new();
    // The JavaScript and TypeScript extractors share their grammars (JSX in
    // `.tsx`, plain script in `.ts`), so either is outdated when one is.
    if matches!(
        language,
        Language::JavaScript | Language::TypeScript | Language::Html | Language::Astro
    ) {
        #[cfg(feature = "tree-sitter-javascript")]
        found.push(tree_sitter::Language::new(tree_sitter_javascript::LANGUAGE));
        #[cfg(feature = "tree-sitter-typescript")]
        {
            found.push(tree_sitter::Language::new(
                tree_sitter_typescript::LANGUAGE_TYPESCRIPT,
            ));
            found.push(tree_sitter::Language::new(
                tree_sitter_typescript::LANGUAGE_TSX,
            ));
        }
        return found;
    }
    match language {
        #[cfg(feature = "tree-sitter-python")]
        Language::Python => found.push(tree_sitter::Language::new(tree_sitter_python::LANGUAGE)),
        #[cfg(feature = "tree-sitter-rust")]
        Language::Rust => found.push(tree_sitter::Language::new(tree_sitter_rust::LANGUAGE)),
        #[cfg(feature = "brokk-tree-sitter-kotlin")]
        Language::Kotlin => found.push(tree_sitter::Language::new(
            brokk_tree_sitter_kotlin::LANGUAGE,
        )),
        #[cfg(feature = "tree-sitter-go")]
        Language::Go => found.push(tree_sitter::Language::new(tree_sitter_go::LANGUAGE)),
        #[cfg(feature = "tree-sitter-java")]
        Language::Java => found.push(tree_sitter::Language::new(tree_sitter_java::LANGUAGE)),
        #[cfg(feature = "tree-sitter-cpp")]
        Language::Cpp => found.push(tree_sitter::Language::new(tree_sitter_cpp::LANGUAGE)),
        #[cfg(feature = "tree-sitter-c")]
        Language::C => found.push(tree_sitter::Language::new(tree_sitter_c::LANGUAGE)),
        #[cfg(feature = "tree-sitter-php")]
        Language::Php => found.push(tree_sitter::Language::new(tree_sitter_php::LANGUAGE_PHP)),
        #[cfg(feature = "tree-sitter-swift")]
        Language::Swift => found.push(tree_sitter::Language::new(tree_sitter_swift::LANGUAGE)),
        #[cfg(feature = "tree-sitter-bash")]
        Language::Shell => found.push(tree_sitter::Language::new(tree_sitter_bash::LANGUAGE)),
        #[cfg(feature = "tree-sitter-yaml")]
        Language::Config | Language::HelmTemplate | Language::HelmValues => {
            found.push(tree_sitter::Language::new(tree_sitter_yaml::LANGUAGE));
        }
        #[cfg(feature = "tree-sitter-hcl")]
        Language::Terraform => found.push(tree_sitter::Language::new(tree_sitter_hcl::LANGUAGE)),
        _ => {}
    }
    found
}
