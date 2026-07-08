//! Orchestrates measurement. `run` measures one circuit; `compare` measures
//! several and tables them side by side. Both share [`benchmark`], so the
//! measurement (and its reproducibility logic) lives in exactly one place.
//!
//! Stages (mirrors bench.sh):
//!   - warm-up compile: NOT measured, keeps compile time out of witness time
//!   - warm-up write_vk: NOT measured, so prove measures pure proving (bb 5.x
//!     requires a verification key; precomputing it keeps vk cost out of prove)
//!   - circuit size: `bb gates` -> constraints + acir_opcodes
//!   - repeated loop: measure witness (nargo execute) + prove (bb prove)
//!
//! Reproducibility: repeat `repeats` times, drop the first (cold) run, and
//! reduce the rest to their median.

use anyhow::{Context, Result, bail};
use indicatif::{ProgressBar, ProgressStyle};
use std::ffi::OsString;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::process::Command;

use crate::circuit::Circuit;
use crate::measure::{Sample, measure, median};
use crate::report;
use crate::tools;

/// The proving scheme we pin for M1. (Backend comparison is a later milestone.)
pub const BACKEND: &str = "ultra_honk";

/// Everything the report needs after a run. Time/memory values here are the
/// medians over the kept samples.
pub struct Outcome {
    pub circuit: String,
    pub backend: &'static str,
    pub constraints: u64,
    pub acir_opcodes: u64,
    pub witness: Sample,
    pub prove: Sample,
    pub repeats: usize,
    pub samples_used: usize,
    pub nargo_version: String,
    pub bb_version: String,
}

/// Entry point for `zkbench run`: measure one circuit and print its report.
pub async fn run(circuit_dir: &Path, repeats: usize) -> Result<()> {
    if repeats == 0 {
        bail!("--repeats must be at least 1");
    }
    let versions = tools::preflight().await?;

    // Absorb session-level cold costs (CRS, cold caches) before measuring.
    warm_up_globally(circuit_dir).await;

    let outcome = benchmark(circuit_dir, repeats, &versions).await?;

    report::print(&outcome);
    let saved = report::save_json(&outcome)
        .await
        .context("failed to write result JSON")?;
    report::print_saved_path(&saved);
    Ok(())
}

/// Entry point for `zkbench compare`: measure several circuits and print them
/// side by side. Each circuit uses the exact same measurement as `run`
/// (via [`benchmark`]), and is still persisted to results/ individually.
pub async fn compare(circuit_dirs: &[PathBuf], repeats: usize) -> Result<()> {
    if repeats == 0 {
        bail!("--repeats must be at least 1");
    }
    let versions = tools::preflight().await?;

    // Absorb session-level cold costs once, using the first circuit, so the
    // first *measured* circuit is not unfairly penalized. (See warm_up_globally.)
    warm_up_globally(&circuit_dirs[0]).await;

    // Measure each circuit independently. A failure (missing folder, compile
    // error, …) is captured per-circuit so the remaining circuits still run.
    let mut entries = Vec::with_capacity(circuit_dirs.len());
    for dir in circuit_dirs {
        match benchmark(dir, repeats, &versions).await {
            Ok(outcome) => {
                // Persist each circuit to results/, same as `run`. A write error
                // here is non-fatal — we still want the comparison table.
                match report::save_json(&outcome).await {
                    Ok(saved) => report::print_saved_path(&saved),
                    Err(e) => eprintln!("warning: failed to write result JSON: {e:#}"),
                }
                entries.push(report::CompareEntry::Ok(outcome));
            }
            Err(e) => entries.push(report::CompareEntry::Failed {
                label: path_label(dir),
                error: format!("{e:#}"),
            }),
        }
    }

    report::print_comparison(&entries);
    Ok(())
}

