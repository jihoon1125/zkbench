#!/usr/bin/env bash
#
# bench.sh — profile the proof-generation pipeline of a Noir ZK circuit.
#
# What it measures:
#   1) witness generation (nargo execute) : wall-clock time + peak RSS
#   2) proof   generation (bb prove)      : wall-clock time + peak RSS
#   3) circuit size       (bb gates)      : ACIR opcode count + gate count
#
# Each stage is measured SEPARATELY. The goal is not a summarized total but
# split, raw numbers showing which stage costs how much time / memory.
#
# Usage:
#   scripts/bench.sh [circuit_dir]
#     Defaults to circuits/balance_threshold when omitted.
#     To swap circuits, pass a different circuit directory path.
#
# Output: results/bench_<circuit>_<timestamp>.json (structured JSON)

set -euo pipefail

# ── Paths ────────────────────────────────────────────────────────────────
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"

CIRCUIT_DIR="${1:-$ROOT_DIR/circuits/balance_threshold}"
CIRCUIT_DIR="$(cd "$CIRCUIT_DIR" && pwd)"   # normalize to absolute path
RESULTS_DIR="$ROOT_DIR/results"

# Proving scheme. bb supports several; Noir + UltraHonk is the default combo.
BACKEND_SCHEME="ultra_honk"

# ── Preflight checks ─────────────────────────────────────────────────────
# Verify required tools exist first (fail fast rather than polluting numbers).
for tool in nargo bb jq /usr/bin/time; do
    command -v "$tool" >/dev/null 2>&1 || { echo "missing tool: $tool" >&2; exit 1; }
done

# Read the package name from Nargo.toml. Artifact filenames derive from it:
#   target/<pkg>.json  = ACIR bytecode (bb input)
#   target/<pkg>.gz    = witness       (bb input)
PKG="$(awk -F'"' '/^[[:space:]]*name[[:space:]]*=/{print $2; exit}' "$CIRCUIT_DIR/Nargo.toml")"
[ -n "$PKG" ] || { echo "could not find package name in Nargo.toml" >&2; exit 1; }

BYTECODE="$CIRCUIT_DIR/target/$PKG.json"
WITNESS="$CIRCUIT_DIR/target/$PKG.gz"
PROOF_DIR="$CIRCUIT_DIR/target/proof"
VK_DIR="$CIRCUIT_DIR/target/vk"     # bb write_vk writes a `vk` file in here

mkdir -p "$RESULTS_DIR"

# ── Measurement helper ───────────────────────────────────────────────────
# measure <out_time_ms_var> <out_mem_kb_var> -- <command...>
#
# Runs /usr/bin/time with -o writing to a dedicated file. This keeps time's
# own stats out of the measured command's stdout/stderr, so parsing is safe.
#   %e = elapsed wall-clock time (seconds, 2 decimals)
#   %M = maximum resident set size = peak RSS over process lifetime (KB, ru_maxrss)
# i.e. we capture wall-clock time AND peak physical memory in the SAME run.
_TIME_FILE="$(mktemp)"
trap 'rm -f "$_TIME_FILE"' EXIT

measure() {
    local time_var="$1" mem_var="$2"
    shift 2
    [ "$1" = "--" ] && shift   # readability separator

    /usr/bin/time -f '%e %M' -o "$_TIME_FILE" -- "$@"

    local wall_s peak_kb
    read -r wall_s peak_kb < "$_TIME_FILE"

    # seconds -> milliseconds, rounded to integer ms (source precision is
    # 2-decimal seconds, so ms is the meaningful resolution).
    printf -v "$time_var" '%d' "$(awk -v s="$wall_s" 'BEGIN{printf "%.0f", s*1000}')"
    printf -v "$mem_var"  '%d' "$peak_kb"
}

echo "==> circuit: $PKG  ($CIRCUIT_DIR)"

# ── Stage 0: warm-up compile (NOT measured) ──────────────────────────────
# nargo execute will compile if needed, which would fold compile time into
# the witness measurement. To measure pure witness generation, compile here
# up front so target/<pkg>.json exists. The following execute skips compiling.
echo "==> [warm-up] compile"
( cd "$CIRCUIT_DIR" && nargo compile )

