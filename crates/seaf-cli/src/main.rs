use clap::{Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(name = "seaf")]
#[command(about = "Self-Evolving Application Framework developer CLI")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Print basic framework information.
    Info,
}

fn main() {
    let cli = Cli::parse();

    match cli.command {
        Command::Info => println!("{}", seaf_core::framework_name()),
    }
}
