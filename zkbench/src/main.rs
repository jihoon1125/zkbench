//! zkbench — a performance profiler for Noir ZK circuits (nargo + Barretenberg).
//!
//! M1: `zkbench run <circuit_dir>` measures proof generation for a single
//! circuit and prints a report. Multi-circuit comparison, visualization, and
//! backend comparison are intentionally out of scope (later milestones).

mod circuit;
mod cli;
mod measure;
mod pipeline;
mod report;
mod report_html;
mod tools;

use clap::Parser;
use cli::{Cli, Command};
use owo_colors::OwoColorize;

// A single-threaded runtime is intentional: this is a measurement tool, so we
// keep our own process lean rather than spinning up worker threads. The tool's
// concurrency (preflight probes) is I/O-bound and runs fine on current_thread.
#[tokio::main(flavor = "current_thread")]
async fn main() {
    if let Err(err) = try_main().await {
        // `{:#}` prints anyhow's full context chain on a single line.
        eprintln!("{} {:#}", "error:".red().bold(), err);
        std::process::exit(1);
    }
}

async fn try_main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Run {
            circuit_dir,
            repeats,
        } => pipeline::run(&circuit_dir, repeats).await,
        Command::Compare {
            circuit_dirs,
            repeats,
        } => pipeline::compare(&circuit_dirs, repeats).await,
        Command::Report {
            results_dir,
            out,
            open,
        } => report_html::generate(&results_dir, &out, open).await,
    }
}
