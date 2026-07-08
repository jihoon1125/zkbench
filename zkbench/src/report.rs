//! Human-facing output (comfy-table + owo-colors) and JSON persistence.

use anyhow::{Context, Result};
use chrono::Local;
use comfy_table::{Attribute, Cell, Color, ContentArrangement, Table, presets};
use owo_colors::OwoColorize;
use serde::Serialize;
use std::path::{Path, PathBuf};

use crate::pipeline::Outcome;

/// KB -> MB with two-decimal rounding (for display and the JSON `_mb` fields).
fn kb_to_mb(kb: u64) -> f64 {
    (kb as f64 / 1024.0 * 100.0).round() / 100.0
}

/// Print the full report: header, facts, per-stage table, and one interpretation line.
pub fn print(o: &Outcome) {
    println!();
    println!("{}", format!("● {}", o.circuit).bold());
    println!(
        "  backend {}   nargo {}   bb {}",
        o.backend.dimmed(),
        o.nargo_version.dimmed(),
        o.bb_version.dimmed()
    );
    println!(
        "  constraints {}   acir_opcodes {}   (median of {} run(s), {} repeat(s))",
        o.constraints.to_string().bold(),
        o.acir_opcodes.to_string().bold(),
        o.samples_used,
        o.repeats,
    );
    println!();

    // Per-stage measurement table. The prove row is colored by how heavy it is
    // relative to witness, so the expensive stage stands out at a glance.
    let ratio = heaviness_ratio(o);
    let prove_color = if ratio >= 2.0 {
        Color::Red
    } else if ratio >= 1.0 {
        Color::Yellow
    } else {
        Color::Green
    };

    let mut table = Table::new();
    table
        .load_preset(presets::UTF8_FULL)
        .set_content_arrangement(ContentArrangement::Dynamic)
        .set_header(vec![
            Cell::new("stage").add_attribute(Attribute::Bold),
            Cell::new("wall-clock (ms)").add_attribute(Attribute::Bold),
            Cell::new("peak mem (MB)").add_attribute(Attribute::Bold),
        ]);

    table.add_row(vec![
        Cell::new("witness"),
        Cell::new(o.witness.time_ms),
        Cell::new(format!("{:.2}", kb_to_mb(o.witness.peak_kb))),
    ]);
    table.add_row(vec![
        Cell::new("prove").fg(prove_color),
        Cell::new(o.prove.time_ms).fg(prove_color),
        Cell::new(format!("{:.2}", kb_to_mb(o.prove.peak_kb))).fg(prove_color),
    ]);

    println!("{table}");

    // One-line interpretation, colored to match the prove row.
    let mem_ratio = o.prove.peak_kb as f64 / o.witness.peak_kb.max(1) as f64;
    let line = format!(
        "→ prove is {ratio:.1}x heavier than witness in time ({mem_ratio:.1}x in memory)"
    );
    let colored = if ratio >= 2.0 {
        line.red().to_string()
    } else if ratio >= 1.0 {
        line.yellow().to_string()
    } else {
        line.green().to_string()
    };
    println!("{colored}");

    // A small witness-vs-prove bar chart, so the split is visual as well as
    // tabular even for a single circuit.
    let rows = vec![
        BarRow {
            label: "witness".to_string(),
            value: o.witness.time_ms as f64,
            display: format!("{} ms", o.witness.time_ms),
        },
        BarRow {
            label: "prove".to_string(),
            value: o.prove.time_ms as f64,
            display: format!("{} ms", o.prove.time_ms),
        },
    ];
    print_bar_chart("wall-clock (ms)", &rows);
}

/// Time heaviness of prove relative to witness (guarded against divide-by-zero).
fn heaviness_ratio(o: &Outcome) -> f64 {
    o.prove.time_ms as f64 / o.witness.time_ms.max(1) as f64
}

pub fn print_saved_path(path: &Path) {
    println!("{} {}", "saved:".dimmed(), path.display());
}

// ── Comparison table (`zkbench compare`) ─────────────────────────────────

/// One row of the comparison table: either a measured circuit or one that
/// failed (compile error, missing folder, …) and could not be measured.
pub enum CompareEntry {
    Ok(Outcome),
    Failed { label: String, error: String },
}

