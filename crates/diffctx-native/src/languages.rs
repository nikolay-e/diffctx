use once_cell::sync::Lazy;
use rustc_hash::FxHashMap;
use std::path::Path;

pub static EXTENSION_TO_LANGUAGE: Lazy<FxHashMap<&'static str, &'static str>> = Lazy::new(|| {
    let entries: &[(&str, &str)] = &[
        (".py", "python"),
        (".pyw", "python"),
        (".pyi", "python"),
        (".js", "javascript"),
        (".mjs", "javascript"),
        (".cjs", "javascript"),
        (".jsx", "jsx"),
        (".ts", "typescript"),
        (".tsx", "tsx"),
        (".mts", "typescript"),
        (".cts", "typescript"),
        (".json", "json"),
        (".yaml", "yaml"),
        (".yml", "yaml"),
        (".toml", "toml"),
        (".md", "markdown"),
        (".markdown", "markdown"),
        (".mdx", "markdown"),
        (".html", "html"),
        (".htm", "html"),
        (".css", "css"),
        (".scss", "scss"),
        (".less", "less"),
        (".xml", "xml"),
        (".svg", "xml"),
        (".sh", "bash"),
        (".bash", "bash"),
        (".zsh", "zsh"),
        (".fish", "fish"),
        (".ksh", "bash"),
        (".ps1", "powershell"),
        (".psm1", "powershell"),
        (".psd1", "powershell"),
        (".bat", "batch"),
        (".cmd", "batch"),
        (".c", "c"),
        (".h", "c"),
        (".cpp", "cpp"),
        (".cc", "cpp"),
        (".cxx", "cpp"),
        (".hpp", "cpp"),
        (".hh", "cpp"),
        (".hxx", "cpp"),
        (".cs", "csharp"),
        (".fs", "fsharp"),
        (".fsi", "fsharp"),
        (".fsx", "fsharp"),
        (".java", "java"),
        (".kt", "kotlin"),
        (".kts", "kotlin"),
        (".scala", "scala"),
        (".sc", "scala"),
        (".go", "go"),
        (".rs", "rust"),
        (".rb", "ruby"),
        (".rake", "ruby"),
        (".gemspec", "ruby"),
        (".php", "php"),
        (".swift", "swift"),
        (".m", "objectivec"),
        (".mm", "objectivec"),
        (".r", "r"),
        (".lua", "lua"),
        (".pl", "perl"),
        (".pm", "perl"),
        (".t", "perl"),
        (".ex", "elixir"),
        (".exs", "elixir"),
        (".erl", "erlang"),
        (".hrl", "erlang"),
        (".hs", "haskell"),
        (".lhs", "haskell"),
        (".ml", "ocaml"),
        (".mli", "ocaml"),
        (".clj", "clojure"),
        (".cljs", "clojure"),
        (".cljc", "clojure"),
        (".sql", "sql"),
        (".graphql", "graphql"),
        (".gql", "graphql"),
        (".proto", "protobuf"),
        (".dockerfile", "dockerfile"),
        (".tf", "terraform"),
        (".hcl", "hcl"),
        (".vim", "vim"),
        (".el", "elisp"),
        (".lisp", "lisp"),
        (".scm", "scheme"),
        (".rkt", "racket"),
        (".zig", "zig"),
        (".nim", "nim"),
        (".v", "v"),
        (".sv", "systemverilog"),
        (".vhd", "vhdl"),
        (".vhdl", "vhdl"),
        (".d", "d"),
        (".dart", "dart"),
        (".groovy", "groovy"),
        (".gradle", "groovy"),
        (".jl", "julia"),
        (".ini", "ini"),
        (".cfg", "ini"),
        (".conf", "ini"),
        (".properties", "properties"),
        (".ada", "ada"),
        (".pas", "pascal"),
        (".f90", "fortran"),
        (".f95", "fortran"),
        (".cob", "cobol"),
        (".asm", "asm"),
        (".s", "asm"),
        (".c++", "cpp"),
        (".h++", "cpp"),
        (".ipp", "cpp"),
        (".tpp", "cpp"),
        (".phtml", "php"),
        (".php3", "php"),
        (".php4", "php"),
        (".php5", "php"),
        (".php7", "php"),
        (".phps", "php"),
        (".adoc", "asciidoc"),
        (".tex", "latex"),
        (".latex", "latex"),
        (".rst", "rst"),
        (".txt", "text"),
        (".log", "text"),
        (".diff", "diff"),
        (".patch", "diff"),
        (".vue", "vue"),
        (".svelte", "svelte"),
        (".sty", "latex"),
        (".cls", "latex"),
        (".bst", "latex"),
        (".dtx", "latex"),
        (".bib", "bibtex"),
        (".nix", "nix"),
        (".prisma", "prisma"),
        (".bzl", "bazel"),
        (".j2", "jinja"),
        (".jinja", "jinja"),
        (".jinja2", "jinja"),
        (".cmake", "cmake"),
        (".mk", "make"),
    ];

    let mut map = FxHashMap::with_capacity_and_hasher(entries.len(), Default::default());
    for &(ext, lang) in entries {
        map.insert(ext, lang);
    }
    map
});

