//! Integration test: `morog completions` emits a usable script.
//!
//! Only presence markers are asserted — the binary name and a known
//! subcommand — not the whole script, since clap_complete's output shifts
//! between versions.

use std::process::{Command, Output};

fn run_completions(shell: &str) -> Output {
    Command::new(env!("CARGO_BIN_EXE_morog"))
        .args(["completions", shell])
        .output()
        .expect("run morog completions")
}

#[test]
fn completions_zsh_emits_script() {
    let out = run_completions("zsh");
    assert!(out.status.success(), "exit: {:?}", out.status);

    let script = String::from_utf8(out.stdout).unwrap();
    assert!(script.contains("#compdef morog"), "missing #compdef header");
    assert!(script.contains("morog"), "missing binary name");
    assert!(script.contains("research"), "missing known subcommand");
}

#[test]
fn completions_rejects_unknown_shell() {
    let out = run_completions("notashell");
    assert!(!out.status.success());
}
