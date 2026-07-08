//! Command-line surface for zkbench (clap derive).

use clap::{Parser, Subcommand};
use std::path::PathBuf;

/// zkbench — a performance profiler for Noir ZK circuits (nargo + Barretenberg).
#[derive(Parser)]
#[command(name = "zkbench", version, about)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand)]
pub enum Command {
    /// Measure proof generation for a single circuit and print a report.
    Run {
        /// Path to the circuit directory (the folder that contains Nargo.toml).
        circuit_dir: PathBuf,

        /// How many times to repeat each measurement. The first run is
        /// discarded (cold CRS download / cold cache), and the rest are
        /// reduced to their median. Minimum 1.
        #[arg(long, default_value_t = 3)]
        repeats: usize,
    },

    /// Measure two or more circuits and print them side by side in one table.
    Compare {
        /// Circuit directories to measure and compare (two or more).
        #[arg(required = true, num_args = 2..)]
        circuit_dirs: Vec<PathBuf>,

        /// Same as `run --repeats`: applied to every circuit. Minimum 1.
        #[arg(long, default_value_t = 3)]
        repeats: usize,
    },

    /// Render the JSON results in results/ into a self-contained HTML report.
    Report {
        /// Directory holding the measurement JSON files.
        #[arg(long, default_value = "results")]
        results_dir: PathBuf,

        /// Output HTML file path.
        #[arg(long, default_value = "results/report.html")]
        out: PathBuf,

        /// After generating, open the report in the default browser.
        #[arg(long)]
        open: bool,
    },
}
