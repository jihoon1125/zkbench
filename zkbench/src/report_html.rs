//! `zkbench report`: read the measurement JSONs in results/ and render a
//! self-contained, dark-theme HTML report.
//!
//! The report is a single file: all measurement data is inlined into the page
//! as JSON, so it opens with no network access beyond the Chart.js CDN script.

use anyhow::{Context, Result, bail};
use chrono::{DateTime, Local};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;
use tokio::process::Command as TokioCommand;

// The record shape mirrors what report::save_json / bench.sh write. Unknown
// fields (e.g. peak_mem_kb) are ignored; everything here is re-serialized into
// the page so the browser-side code has the full dataset.
#[derive(Deserialize, Serialize, Clone)]
struct ToolVersions {
    nargo: String,
    bb: String,
}

#[derive(Deserialize, Serialize, Clone)]
struct MemMb {
    witness: f64,
    prove: f64,
}

#[derive(Deserialize, Serialize, Clone)]
struct Record {
    circuit: String,
    backend: String,
    timestamp: String,
    #[serde(default)]
    repeats: usize,
    #[serde(default)]
    samples_used: usize,
    tool_versions: ToolVersions,
    constraints: u64,
    acir_opcodes: u64,
    witness_time_ms: u64,
    prove_time_ms: u64,
    peak_mem_mb: MemMb,
}

/// Read every `*.json` under `results_dir`, keep the newest measurement per
/// circuit, render the HTML report to `out_file`, and print where it landed.
/// When `open` is set, also open the report in the default browser.
pub async fn generate(results_dir: &Path, out_file: &Path, open: bool) -> Result<()> {
    let mut records = load_latest_per_circuit(results_dir).await?;
    if records.is_empty() {
        bail!(
            "no measurement JSON found in {} (run `zkbench run` or `compare` first)",
            results_dir.display()
        );
    }

    // Charts and tables all present circuits in ascending constraint order.
    records.sort_by_key(|r| r.constraints);

    let data_json = serde_json::to_string(&records).context("failed to serialize records")?;
    let generated_at = Local::now().format("%Y-%m-%d %H:%M:%S %:z").to_string();

    let html = HTML_TEMPLATE
        .replace("__GENERATED_AT__", &generated_at)
        .replace("__DATA__", &data_json);

    if let Some(parent) = out_file.parent() {
        tokio::fs::create_dir_all(parent).await.ok();
    }
    tokio::fs::write(out_file, html)
        .await
        .with_context(|| format!("failed to write {}", out_file.display()))?;

    println!("report saved: {}", out_file.display());

    if open {
        // Best-effort: the report file is already written, so a failure to open
        // it is only a warning, not an error for the whole command.
        match open_in_browser(out_file).await {
            Ok(()) => println!("opening in browser..."),
            Err(e) => eprintln!(
                "warning: could not open the report automatically ({e:#}); open {} manually",
                out_file.display()
            ),
        }
    }

    Ok(())
}

/// Open `file` in the default browser. WSL is handled specially: the Linux
/// path is translated with `wslpath` and handed to the Windows shell, since a
/// Linux opener cannot reach the Windows browser. Native Linux/macOS/Windows go
/// through the `open` crate (xdg-open / open / start).
async fn open_in_browser(file: &Path) -> Result<()> {
    // Absolute path so both wslpath and the OS opener resolve it unambiguously.
    let abs = tokio::fs::canonicalize(file)
        .await
        .unwrap_or_else(|_| file.to_path_buf());

    if is_wsl() {
        return open_on_wsl(&abs).await;
    }

    open::that(&abs).context("failed to open the report with the default handler")?;
    Ok(())
}

/// Detect WSL: the interop env vars are the fast path; `/proc/version` naming
/// is the fallback (it contains "microsoft" under WSL).
fn is_wsl() -> bool {
    if std::env::var_os("WSL_DISTRO_NAME").is_some() || std::env::var_os("WSL_INTEROP").is_some() {
        return true;
    }
    std::fs::read_to_string("/proc/version")
        .map(|v| v.to_lowercase().contains("microsoft"))
        .unwrap_or(false)
}