pub static FILENAME_TO_LANGUAGE: Lazy<FxHashMap<&'static str, &'static str>> = Lazy::new(|| {
    let entries: &[(&str, &str)] = &[
        ("makefile", "makefile"),
        ("gnumakefile", "makefile"),
        ("dockerfile", "dockerfile"),
        ("containerfile", "dockerfile"),
        ("vagrantfile", "ruby"),
        ("gemfile", "ruby"),
        ("rakefile", "ruby"),
        ("guardfile", "ruby"),
        ("brewfile", "ruby"),
        ("podfile", "ruby"),
        ("cmakelists.txt", "cmake"),
        ("justfile", "just"),
        (".bashrc", "bash"),
        (".bash_profile", "bash"),
        (".bash_aliases", "bash"),
        (".zshrc", "zsh"),
        (".zshenv", "zsh"),
        (".zprofile", "zsh"),
        (".profile", "bash"),
        (".gitconfig", "gitconfig"),
        (".gitattributes", "gitattributes"),
        (".gitignore", "gitignore"),
        (".dockerignore", "gitignore"),
        (".diffctxignore", "gitignore"),
        // Dotfiles have no extension: `Path::new(".env").extension()` is
        // `None`, so these only ever match as whole names.
        (".env", "dotenv"),
        (".editorconfig", "editorconfig"),
        (".npmrc", "ini"),
        (".yarnrc", "yaml"),
        (".prettierrc", "json"),
        (".eslintrc", "json"),
        ("package.json", "json"),
        ("tsconfig.json", "json"),
        ("composer.json", "json"),
        ("cargo.toml", "toml"),
        ("pyproject.toml", "toml"),
        ("go.mod", "gomod"),
        ("go.sum", "gosum"),
        ("requirements.txt", "text"),
        ("pipfile", "toml"),
        ("procfile", "text"),
        ("jenkinsfile", "groovy"),
        ("build.bazel", "bazel"),
        ("workspace.bazel", "bazel"),
        ("flake.lock", "json"),
    ];

    let mut map = FxHashMap::with_capacity_and_hasher(entries.len(), Default::default());
    for &(name, lang) in entries {
        map.insert(name, lang);
    }
    map
});

