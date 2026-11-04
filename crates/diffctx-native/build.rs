//! Fingerprints of the code that decides what an on-disk cache may serve: the
//! facts cache keys its entries on the sources under `src/facts/` and the lock
//! file (the grammars come with it), the token cache on the sources that make
//! tokens. A hand-bumped version is forgotten by the change that needed it; a
//! digest of the code cannot be.

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
    let lock = [root.join("Cargo.lock"), root.join("../../Cargo.lock")]
        .into_iter()
        .find(|p| p.is_file());
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=src/facts");
    println!("cargo:rerun-if-changed=src/config");
    let facts = digest(&root, &[], &["src/facts"], lock.as_deref());
    let tokens = digest(
        &root,
        &[
            "src/token_corpus.rs",
            "src/tokenizer.rs",
            "src/config/tokenization.rs",
            "src/config/bm25.rs",
            "src/stopwords.rs",
        ],
        &["src/stopwords"],
        None,
    );
    fs::write(
        out.join("facts_fingerprint.in"),
        format!("{facts:#018x}_u64"),
    )
    .expect("write the facts fingerprint");
    fs::write(
        out.join("tokens_fingerprint.in"),
        format!("{tokens:#018x}_u64"),
    )
    .expect("write the tokens fingerprint");
}
