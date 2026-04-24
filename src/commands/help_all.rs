use clap::CommandFactory;

use crate::cli::Cli;

/// Print top-level long help followed by each subcommand's long help,
/// separated by `=== cch help <cmd> ===` banners.
pub fn run() -> anyhow::Result<()> {
    let mut cmd = Cli::command();
    cmd.build();

    cmd.print_long_help()?;
    println!();

    let names: Vec<String> = cmd
        .get_subcommands()
        .filter(|s| !s.is_hide_set())
        .map(|s| s.get_name().to_string())
        .filter(|n| n != "help" && n != "help-all")
        .collect();

    let bin = cmd.get_name().to_string();
    for name in names {
        println!();
        println!("=== {bin} help {name} ===");
        if let Some(sub) = cmd.find_subcommand_mut(&name) {
            sub.print_long_help()?;
            println!();
        }
    }

    Ok(())
}