/// Print all circuits side by side. Rows are sorted by constraint count
/// ascending; failed circuits sink to the bottom and show "FAILED", with the
/// underlying error detailed beneath the table.
pub fn print_comparison(entries: &[CompareEntry]) {
    // Successful circuits first (by ascending constraints), then failures.
    let mut order: Vec<&CompareEntry> = entries.iter().collect();
    order.sort_by_key(|entry| match entry {
        CompareEntry::Ok(o) => (0u8, o.constraints),
        CompareEntry::Failed { .. } => (1u8, 0),
    });

    let mut table = Table::new();
    table
        .load_preset(presets::UTF8_FULL)
        .set_content_arrangement(ContentArrangement::Dynamic)
        .set_header(vec![
            Cell::new("circuit").add_attribute(Attribute::Bold),
            Cell::new("constraints").add_attribute(Attribute::Bold),
            Cell::new("witness (ms)").add_attribute(Attribute::Bold),
            Cell::new("prove (ms)").add_attribute(Attribute::Bold),
            Cell::new("prove (MB)").add_attribute(Attribute::Bold),
        ]);

    for entry in &order {
        match entry {
            CompareEntry::Ok(o) => table.add_row(vec![
                Cell::new(&o.circuit),
                Cell::new(o.constraints),
                Cell::new(o.witness.time_ms),
                Cell::new(o.prove.time_ms),
                Cell::new(format!("{:.2}", kb_to_mb(o.prove.peak_kb))),
            ]),
            CompareEntry::Failed { label, .. } => table.add_row(vec![
                Cell::new(label).fg(Color::Red),
                Cell::new("FAILED").fg(Color::Red),
                Cell::new("FAILED").fg(Color::Red),
                Cell::new("FAILED").fg(Color::Red),
                Cell::new("FAILED").fg(Color::Red),
            ]),
        };
    }

    println!();
    println!("{}", "● circuit comparison".bold());
    println!("{table}");

    // ASCII bar charts below the table (successful circuits only, same order as
    // the table). The numeric table stays authoritative; the bars just make the
    // relative magnitudes easy to eyeball.
    let measured: Vec<&Outcome> = order
        .iter()
        .filter_map(|entry| match entry {
            CompareEntry::Ok(o) => Some(o),
            CompareEntry::Failed { .. } => None,
        })
        .collect();

    if !measured.is_empty() {
        let time_rows: Vec<BarRow> = measured
            .iter()
            .map(|o| BarRow {
                label: o.circuit.clone(),
                value: o.prove.time_ms as f64,
                display: format!("{} ms", o.prove.time_ms),
            })
            .collect();
        print_bar_chart("prove time (ms)", &time_rows);

        let mem_rows: Vec<BarRow> = measured
            .iter()
            .map(|o| {
                let mb = kb_to_mb(o.prove.peak_kb);
                BarRow {
                    label: o.circuit.clone(),
                    value: mb,
                    display: format!("{mb:.2} MB"),
                }
            })
            .collect();
        print_bar_chart("prove memory (MB)", &mem_rows);

        // Stacked witness+prove time, so the split (not just the totals) is visible.
        let breakdown_rows: Vec<(String, u64, u64)> = measured
            .iter()
            .map(|o| (o.circuit.clone(), o.witness.time_ms, o.prove.time_ms))
            .collect();
        print_time_breakdown(&breakdown_rows);
    }

    // Detail each failure below the charts so the user can act on it.
    for entry in entries {
        if let CompareEntry::Failed { label, error } = entry {
            println!("{} {}: {}", "FAILED".red().bold(), label, error);
        }
    }
}

// ── ASCII bar chart (hand-drawn, no chart dependency) ────────────────────

/// One horizontal bar: a label, the numeric value used for scaling, and the
/// pre-formatted value string shown at the end of the bar.
struct BarRow {
    label: String,
    value: f64,
    display: String,
}

/// Draw a horizontal bar chart. The largest value fills [`BAR_WIDTH`] blocks;
/// the rest scale proportionally. Bars are colored by magnitude (heaviest red,
/// lightest green), so the expensive circuit pops out at a glance.
fn print_bar_chart(title: &str, rows: &[BarRow]) {
    /// Full-bar width in block characters. Fixed (fits comfortably in ~80 cols
    /// alongside the label and value), so we do not query the terminal size.
    const BAR_WIDTH: usize = 24;

    if rows.is_empty() {
        return;
    }

    println!();
    println!("{}", title.bold());

    // Column widths so labels (and hence the bars) line up.
    let label_w = rows.iter().map(|r| r.label.chars().count()).max().unwrap_or(0);
    let max_val = rows.iter().map(|r| r.value).fold(0.0_f64, f64::max);

    for row in rows {
        // Fraction of the largest value; guard against an all-zero chart.
        let ratio = if max_val > 0.0 { row.value / max_val } else { 0.0 };
        let filled = ((ratio * BAR_WIDTH as f64).round() as usize).min(BAR_WIDTH);
        let bar = format!("{}{}", "█".repeat(filled), "░".repeat(BAR_WIDTH - filled));

        // Heaviest red -> lightest green, with amber in between.
        let colored = if ratio >= 0.66 {
            bar.red().to_string()
        } else if ratio >= 0.33 {
            bar.yellow().to_string()
        } else {
            bar.green().to_string()
        };

        println!("  {:<label_w$}  {}  {}", row.label, colored, row.display);
    }
}

