mod cli;
mod commands;
mod paths;
mod session;
mod term;
mod timebounds;
mod transcript;

use clap::Parser;

fn main() -> anyhow::Result<()> {
    let args = cli::Cli::parse_from(rewrite_argv(std::env::args_os()));
    cli::dispatch(args)
}

/// `cch session -3` → `cch session -n 3` (head-style numeric shortcut).
/// Rewrites a single `-<digits>` token immediately after the `session` subcommand.
fn rewrite_argv<I>(argv: I) -> Vec<std::ffi::OsString>
where
    I: IntoIterator<Item = std::ffi::OsString>,
{
    let mut out: Vec<std::ffi::OsString> = argv.into_iter().collect();
    let Some(cmd_idx) = out.iter().position(|a| a == "session") else {
        return out;
    };
    let Some(next) = out
        .get(cmd_idx + 1)
        .and_then(|s| s.to_str())
        .map(str::to_owned)
    else {
        return out;
    };
    if let Some(rest) = next.strip_prefix('-') {
        if !rest.is_empty() && rest.chars().all(|c| c.is_ascii_digit()) {
            out[cmd_idx + 1] = std::ffi::OsString::from("-n");
            out.insert(cmd_idx + 2, std::ffi::OsString::from(rest));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;

    fn os(args: &[&str]) -> Vec<OsString> {
        args.iter().map(|s| OsString::from(*s)).collect()
    }

    #[test]
    fn rewrites_numeric_shortcut() {
        let got = rewrite_argv(os(&["cch", "session", "-3"]));
        assert_eq!(got, os(&["cch", "session", "-n", "3"]));
    }

    #[test]
    fn leaves_normal_args_alone() {
        let got = rewrite_argv(os(&["cch", "session", "-n", "3"]));
        assert_eq!(got, os(&["cch", "session", "-n", "3"]));
    }

    #[test]
    fn leaves_other_subcommands_alone() {
        let got = rewrite_argv(os(&["cch", "grep", "foo"]));
        assert_eq!(got, os(&["cch", "grep", "foo"]));
    }

    #[test]
    fn ignores_non_numeric_flag() {
        let got = rewrite_argv(os(&["cch", "session", "--all"]));
        assert_eq!(got, os(&["cch", "session", "--all"]));
    }
}
