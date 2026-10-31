//! Harness for `hook_property.rs`: scratch repositories, the grammar of the
//! command lines an agent sends to Bash, the released hook driven Pre -> line
//! -> Post as Claude Code does, and the verdict on what it printed.

#![allow(dead_code)]

pub mod drive;
pub mod judge;
pub mod line;
pub mod strategies;
pub mod world;

use std::path::PathBuf;
use std::process::{Command, Stdio};

/// A bash the lines run under. macOS ships 3.2 at `/bin/bash`; a newer one
/// is run too when the machine has it.
pub struct Shell {
    pub path: PathBuf,
    pub label: String,
    major: String,
}

pub fn shells() -> Vec<Shell> {
    let mut found: Vec<Shell> = Vec::new();
    for candidate in [
        "/bin/bash",
        "/usr/bin/bash",
        "/opt/homebrew/bin/bash",
        "/usr/local/bin/bash",
        "bash",
    ] {
        let Ok(out) = Command::new(candidate)
            .args(["-c", "printf %s \"$BASH_VERSION\""])
            .stdin(Stdio::null())
            .output()
        else {
            continue;
        };
        let version = String::from_utf8_lossy(&out.stdout).into_owned();
        let major = version.split('.').next().unwrap_or_default().to_string();
        if major.is_empty() || found.iter().any(|s| s.major == major) {
            continue;
        }
        found.push(Shell {
            path: PathBuf::from(candidate),
            label: format!("bash {version}"),
            major,
        });
    }
    found
}