/// The shared measurement pipeline behind both `run` and `compare`.
///
/// Loads and validates the circuit, runs the unmeasured warm-ups (compile +
/// vk), reads the circuit size, then measures witness + prove `repeats` times.
/// Reproducibility is preserved here: the first (cold) sample is dropped and
/// the rest are reduced to their median. Callers supply the already-captured
/// tool `versions` so we probe the toolchain only once per invocation.
async fn benchmark(
    circuit_dir: &Path,
    repeats: usize,
    versions: &tools::Versions,
) -> Result<Outcome> {
    let circuit = Circuit::load(circuit_dir).await?;
    let name = circuit.name.clone();

    // Stage 0: warm-up compile (discarded — only to produce target/<name>.json
    // so the measured `nargo execute` below does not also pay compile cost).
    with_spinner(&format!("[{name}] compiling (warm-up)..."), compile(&circuit)).await?;

    // Stage 0b: warm-up verification key (discarded). bb 5.x `prove` needs a vk;
    // precomputing it here means the measured prove is pure proving, not proving
    // + vk generation. The vk is structural (like gate count), so this is fair.
    with_spinner(
        &format!("[{name}] writing verification key (warm-up)..."),
        write_vk(&circuit),
    )
    .await?;

    // Stage 1: circuit size. Structural, so measured once.
    let (constraints, acir_opcodes) = with_spinner(
        &format!("[{name}] reading circuit size..."),
        read_gates(&circuit),
    )
    .await?;

    // Stage 2: repeated witness + prove measurements.
    tokio::fs::create_dir_all(&circuit.proof_dir)
        .await
        .context("failed to create proof output directory")?;

    let mut witness_samples = Vec::with_capacity(repeats);
    let mut prove_samples = Vec::with_capacity(repeats);
    for i in 1..=repeats {
        // MEASUREMENT IS DELIBERATELY SERIAL. Even though these are async, we
        // await one process fully before starting the next. Running witness and
        // prove (or several repeats) concurrently would make them contend for
        // CPU and memory bandwidth, corrupting both the wall-clock and peak-RSS
        // numbers — which defeats the entire purpose of the tool.
        let witness = with_spinner(
            &format!("[{name}] measuring witness ({i}/{repeats})..."),
            // nargo execute resolves the package from the cwd.
            measure("nargo", ["execute"], &circuit.dir),
        )
        .await?;
        witness_samples.push(witness);

        let prove = with_spinner(
            &format!("[{name}] measuring prove ({i}/{repeats})..."),
            measure("bb", prove_args(&circuit), &circuit.dir),
        )
        .await?;
        prove_samples.push(prove);
    }

    // Drop the first (cold) sample when we have more than one; the first run
    // pays the CRS download / cold-cache cost we explicitly want to exclude.
    let dropped = if repeats > 1 { 1 } else { 0 };
    let witness = reduce(&witness_samples[dropped..]);
    let prove = reduce(&prove_samples[dropped..]);

    Ok(Outcome {
        circuit: circuit.name.clone(),
        backend: BACKEND,
        constraints,
        acir_opcodes,
        witness,
        prove,
        repeats,
        samples_used: repeats - dropped,
        nargo_version: versions.nargo.clone(),
        bb_version: versions.bb.clone(),
    })
}

/// Human-friendly label for a circuit path (its final component), used in the
/// comparison table when a circuit fails before we can read its package name.
fn path_label(dir: &Path) -> String {
    dir.file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| dir.display().to_string())
}

/// The `bb prove` argument vector for a circuit, using the precomputed vk so
/// the measured (or warm-up) prove is pure proving.
fn prove_args(circuit: &Circuit) -> Vec<OsString> {
    vec![
        "prove".into(),
        "-s".into(),
        BACKEND.into(),
        "-b".into(),
        circuit.bytecode.clone().into_os_string(),
        "-w".into(),
        circuit.witness.clone().into_os_string(),
        "-o".into(),
        circuit.proof_dir.clone().into_os_string(),
        "-k".into(),
        circuit.vk_dir.join("vk").into_os_string(),
    ]
}

/// Global warm-up (NOT measured), run once before any circuit is measured.
///
/// The per-circuit warm-ups + first-repeat drop remove *that circuit's* cold
/// costs, but the very first `bb prove` of the whole session also pays one-time
/// costs — CRS download/caching, cold filesystem caches, first-touch page
/// allocation — that would otherwise land entirely on whichever circuit happens
/// to be measured first, inflating it. Running a full compile → witness → vk →
/// prove pass here (result discarded) absorbs those costs up front, so every
/// measured circuit starts from an equally warm state.
async fn global_warmup(circuit_dir: &Path) -> Result<()> {
    let circuit = Circuit::load(circuit_dir).await?;
    compile(&circuit).await?;
    tokio::fs::create_dir_all(&circuit.proof_dir)
        .await
        .context("failed to create proof output directory")?;
    // Produce a witness and vk, then prove once — every step discarded. `prove`
    // is what triggers the CRS work we are trying to pay for here.
    measure("nargo", ["execute"], &circuit.dir).await?;
    write_vk(&circuit).await?;
    measure("bb", prove_args(&circuit), &circuit.dir).await?;
    Ok(())
}