/// Draw a stacked witness+prove time bar per circuit: a blue witness segment
/// joined to a red prove segment. The whole bar's length is proportional to the
/// sum (witness + prove), so the circuit with the largest total fills the full
/// width; within each bar the two colors show how the time splits.
///
/// `rows` is `(circuit, witness_ms, prove_ms)`.
fn print_time_breakdown(rows: &[(String, u64, u64)]) {
    const BAR_WIDTH: usize = 24;

    if rows.is_empty() {
        return;
    }

    println!();
    println!("{}", "time breakdown (witness + prove)".bold());
    // Legend: which color is which.
    println!("  {} witness   {} prove", "██".blue(), "██".red());

    let label_w = rows.iter().map(|(l, _, _)| l.chars().count()).max().unwrap_or(0);
    let max_total = rows.iter().map(|(_, w, p)| w + p).max().unwrap_or(0);

    for (label, witness_ms, prove_ms) in rows {
        let total = witness_ms + prove_ms;

        // Whole bar length scaled to the largest total across circuits.
        let bar_len = if max_total > 0 {
            (((total as f64) / max_total as f64) * BAR_WIDTH as f64).round() as usize
        } else {
            0
        };
        let bar_len = bar_len.min(BAR_WIDTH);

        // Split that length between witness and prove by their share of total.
        let witness_len = if total > 0 {
            (((*witness_ms as f64) / total as f64) * bar_len as f64).round() as usize
        } else {
            0
        };
        let witness_len = witness_len.min(bar_len);
        let prove_len = bar_len - witness_len;

        let witness_seg = "█".repeat(witness_len).blue().to_string();
        let prove_seg = "█".repeat(prove_len).red().to_string();
        // Pad the unused width with spaces so the trailing W/P labels line up.
        let pad = " ".repeat(BAR_WIDTH - bar_len);

        println!(
            "  {:<label_w$}  {}{}{}  W{}ms P{}ms",
            label, witness_seg, prove_seg, pad, witness_ms, prove_ms,
        );
    }
}

// ── JSON persistence ─────────────────────────────────────────────────────
// Shape kept consistent with bench.sh output so any downstream viz can read
// both. Raw KB values are retained alongside the rounded MB values.

#[derive(Serialize)]
struct ToolVersions {
    nargo: String,
    bb: String,
}

#[derive(Serialize)]
struct MemMb {
    witness: f64,
    prove: f64,
}

#[derive(Serialize)]
struct MemKb {
    witness: u64,
    prove: u64,
}

#[derive(Serialize)]
struct BenchResult {
    circuit: String,
    backend: String,
    timestamp: String,
    repeats: usize,
    samples_used: usize,
    tool_versions: ToolVersions,
    constraints: u64,
    acir_opcodes: u64,
    witness_time_ms: u64,
    prove_time_ms: u64,
    peak_mem_mb: MemMb,
    peak_mem_kb: MemKb,
}

/// Write the result to `results/<circuit>_<timestamp>.json` (relative to the
/// current working directory) and return the path.
pub async fn save_json(o: &Outcome) -> Result<PathBuf> {
    let now = Local::now();

    let result = BenchResult {
        circuit: o.circuit.clone(),
        backend: o.backend.to_string(),
        timestamp: now.to_rfc3339(),
        repeats: o.repeats,
        samples_used: o.samples_used,
        tool_versions: ToolVersions {
            nargo: o.nargo_version.clone(),
            bb: o.bb_version.clone(),
        },
        constraints: o.constraints,
        acir_opcodes: o.acir_opcodes,
        witness_time_ms: o.witness.time_ms,
        prove_time_ms: o.prove.time_ms,
        peak_mem_mb: MemMb {
            witness: kb_to_mb(o.witness.peak_kb),
            prove: kb_to_mb(o.prove.peak_kb),
        },
        peak_mem_kb: MemKb {
            witness: o.witness.peak_kb,
            prove: o.prove.peak_kb,
        },
    };

    let dir = PathBuf::from("results");
    tokio::fs::create_dir_all(&dir)
        .await
        .context("failed to create results/ directory")?;
    let filename = format!("{}_{}.json", o.circuit, now.format("%Y%m%dT%H%M%S"));
    let path = dir.join(filename);

    let json = serde_json::to_string_pretty(&result).context("failed to serialize result")?;
    tokio::fs::write(&path, json)
        .await
        .with_context(|| format!("failed to write {}", path.display()))?;
    Ok(path)
}
