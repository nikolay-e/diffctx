//! The binary the suites drive, as `DIFFCTX_NATIVE_BIN` names it or cargo
//! built it, with each run's profile line named after the test that ran it.

use std::process::Command;

pub fn diffctx() -> Command {
    let mut cmd = Command::new(
        std::env::var("DIFFCTX_NATIVE_BIN")
            .unwrap_or_else(|_| env!("CARGO_BIN_EXE_diffctx").to_string()),
    );
    let suite = env!("CARGO_CRATE_NAME");
    let tag = match std::thread::current().name() {
        Some(test) if test != "main" => format!("{suite}::{test}"),
        _ => suite.to_string(),
    };
    cmd.env(_diffctx::profile::TAG_ENV, tag);
    cmd
}