/// Run [`global_warmup`] with a spinner, best-effort. A warm-up failure only
/// warns — it must never abort the run or mask the real measurement/error,
/// which will surface normally when the circuit itself is measured.
async fn warm_up_globally(circuit_dir: &Path) {
    let spun = with_spinner(
        "Global warm-up (priming CRS / caches)...",
        global_warmup(circuit_dir),
    )
    .await;
    if let Err(e) = spun {
        eprintln!("warning: global warm-up skipped ({e:#}); the first measured circuit may include cold-start cost");
    }
}

/// Reduce a set of samples to per-metric medians.
fn reduce(samples: &[Sample]) -> Sample {
    let times: Vec<u64> = samples.iter().map(|s| s.time_ms).collect();
    let mems: Vec<u64> = samples.iter().map(|s| s.peak_kb).collect();
    Sample {
        time_ms: median(&times),
        peak_kb: median(&mems),
    }
}

/// Warm-up compile (not measured). Surfaces nargo's stderr on failure.
async fn compile(circuit: &Circuit) -> Result<()> {
    let out = Command::new("nargo")
        .arg("compile")
        .current_dir(&circuit.dir)
        .output()
        .await
        .context("failed to run nargo compile")?;
    if !out.status.success() {
        bail!("compile failed:\n{}", String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(())
}

/// Warm-up verification-key generation (not measured). Produces `<vk_dir>/vk`
/// for `bb prove -k` to consume, keeping vk cost out of the prove measurement.
async fn write_vk(circuit: &Circuit) -> Result<()> {
    tokio::fs::create_dir_all(&circuit.vk_dir)
        .await
        .context("failed to create vk output directory")?;
    let out = Command::new("bb")
        .args(["write_vk", "-s", BACKEND, "-b"])
        .arg(&circuit.bytecode)
        .arg("-o")
        .arg(&circuit.vk_dir)
        .output()
        .await
        .context("failed to run bb write_vk")?;
    if !out.status.success() {
        bail!("bb write_vk failed:\n{}", String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(())
}

/// Extract circuit size from `bb gates` JSON:
///   circuit_size = backend (UltraHonk) gate/constraint count
///   acir_opcodes = ACIR-level opcode count
async fn read_gates(circuit: &Circuit) -> Result<(u64, u64)> {
    let out = Command::new("bb")
        .args(["gates", "-s", BACKEND, "-b"])
        .arg(&circuit.bytecode)
        .output()
        .await
        .context("failed to run bb gates")?;
    if !out.status.success() {
        bail!("bb gates failed:\n{}", String::from_utf8_lossy(&out.stderr).trim());
    }

    let json: serde_json::Value =
        serde_json::from_slice(&out.stdout).context("failed to parse bb gates JSON")?;
    let func = &json["functions"][0];
    let constraints = func["circuit_size"]
        .as_u64()
        .context("bb gates output has no circuit_size")?;
    let acir_opcodes = func["acir_opcodes"]
        .as_u64()
        .context("bb gates output has no acir_opcodes")?;
    Ok((constraints, acir_opcodes))
}

/// Run `fut` while showing an indicatif spinner; mark ✓/✗ on completion.
async fn with_spinner<T>(message: &str, fut: impl Future<Output = Result<T>>) -> Result<T> {
    let pb = ProgressBar::new_spinner();
    pb.set_style(
        ProgressStyle::with_template("{spinner:.cyan} {msg}").expect("valid spinner template"),
    );
    pb.set_message(message.to_string());
    pb.enable_steady_tick(Duration::from_millis(80));

    let result = fut.await;
    match &result {
        Ok(_) => pb.finish_with_message(format!("✓ {message}")),
        Err(_) => pb.finish_with_message(format!("✗ {message}")),
    }
    result
}
