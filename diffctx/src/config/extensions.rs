use once_cell::sync::Lazy;
use rustc_hash::FxHashSet;

fn set_from(exts: &[&'static str]) -> FxHashSet<&'static str> {
    exts.iter().copied().collect()
}

pub static PYTHON_EXTENSIONS: Lazy<FxHashSet<&'static str>> =
    Lazy::new(|| set_from(&[".py", ".pyi", ".pyw"]));

pub static JAVASCRIPT_EXTENSIONS: Lazy<FxHashSet<&'static str>> =
    Lazy::new(|| set_from(&[".js", ".jsx", ".mjs", ".cjs"]));

pub static TYPESCRIPT_EXTENSIONS: Lazy<FxHashSet<&'static str>> =
    Lazy::new(|| set_from(&[".ts", ".tsx", ".mts", ".cts"]));

pub static JS_TS_EXTENSIONS: Lazy<FxHashSet<&'static str>> = Lazy::new(|| {
    JAVASCRIPT_EXTENSIONS
        .iter()
        .chain(TYPESCRIPT_EXTENSIONS.iter())
        .copied()
        .collect()
});

pub static GO_EXTENSIONS: Lazy<FxHashSet<&'static str>> = Lazy::new(|| set_from(&[".go"]));

pub static RUST_EXTENSIONS: Lazy<FxHashSet<&'static str>> = Lazy::new(|| set_from(&[".rs"]));

pub static JAVA_EXTENSIONS: Lazy<FxHashSet<&'static str>> = Lazy::new(|| set_from(&[".java"]));

pub static KOTLIN_EXTENSIONS: Lazy<FxHashSet<&'static str>> =
    Lazy::new(|| set_from(&[".kt", ".kts"]));

pub static SCALA_EXTENSIONS: Lazy<FxHashSet<&'static str>> =
    Lazy::new(|| set_from(&[".scala", ".sc"]));

pub static JVM_EXTENSIONS: Lazy<FxHashSet<&'static str>> = Lazy::new(|| {
    JAVA_EXTENSIONS
        .iter()
        .chain(KOTLIN_EXTENSIONS.iter())
        .chain(SCALA_EXTENSIONS.iter())
        .copied()
        .collect()
});

pub static C_EXTENSIONS: Lazy<FxHashSet<&'static str>> = Lazy::new(|| set_from(&[".c", ".h"]));

pub static CPP_EXTENSIONS: Lazy<FxHashSet<&'static str>> = Lazy::new(|| {
    set_from(&[
        ".cpp", ".hpp", ".cc", ".hh", ".cxx", ".hxx", ".c++", ".h++", ".ipp", ".tpp",
    ])
});

pub static C_FAMILY_EXTENSIONS: Lazy<FxHashSet<&'static str>> = Lazy::new(|| {
    C_EXTENSIONS
        .iter()
        .chain(CPP_EXTENSIONS.iter())
        .copied()
        .chain([".m", ".mm"])
        .collect()
});

pub static CSHARP_EXTENSIONS: Lazy<FxHashSet<&'static str>> = Lazy::new(|| set_from(&[".cs"]));

pub static FSHARP_EXTENSIONS: Lazy<FxHashSet<&'static str>> =
    Lazy::new(|| set_from(&[".fs", ".fsi", ".fsx"]));

pub static DOTNET_EXTENSIONS: Lazy<FxHashSet<&'static str>> = Lazy::new(|| {
    CSHARP_EXTENSIONS
        .iter()
        .chain(FSHARP_EXTENSIONS.iter())
        .copied()
        .collect()
});

pub static RUBY_EXTENSIONS: Lazy<FxHashSet<&'static str>> =
    Lazy::new(|| set_from(&[".rb", ".rake", ".gemspec"]));

pub static PHP_EXTENSIONS: Lazy<FxHashSet<&'static str>> = Lazy::new(|| {
    set_from(&[
        ".php", ".phtml", ".php3", ".php4", ".php5", ".php7", ".phps",
    ])
});

pub static SHELL_EXTENSIONS: Lazy<FxHashSet<&'static str>> = Lazy::new(|| {
    set_from(&[
        ".sh", ".bash", ".zsh", ".ksh", ".fish", ".ps1", ".psm1", ".psd1",
    ])
});

pub static SWIFT_EXTENSIONS: Lazy<FxHashSet<&'static str>> = Lazy::new(|| set_from(&[".swift"]));

pub static OTHER_CODE_EXTENSIONS: Lazy<FxHashSet<&'static str>> = Lazy::new(|| {
    set_from(&[
        ".lua", ".r", ".jl", ".pl", ".pm", ".ex", ".exs", ".erl", ".hs", ".clj", ".lisp", ".ml",
        ".mli", ".nim", ".v", ".zig", ".d", ".ada", ".pas", ".f90", ".f95", ".cob", ".asm", ".s",
        ".vhd", ".sv", ".vue", ".svelte", ".dart",
    ])
});

pub static CODE_EXTENSIONS: Lazy<FxHashSet<&'static str>> = Lazy::new(|| {
    PYTHON_EXTENSIONS
        .iter()
        .chain(JS_TS_EXTENSIONS.iter())
        .chain(GO_EXTENSIONS.iter())
        .chain(RUST_EXTENSIONS.iter())
        .chain(JVM_EXTENSIONS.iter())
        .chain(C_FAMILY_EXTENSIONS.iter())
        .chain(DOTNET_EXTENSIONS.iter())
        .chain(RUBY_EXTENSIONS.iter())
        .chain(PHP_EXTENSIONS.iter())
        .chain(SHELL_EXTENSIONS.iter())
        .chain(SWIFT_EXTENSIONS.iter())
        .chain(OTHER_CODE_EXTENSIONS.iter())
        .copied()
        .collect()
});

pub static CONFIG_EXTENSIONS: Lazy<FxHashSet<&'static str>> = Lazy::new(|| {
    set_from(&[
        ".yaml",
        ".yml",
        ".json",
        ".toml",
        ".ini",
        ".cfg",
        ".conf",
        ".xml",
        ".properties",
    ])
});

pub static DOC_EXTENSIONS: Lazy<FxHashSet<&'static str>> =
    Lazy::new(|| set_from(&[".md", ".rst", ".txt", ".adoc"]));