pub fn get_language_for_file(path: &str) -> Option<&'static str> {
    let p = Path::new(path);

    if let Some(name) = p.file_name() {
        let name = name.to_string_lossy();
        // Bazel spells these in capitals; a lowercase `build` or `workspace`
        // with no extension is a script or a directory marker, not Bazel, so
        // this is the one lookup that keeps its case.
        if name == "BUILD" || name == "WORKSPACE" {
            return Some("bazel");
        }
        let name_lower = name.to_lowercase();
        if let Some(&lang) = FILENAME_TO_LANGUAGE.get(name_lower.as_str()) {
            return Some(lang);
        }
        if name_lower.starts_with(".env.") || name_lower.ends_with(".env") {
            return Some("dotenv");
        }
    }

    if let Some(ext) = p.extension() {
        let ext_lower = format!(".{}", ext.to_string_lossy().to_lowercase());
        if let Some(&lang) = EXTENSION_TO_LANGUAGE.get(ext_lower.as_str()) {
            return Some(lang);
        }
    }

    if let Some(name) = p.file_name() {
        let name_lower = name.to_string_lossy().to_lowercase();
        if name_lower.starts_with("dockerfile") {
            return Some("dockerfile");
        }
    }

    None
}
/// The language a file's content says it is, where that differs from what its
/// name says: `.h` is C or C++, `.ts` is TypeScript or Qt Linguist XML, `.m`
/// is Objective-C or MATLAB, and an extension-less script names its
/// interpreter on line one. `None` means the name-based answer stands.
pub fn sniff_language(path: &str, content: &str) -> Option<&'static str> {
    let ext = Path::new(path)
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase());
    match ext.as_deref() {
        Some("h") => CPP_MARKER.is_match(head(content, 4096)).then_some("cpp"),
        Some("ts") => looks_like_xml(content).then_some("xml"),
        Some("m") => (!OBJC_MARKER.is_match(head(content, 4096))
            && MATLAB_MARKER.is_match(head(content, 4096)))
        .then_some("matlab"),
        None if get_language_for_file(path).is_none() => shebang_language(content),
        _ => None,
    }
}

fn head(content: &str, max: usize) -> &str {
    let mut end = content.len().min(max);
    while !content.is_char_boundary(end) {
        end -= 1;
    }
    &content[..end]
}

static CPP_MARKER: Lazy<regex::Regex> = Lazy::new(|| {
    regex::Regex::new(
        r"(?m)^\s*(?:class\s+\w+\s*[:{]|namespace\s+\w+|template\s*<|using\s+namespace\b|(?:public|private|protected)\s*:)",
    )
    .unwrap()
});
static OBJC_MARKER: Lazy<regex::Regex> = Lazy::new(|| {
    regex::Regex::new(
        r"(?m)^\s*(?:#import\b|#include\b|@interface\b|@implementation\b|@protocol\b)",
    )
    .unwrap()
});
static MATLAB_MARKER: Lazy<regex::Regex> =
    Lazy::new(|| regex::Regex::new(r"(?m)^\s*(?:function\b|%|classdef\b)").unwrap());

fn looks_like_xml(content: &str) -> bool {
    let start = content.trim_start_matches('\u{feff}').trim_start();
    start.starts_with("<?xml") || start.starts_with("<!DOCTYPE TS") || start.starts_with("<TS")
}