/// Open a file living in the WSL filesystem in the Windows default browser.
async fn open_on_wsl(abs: &Path) -> Result<()> {
    // Translate /home/... -> \\wsl$\... (a UNC path Windows can open).
    let out = TokioCommand::new("wslpath")
        .arg("-w")
        .arg(abs)
        .output()
        .await
        .context("failed to run wslpath (is this really WSL?)")?;
    if !out.status.success() {
        bail!("wslpath failed: {}", String::from_utf8_lossy(&out.stderr).trim());
    }
    let win_path = String::from_utf8_lossy(&out.stdout).trim().to_string();

    // explorer.exe opens the .html with Windows' default handler (the browser).
    // Note: explorer.exe commonly returns a non-zero exit code even on success,
    // so we only require that the process launched, not that it exited zero.
    TokioCommand::new("explorer.exe")
        .arg(&win_path)
        .status()
        .await
        .context("failed to launch explorer.exe")?;
    Ok(())
}

/// Load all result JSONs, deduplicating to the latest (by timestamp) per circuit.
async fn load_latest_per_circuit(dir: &Path) -> Result<Vec<Record>> {
    let mut read = tokio::fs::read_dir(dir)
        .await
        .with_context(|| format!("cannot read results directory: {}", dir.display()))?;

    // circuit name -> (timestamp millis, record). Higher millis wins.
    let mut latest: HashMap<String, (i64, Record)> = HashMap::new();

    while let Some(entry) = read.next_entry().await? {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }

        let text = match tokio::fs::read_to_string(&path).await {
            Ok(t) => t,
            Err(e) => {
                eprintln!("warning: skipping {} ({e})", path.display());
                continue;
            }
        };
        let record: Record = match serde_json::from_str(&text) {
            Ok(r) => r,
            Err(_) => {
                // Not a benchmark JSON (or an old/foreign shape) — skip quietly.
                eprintln!("warning: skipping unparseable {}", path.display());
                continue;
            }
        };

        let ts = parse_timestamp_millis(&record.timestamp);
        match latest.get(&record.circuit) {
            // Keep the existing one if it is newer or equal.
            Some((cur_ts, _)) if *cur_ts >= ts => {}
            _ => {
                latest.insert(record.circuit.clone(), (ts, record));
            }
        }
    }

    Ok(latest.into_values().map(|(_, record)| record).collect())
}

/// Parse an RFC 3339 timestamp to epoch millis for ordering; unparseable
/// timestamps sort oldest so a valid one always wins.
fn parse_timestamp_millis(s: &str) -> i64 {
    DateTime::parse_from_rfc3339(s)
        .map(|dt| dt.timestamp_millis())
        .unwrap_or(i64::MIN)
}