# ── Stage 0b: warm-up verification key (NOT measured) ─────────────────────
# bb 5.x `prove` needs a verification key. We precompute it here (untimed) so
# the measured prove below is pure proving, not proving + vk generation.
# The vk is structural (like the gate count), so precomputing it is fair.
echo "==> [warm-up] verification key (bb write_vk)"
mkdir -p "$VK_DIR"
bb write_vk -s "$BACKEND_SCHEME" -b "$BYTECODE" -o "$VK_DIR" >/dev/null 2>&1

# ── Stage 1: measure witness generation (time + memory) ──────────────────
# nargo execute resolves the package from the current working directory, so
# cd into the circuit dir. Every bb path below is absolute, so cd is harmless.
echo "==> [measure] witness generation (nargo execute)"
cd "$CIRCUIT_DIR"
measure WITNESS_TIME_MS WITNESS_MEM_KB -- nargo execute
echo "    witness: ${WITNESS_TIME_MS} ms, peak ${WITNESS_MEM_KB} KB"

# ── Stage 2: extract circuit size (gate count) ───────────────────────────
# bb gates builds the circuit from bytecode and emits size info as JSON.
#   acir_opcodes : opcode count at the ACIR level
#   circuit_size : backend (UltraHonk) gate/constraint count  <- "constraints"
echo "==> circuit size (bb gates)"
GATES_JSON="$(bb gates -s "$BACKEND_SCHEME" -b "$BYTECODE" 2>/dev/null)"
ACIR_OPCODES="$(echo "$GATES_JSON" | jq '.functions[0].acir_opcodes // 0')"
CIRCUIT_SIZE="$(echo "$GATES_JSON" | jq '.functions[0].circuit_size // 0')"
echo "    acir_opcodes=${ACIR_OPCODES}, circuit_size(gates)=${CIRCUIT_SIZE}"

# ── Stage 3: measure proof generation (time + memory) ────────────────────
# Note: the first ever run fetches the CRS (common reference string) from the
# internet, which can inflate prove time. For a representative number, run
# once (to cache the CRS) and use the second run's value.
# We pass the precomputed vk (-k) so this measures pure proving.
echo "==> [measure] proof generation (bb prove)"
mkdir -p "$PROOF_DIR"
measure PROVE_TIME_MS PROVE_MEM_KB -- \
    bb prove -s "$BACKEND_SCHEME" -b "$BYTECODE" -w "$WITNESS" -o "$PROOF_DIR" -k "$VK_DIR/vk"
echo "    prove:   ${PROVE_TIME_MS} ms, peak ${PROVE_MEM_KB} KB"

# ── Record tool versions (reproducibility) ───────────────────────────────
NARGO_VER="$(nargo --version 2>/dev/null | awk '/nargo version/{print $NF}')"
BB_VER="$(bb --version 2>/dev/null)"

# ── Write result JSON ────────────────────────────────────────────────────
# KB -> MB conversion happens only here; the raw KB values are kept too
# (raw first).
TS="$(date +%Y%m%dT%H%M%S)"
OUT="$RESULTS_DIR/bench_${PKG}_${TS}.json"

jq -n \
  --arg circuit       "$PKG" \
  --arg backend       "$BACKEND_SCHEME" \
  --arg timestamp     "$(date -Iseconds)" \
  --arg nargo_version "$NARGO_VER" \
  --arg bb_version    "$BB_VER" \
  --argjson acir_opcodes      "$ACIR_OPCODES" \
  --argjson constraints       "$CIRCUIT_SIZE" \
  --argjson witness_time_ms   "$WITNESS_TIME_MS" \
  --argjson witness_mem_kb    "$WITNESS_MEM_KB" \
  --argjson prove_time_ms     "$PROVE_TIME_MS" \
  --argjson prove_mem_kb      "$PROVE_MEM_KB" \
  '{
     circuit: $circuit,
     backend: $backend,
     timestamp: $timestamp,
     tool_versions: { nargo: $nargo_version, bb: $bb_version },
     constraints: $constraints,
     acir_opcodes: $acir_opcodes,
     witness_time_ms: $witness_time_ms,
     prove_time_ms:   $prove_time_ms,
     peak_mem_mb: {
       witness: (($witness_mem_kb / 1024) * 100 | round / 100),
       prove:   (($prove_mem_kb   / 1024) * 100 | round / 100)
     },
     peak_mem_kb: {
       witness: $witness_mem_kb,
       prove:   $prove_mem_kb
     }
   }' > "$OUT"

echo "==> saved: $OUT"
echo "----------------------------------------"
cat "$OUT"
