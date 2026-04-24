//! Minimal ANSI color helpers. Colors are emitted only when stdout is a TTY.

use std::io::IsTerminal;
use std::sync::OnceLock;

pub const RESET: &str = "\x1b[0m";
pub const BOLD: &str = "\x1b[1m";
pub const DIM: &str = "\x1b[2m";
pub const GREEN: &str = "\x1b[32m";
pub const YELLOW: &str = "\x1b[33m";
pub const CYAN: &str = "\x1b[36m";
pub const MAGENTA: &str = "\x1b[35m";
pub const BOLD_RED: &str = "\x1b[1;31m";

pub fn use_color() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| {
        if std::env::var_os("NO_COLOR").is_some() {
            return false;
        }
        std::io::stdout().is_terminal()
    })
}

/// Wrap `s` with `code` ... `RESET` only if color is enabled.
pub fn paint(code: &str, s: &str) -> String {
    if use_color() {
        format!("{code}{s}{RESET}")
    } else {
        s.to_string()
    }
}
