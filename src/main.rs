use clap::{Parser, Subcommand};
use std::process::ExitCode;

mod commands;
mod config;

#[derive(Parser)]
#[command(name = "cw", about = "Claude Workers - Git clone-based workspace manager")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Create a new worker environment (clone + symlink + setup)
    New {
        /// Worker name
        name: String,
        /// Branch name to create
        branch: String,
    },
    /// List all worker environments
    Ls,
    /// Print the path to a worker environment
    Path {
        /// Worker name
        name: String,
    },
    /// Remove a worker environment
    Rm {
        /// Worker name (or --all)
        name: Option<String>,
        /// Remove all workers
        #[arg(long)]
        all: bool,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();

    let result = match cli.command {
        Commands::New { name, branch } => commands::new_worker(&name, &branch),
        Commands::Ls => commands::list_workers(),
        Commands::Path { name } => commands::worker_path(&name),
        Commands::Rm { name, all } => commands::remove_worker(name.as_deref(), all),
    };

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}