// The page: dark, monospace, terminal-tool feel. `__DATA__` is replaced with a
// JSON array of records and `__GENERATED_AT__` with the generation time. We use
// string replacement (not format!) so the CSS/JS braces need no escaping.
const HTML_TEMPLATE: &str = r####"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>zkbench report</title>
<script src="https://cdn.jsdelivr.net/npm/chart.js@4.4.4/dist/chart.umd.min.js"></script>
<style>
  :root {
    --bg: #0d1117; --panel: #161b22; --border: #30363d;
    --text: #c9d1d9; --muted: #8b949e; --accent: #58a6ff;
    --witness: #58a6ff; --prove: #f85149; --mem: #bc8cff; --gates: #3fb950;
    --warn: #f0883e;
  }
  * { box-sizing: border-box; }
  body {
    margin: 0; padding: 32px 20px; background: var(--bg); color: var(--text);
    font-family: 'JetBrains Mono','SFMono-Regular',ui-monospace,Consolas,monospace;
    line-height: 1.5;
  }
  .wrap { max-width: 1100px; margin: 0 auto; }
  h1 { margin: 0 0 4px; font-size: 22px; }
  h1 .dim { color: var(--muted); font-weight: normal; }
  .sub { color: var(--muted); font-size: 13px; margin-bottom: 6px; }
  .versions { font-size: 13px; margin-bottom: 4px; }
  .badge {
    display: inline-block; padding: 2px 8px; border-radius: 6px;
    background: var(--panel); border: 1px solid var(--border); font-size: 12px;
  }
  .warn { color: var(--warn); border-color: var(--warn); }
  section { margin-top: 28px; }
  section h2 {
    font-size: 14px; color: var(--muted); text-transform: uppercase;
    letter-spacing: 0.06em; margin: 0 0 10px; font-weight: 600;
  }
  table { width: 100%; border-collapse: collapse; font-size: 13px; }
  th, td { text-align: right; padding: 8px 12px; border-bottom: 1px solid var(--border); }
  th:first-child, td:first-child { text-align: left; }
  th { color: var(--muted); font-weight: 600; }
  tbody tr:hover { background: var(--panel); }
  .grid { display: grid; grid-template-columns: 1fr 1fr; gap: 16px; }
  @media (max-width: 720px) { .grid { grid-template-columns: 1fr; } }
  .card {
    background: var(--panel); border: 1px solid var(--border);
    border-radius: 10px; padding: 14px 16px;
  }
  .card h3 { margin: 0 0 10px; font-size: 13px; font-weight: 600; }
  .chart-box { position: relative; height: 260px; }
  footer { margin-top: 32px; color: var(--muted); font-size: 12px; text-align: center; }
  code { color: var(--accent); }
</style>
</head>
<body>
<div class="wrap">
  <h1>zkbench <span class="dim">report</span></h1>
  <div class="sub">generated __GENERATED_AT__</div>
  <div class="versions"><span id="versions" class="badge"></span> <span id="mixed"></span></div>

  <section>
    <h2>Comparison</h2>
    <table>
      <thead><tr>
        <th>circuit</th><th>constraints</th><th>witness (ms)</th>
        <th>prove (ms)</th><th>prove (MB)</th>
      </tr></thead>
      <tbody id="cmp"></tbody>
    </table>
  </section>

  <section>
    <h2>Charts</h2>
    <div class="grid">
      <div class="card"><h3>prove time (ms)</h3><div class="chart-box"><canvas id="cProveTime"></canvas></div></div>
      <div class="card"><h3>prove memory (MB)</h3><div class="chart-box"><canvas id="cProveMem"></canvas></div></div>
      <div class="card"><h3>constraints vs prove time</h3><div class="chart-box"><canvas id="cScatter"></canvas></div></div>
      <div class="card"><h3>time breakdown (witness + prove)</h3><div class="chart-box"><canvas id="cStacked"></canvas></div></div>
    </div>
  </section>

  <section>
    <h2>Toolchain per circuit</h2>
    <table>
      <thead><tr><th>circuit</th><th>nargo</th><th>bb</th><th>backend</th><th>measured at</th></tr></thead>
      <tbody id="tc"></tbody>
    </table>
  </section>

  <footer>zkbench &middot; self-contained report &middot; data inlined, no external fetch</footer>
</div>

<script>
const DATA = __DATA__;
// Defensive: ensure ascending constraint order for every view.
DATA.sort((a, b) => a.constraints - b.constraints);
const labels = DATA.map(d => d.circuit);

// ---- header: distinct toolchain versions (highlight if mixed) ----
const nargos = [...new Set(DATA.map(d => d.tool_versions.nargo))];
const bbs = [...new Set(DATA.map(d => d.tool_versions.bb))];
document.getElementById('versions').textContent =
  'nargo: ' + nargos.join(', ') + '  ·  bb: ' + bbs.join(', ');
if (nargos.length > 1 || bbs.length > 1) {
  const m = document.getElementById('mixed');
  m.className = 'badge warn';
  m.textContent = '⚠ mixed toolchain versions';
}

