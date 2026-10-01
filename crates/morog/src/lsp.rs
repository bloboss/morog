//! `morog lsp` — the language server over stdio (plan §5, milestone M7).

use std::process::ExitCode;

pub fn run() -> ExitCode {
    match morog_lsp::run_stdio() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("morog lsp: {e}");
            ExitCode::FAILURE
        }
    }
}