fn shebang_language(content: &str) -> Option<&'static str> {
    let line = head(content, 128).lines().next()?.strip_prefix("#!")?;
    let mut words = line.split_whitespace();
    let mut program = words.next()?.rsplit('/').next()?;
    if program == "env" {
        program = words.find(|w| !w.starts_with('-'))?;
    }
    let program = program.trim_end_matches(|c: char| c.is_ascii_digit() || c == '.');
    match program {
        "sh" | "bash" | "dash" | "ksh" => Some("bash"),
        "zsh" => Some("bash"),
        "python" => Some("python"),
        "ruby" => Some("ruby"),
        "perl" => Some("perl"),
        "node" => Some("javascript"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_extension_resolves_to_its_language() {
        assert_eq!(get_language_for_file("foo.py"), Some("python"));
        assert_eq!(get_language_for_file("src/lib.rs"), Some("rust"));
    }

    #[test]
    fn dotfiles_resolve_by_name_and_bazel_keeps_its_case() {
        assert_eq!(get_language_for_file("app/.env"), Some("dotenv"));
        assert_eq!(get_language_for_file(".editorconfig"), Some("editorconfig"));
        assert_eq!(get_language_for_file("pkg/BUILD"), Some("bazel"));
        assert_eq!(get_language_for_file("WORKSPACE"), Some("bazel"));
        assert_eq!(get_language_for_file("scripts/build"), None);
        assert_eq!(get_language_for_file("workspace"), None);
    }

    #[test]
    fn extension_matching_is_case_insensitive() {
        assert_eq!(get_language_for_file("FOO.PY"), Some("python"));
        assert_eq!(get_language_for_file("Main.RS"), Some("rust"));
    }

    #[test]
    fn filename_map_prefix_rule_catches_dockerfile_variants() {
        assert_eq!(get_language_for_file("Dockerfile"), Some("dockerfile"));
        assert_eq!(get_language_for_file("Dockerfile.prod"), Some("dockerfile"));
        assert_eq!(get_language_for_file("dockerfile.dev"), Some("dockerfile"));
    }

    #[test]
    fn exact_filename_map_hit_takes_priority_over_extension() {
        assert_eq!(get_language_for_file("CMakeLists.txt"), Some("cmake"));
        assert_eq!(get_language_for_file("Makefile"), Some("makefile"));
    }

    #[test]
    fn unknown_extension_resolves_to_none() {
        assert_eq!(get_language_for_file("foo.xyz123notreal"), None);
        assert_eq!(get_language_for_file("no_extension_at_all"), None);
    }

    #[test]
    fn mdx_matches_markdown_extensions_consistently() {
        assert_eq!(get_language_for_file("component.mdx"), Some("markdown"));
    }

    #[test]
    fn cmake_and_make_extensions_are_discoverable() {
        assert_eq!(get_language_for_file("toolchain.cmake"), Some("cmake"));
        assert_eq!(get_language_for_file("rules.mk"), Some("make"));
    }

    #[test]
    fn content_overrides_an_ambiguous_name() {
        assert_eq!(
            sniff_language(
                "geo.h",
                "namespace geo {\nclass Circle : public Shape {};\n}\n"
            ),
            Some("cpp")
        );
        assert_eq!(sniff_language("geo.h", "int area(int r);\n"), None);
        assert_eq!(
            sniff_language(
                "app_de.ts",
                "<?xml version=\"1.0\"?>\n<!DOCTYPE TS>\n<TS/>\n"
            ),
            Some("xml")
        );
        assert_eq!(sniff_language("app.ts", "export const x = 1;\n"), None);
        assert_eq!(
            sniff_language("solve.m", "function y = solve(x)\n% comment\ny = x;\nend\n"),
            Some("matlab")
        );
        assert_eq!(
            sniff_language("View.m", "#import \"View.h\"\n@implementation View\n@end\n"),
            None
        );
        assert_eq!(
            sniff_language("bin/deploy", "#!/usr/bin/env bash\nset -e\n"),
            Some("bash")
        );
        assert_eq!(
            sniff_language("bin/tool", "#!/usr/bin/python3.12\nprint(1)\n"),
            Some("python")
        );
        assert_eq!(sniff_language("bin/notes", "plain text\n"), None);
        assert_eq!(sniff_language("Makefile", "#!/bin/sh\n"), None);
    }

    #[test]
    fn env_variants_and_perl_tests_resolve_by_name() {
        assert_eq!(get_language_for_file("config/cache.env"), Some("dotenv"));
        assert_eq!(get_language_for_file(".env.production"), Some("dotenv"));
        assert_eq!(get_language_for_file("t/basic.t"), Some("perl"));
    }

    #[test]
    fn extension_table_row_count_is_pinned() {
        assert_eq!(
            EXTENSION_TO_LANGUAGE.len(),
            144,
            "a row was added or removed from EXTENSION_TO_LANGUAGE; update this count \
             deliberately and re-check get_language_for_file coverage"
        );
    }

    #[test]
    fn filename_table_row_count_is_pinned() {
        assert_eq!(
            FILENAME_TO_LANGUAGE.len(),
            44,
            "a row was added or removed from FILENAME_TO_LANGUAGE; update this count \
             deliberately and re-check get_language_for_file coverage"
        );
    }
}