// ---- comparison table ----
const cmp = document.getElementById('cmp');
DATA.forEach(d => {
  const tr = document.createElement('tr');
  tr.innerHTML =
    '<td>' + d.circuit + '</td>' +
    '<td>' + d.constraints.toLocaleString() + '</td>' +
    '<td>' + d.witness_time_ms + '</td>' +
    '<td>' + d.prove_time_ms + '</td>' +
    '<td>' + d.peak_mem_mb.prove.toFixed(2) + '</td>';
  cmp.appendChild(tr);
});

// ---- toolchain table ----
const tc = document.getElementById('tc');
DATA.forEach(d => {
  const tr = document.createElement('tr');
  tr.innerHTML =
    '<td>' + d.circuit + '</td>' +
    '<td>' + d.tool_versions.nargo + '</td>' +
    '<td>' + d.tool_versions.bb + '</td>' +
    '<td>' + d.backend + '</td>' +
    '<td>' + d.timestamp + '</td>';
  tc.appendChild(tr);
});

// ---- Chart.js dark defaults ----
const css = getComputedStyle(document.documentElement);
const c = n => css.getPropertyValue(n).trim();
Chart.defaults.color = c('--text');
Chart.defaults.font.family = "'JetBrains Mono','SFMono-Regular',Consolas,monospace";
const gridColor = c('--border');
const axis = (xTitle, yTitle) => ({
  x: { grid: { color: gridColor }, title: { display: !!xTitle, text: xTitle } },
  y: { grid: { color: gridColor }, title: { display: !!yTitle, text: yTitle } },
});

// prove time — horizontal bar
new Chart(document.getElementById('cProveTime'), {
  type: 'bar',
  data: { labels, datasets: [{ data: DATA.map(d => d.prove_time_ms), backgroundColor: c('--witness') }] },
  options: { indexAxis: 'y', responsive: true, maintainAspectRatio: false,
    plugins: { legend: { display: false } }, scales: axis('ms', null) },
});

// prove memory — horizontal bar
new Chart(document.getElementById('cProveMem'), {
  type: 'bar',
  data: { labels, datasets: [{ data: DATA.map(d => d.peak_mem_mb.prove), backgroundColor: c('--mem') }] },
  options: { indexAxis: 'y', responsive: true, maintainAspectRatio: false,
    plugins: { legend: { display: false } }, scales: axis('MB', null) },
});

// constraints vs prove time — scatter (relationship of size to time)
new Chart(document.getElementById('cScatter'), {
  type: 'scatter',
  data: { datasets: [{
    data: DATA.map(d => ({ x: d.constraints, y: d.prove_time_ms, circuit: d.circuit })),
    backgroundColor: c('--gates'), pointRadius: 6, pointHoverRadius: 8,
  }] },
  options: { responsive: true, maintainAspectRatio: false,
    plugins: { legend: { display: false },
      tooltip: { callbacks: { label: ctx =>
        ctx.raw.circuit + ': ' + ctx.raw.x.toLocaleString() + ' gates, ' + ctx.raw.y + ' ms' } } },
    scales: axis('constraints', 'prove time (ms)') },
});

// witness + prove — stacked horizontal bar
new Chart(document.getElementById('cStacked'), {
  type: 'bar',
  data: { labels, datasets: [
    { label: 'witness', data: DATA.map(d => d.witness_time_ms), backgroundColor: c('--witness') },
    { label: 'prove',   data: DATA.map(d => d.prove_time_ms),   backgroundColor: c('--prove') },
  ] },
  options: { indexAxis: 'y', responsive: true, maintainAspectRatio: false,
    plugins: { legend: { position: 'bottom' } },
    scales: { x: { stacked: true, grid: { color: gridColor }, title: { display: true, text: 'ms' } },
              y: { stacked: true, grid: { color: gridColor } } } },
});
</script>
</body>
</html>
"####;
