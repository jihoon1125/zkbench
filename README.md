# zkbench

A reproducible proof-generation benchmarker for [Noir](https://noir-lang.org/) ZK circuits.

`zkbench` measures how long it takes — and how much memory it costs — to generate
zero-knowledge proofs for your Noir circuits, and presents the results as clean
tables, terminal bar charts, and an HTML report.

## Screenshots

Terminal comparison:

![terminal output](zkbench/docs/zkbench_cli.png)

HTML report:

![html report](zkbench/docs/zkbench_html.png)


## Why another benchmarker?

Measuring ZK proving performance naively is misleading. The first run of any
circuit pays a one-time cost (CRS fetch, cold caches) that inflates the numbers,
and system noise can produce phantom results that look like real effects but
disappear on re-measurement.

`zkbench` focuses on **honest, reproducible measurement**:

- **Warm-up runs are discarded.** A global warm-up absorbs CRS/cold-start cost
  before any measurement begins, and per-circuit warm-up runs are dropped.
- **Median of repeats.** Each stage is measured multiple times and the median is
  reported, so a single noisy run can't skew the result.
- **witness and prove measured separately.** Proof generation is dominated by
  the `prove` stage; separating it from `witness` shows where the real cost is —
  and how that shifts as circuits grow.

These aren't cosmetic: while building this tool, naive measurement produced a
"staircase" in proving time that looked like a hard performance cliff. It was
contamination. With warm-up + median, the real curve is smooth.

## Install

```bash
cargo install zkbench
```

Requires [`nargo`](https://noir-lang.org/) and
[`bb`](https://github.com/AztecProtocol/aztec-packages/tree/master/barretenberg)
(Barretenberg) on your `PATH`.

Tested with: `nargo 1.0.0-beta.x`, `bb` (UltraHonk backend).

## Usage

Measure a single circuit:

```bash
zkbench run ./path/to/circuit
```

Compare several circuits side by side:

```bash
zkbench compare ./circuit_a ./circuit_b ./circuit_c
```

Generate an HTML report from accumulated results and open it:

```bash
zkbench report --open
```

A "circuit" is any folder containing a `Nargo.toml` (and a `Prover.toml` with
valid inputs). `zkbench` reads the package name, compiles, and measures.

## What it measures

For each circuit:

| Metric        | Meaning                                            |
|---------------|----------------------------------------------------|
| `constraints` | Backend gate count (proving cost driver)           |
| `acir_opcodes`| ACIR-level opcode count (pre-lowering)             |
| `witness`     | Witness generation: wall-clock time + peak memory  |
| `prove`       | Proof generation: wall-clock time + peak memory    |

Results are saved as JSON under `results/` and can be turned into an HTML report.

## Example

```
● circuit comparison
┌───────────────────┬─────────────┬──────────────┬────────────┬────────────┐
│ circuit           ┆ constraints ┆ witness (ms) ┆ prove (ms) ┆ prove (MB) │
├───────────────────┼─────────────┼──────────────┼────────────┼────────────┤
│ pure_compare      ┆ 2812        ┆ 210          ┆ 250        ┆ 36         │
│ zk_voting         ┆ 3739        ┆ 275          ┆ 300        ┆ 39         │
│ hash_light        ┆ 12048       ┆ 275          ┆ 335        ┆ 57         │
│ balance_threshold ┆ 67428       ┆ 390          ┆ 660        ┆ 143        │
└───────────────────┴─────────────┴──────────────┴────────────┴────────────┘
```

Note how `prove` grows faster than `witness` as circuits get larger, and how
peak memory scales even more steeply than time — the practical limit for
constrained environments (e.g. mobile / client-side proving) is often memory,
not time.

## Status

Early. Built as a learning project and a foundation for benchmarking
client-side / constrained-environment proving. Contributions and issues welcome.

## License

Licensed under either of MIT or Apache-2.0 at your option.