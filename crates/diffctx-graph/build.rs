//! The fingerprint of the code that makes tokens: the token cache keys its
//! entries on `token_corpus.rs` and on the sources of every workspace path
//! dependency (read from the manifests, so a new leaf crate joins by being
//! depended on). A hand-bumped version is forgotten by the change that needed
//! it; a digest of the code cannot be. The facts cache keys on diffctx-facts'
//! own build script.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};

const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

fn mix(state: &mut u64, bytes: &[u8]) {
    for byte in bytes {
        *state ^= u64::from(*byte);
        *state = state.wrapping_mul(FNV_PRIME);
    }
}

fn sources(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(read) = fs::read_dir(dir) else {
        return;
    };
    for entry in read.flatten() {
        let path = entry.path();
        if path.is_dir() {
            sources(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs" || e == "txt") {
            out.push(path);
        }
    }
}

/// The `src` directories, relative to `root`, of the dependencies that live in
/// this workspace: a dependency with its own `path`, or `workspace = true`
/// naming a `[workspace.dependencies]` entry that has one.
fn path_dependency_sources(root: &Path) -> Vec<String> {
    let read = |manifest: &Path| -> toml::Table {
        let text = fs::read_to_string(manifest).expect("read a manifest");
        println!("cargo:rerun-if-changed={}", manifest.display());
        text.parse().expect("parse a manifest")
    };
    let own = read(&root.join("Cargo.toml"));
    let shared = root.ancestors().enumerate().skip(1).find_map(|(up, dir)| {
        let manifest = dir.join("Cargo.toml");
        let table = read(&manifest.is_file().then_some(manifest)?);
        let deps = table
            .get("workspace")?
            .get("dependencies")?
            .as_table()?
            .clone();
        Some(("../".repeat(up), deps))
    });
    let mut dirs = Vec::new();
    for section in ["dependencies", "build-dependencies"] {
        let Some(deps) = own.get(section).and_then(toml::Value::as_table) else {
            continue;
        };
        for (name, dep) in deps {
            let own_path = dep.get("path").and_then(toml::Value::as_str);
            let inherits = dep.get("workspace").and_then(toml::Value::as_bool) == Some(true);
            match (own_path, &shared) {
                (Some(path), _) => dirs.push(format!("{path}/src")),
                (None, Some((base, table))) if inherits => {
                    let path = table
                        .get(name)
                        .and_then(|entry| entry.get("path"))
                        .and_then(toml::Value::as_str);
                    dirs.extend(path.map(|path| format!("{base}{path}/src")));
                }
                _ => {}
            }
        }
    }
    dirs.sort();
    dirs.dedup();
    dirs
}

/// The digest of the files (sorted by path, each with its path) and of the lock
/// file when there is one.
fn digest(root: &Path, files: &[&str], dirs: &[&str], lock: Option<&Path>) -> u64 {
    let mut paths: Vec<PathBuf> = files.iter().map(|f| root.join(f)).collect();
    for dir in dirs {
        sources(&root.join(dir), &mut paths);
    }
    paths.sort();
    let mut state = FNV_OFFSET;
    for path in &paths {
        let name = path.strip_prefix(root).unwrap_or(path);
        mix(
            &mut state,
            name.to_string_lossy().replace('\\', "/").as_bytes(),
        );
        mix(&mut state, &fs::read(path).unwrap_or_default());
        println!("cargo:rerun-if-changed={}", path.display());
    }
    if let Some(lock) = lock {
        mix(&mut state, &fs::read(lock).unwrap_or_default());
        println!("cargo:rerun-if-changed={}", lock.display());
    }
    state
}

fn main() {
    let root = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("a manifest dir"));
    let out = PathBuf::from(env::var_os("OUT_DIR").expect("an out dir"));
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=src");
    let dependencies = path_dependency_sources(&root);
    let dirs = |own: &[&str]| -> Vec<String> {
        own.iter()
            .map(|dir| (*dir).to_owned())
            .chain(dependencies.iter().cloned())
            .collect()
    };
    let token_dirs = dirs(&[]);
    let tokens = digest(
        &root,
        &["src/token_corpus.rs"],
        &token_dirs.iter().map(String::as_str).collect::<Vec<_>>(),
        None,
    );
    fs::write(
        out.join("tokens_fingerprint.in"),
        format!("{tokens:#018x}_u64"),
    )
    .expect("write the tokens fingerprint");
}
