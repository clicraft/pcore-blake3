# core-blake3

[![CI](https://github.com/clicraft/core-blake3/actions/workflows/ci.yml/badge.svg)](https://github.com/clicraft/core-blake3/actions/workflows/ci.yml)

Fast BLAKE3 file hashing that pins work to your CPU's cores. On hybrid CPUs
(Intel P/E-core, AMD) it detects performance vs efficiency cores and runs
**one thread per physical core** — which our analysis found is the
throughput sweet spot for BLAKE3: it saturates each core's SIMD units, so a
second hardware thread per core barely helps.

Library + CLI, pure Rust. The hashing itself is the official
[`blake3`](https://crates.io/crates/blake3) crate; this crate adds the
core detection, pinning, and scheduling around it.

## Install

```toml
[dependencies]
core-blake3 = { git = "https://github.com/clicraft/core-blake3", tag = "v0.5.0" }
```

CLI: `cargo install --git https://github.com/clicraft/core-blake3`, or grab
a prebuilt Linux/Windows binary from
[Releases](https://github.com/clicraft/core-blake3/releases).

## Library usage

```rust
use core_blake3::CoreHasher;

let hasher = CoreHasher::new();           // one thread per physical core
let hash = hasher.hash_file("doc.pdf")?;  // one file (tree-parallel over all cores)
let hash = hasher.hash_bytes(b"data");    // in-memory buffer

// A batch: one file per thread, results in input order, one Result per file.
let hashes = hasher.hash_files(&["a.pdf".into(), "b.pdf".into()]);
```

Digests are identical to `blake3::hash` — the scheduling never changes the
result. A single file uses BLAKE3's tree parallelism across every core; a
batch hashes one file per thread (rayon work-steals, so fast cores take
more files and slow ones fewer).

### Modes

Two modes, both spanning every core (P and E on a hybrid CPU):

| constructor | threads | when |
|---|---|---|
| `CoreHasher::new()` | one per **physical core** | **default** — the efficient sweet spot |
| `CoreHasher::all_threads()` | one per **logical CPU** (all SMT threads) | the conventional "use everything" baseline |

One thread per physical core is BLAKE3's throughput sweet spot: it
saturates each core's SIMD units, so the extra SMT thread per core doesn't
help (and slightly hurts once the machine is memory-bandwidth-bound).

Hashing a batch of 64 diverse files on the reference i9-13900HK (6 P-cores
+ 8 E-cores, 20 logical CPUs) bears this out — fewer threads, more speed:

| mode | threads | throughput |
|---|---|---|
| `new()` | 14 | **~14.5 GiB/s** |
| `all_threads()` | 20 | ~13.3 GiB/s |

## CLI

```console
$ core-blake3 --info
Topology: hybrid
Performance cores: [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11]
Efficiency cores: [12, 13, 14, 15, 16, 17, 18, 19]
Physical cores (P+E): 14   Logical CPUs: 20
This run: 14 threads (one per physical core)

$ core-blake3 doc1.pdf doc2.pdf       # b3sum-compatible output
$ core-blake3 --all-threads *.pdf     # use every logical CPU instead
```

## Examples

Self-contained (generate their own data when run without arguments):

| example | shows |
|---|---|
| `detect_topology` | detected cores and the chosen thread layout |
| `hash_file` | single file + throughput, checked against `blake3::hash` |
| `hash_batch` | order-preserving directory batch, per-file error isolation |
| `pcore_vs_ecore` | P-core vs E-core throughput on hybrid CPUs |

Run with `cargo run --release --example <name>`.

## API overview

| item | purpose |
|---|---|
| `topology() -> Topology` | `Hybrid` or `Homogeneous` |
| `performance_cpus()` / `efficiency_cpus()` | logical CPU ids of P- / E-cores |
| `all_physical_cpus()` | one logical CPU per physical core, P and E |
| `all_logical_cpus()` | every logical CPU (all SMT threads) |
| `physical_core_leaders(&[usize])` | collapse SMT siblings in any CPU set |
| `pin_current_thread_to_cpu(usize)` | pin the calling thread to one CPU |
| `CoreHasher::new` / `all_threads` / `with_cpus` | build a hasher |
| `CoreHasher::threads` | thread count of this hasher |
| `CoreHasher::hash_bytes` / `hash_file` / `hash_files` | hash |

## Why one thread per physical core

Three measured effects (all on the reference i9-13900HK) explain why one
thread per physical core beats using every logical thread.

**SMT barely moves BLAKE3.** Simultaneous multithreading (two hardware
threads on one core — Intel Hyper-Threading) speeds a core up only when one
thread leaves execution units idle, stalled on memory or a dependency, so
its sibling can slip work into the gaps. BLAKE3's inner loop is almost pure
256-bit AVX2 (SIMD) work that keeps the core's vector execution ports busy
on its own — there are almost no idle slots for a second thread to fill, so
it mostly just contends for the same vector ports. Across 15 interleaved
trials the second thread per P-core changed throughput by only **+2.5%**
(P-cores alone) to **−2.5%** (whole machine): statistically real, but tiny.

**Memory contention decides the sign.** That small residual flips with
memory pressure. On the P-cores alone, DRAM latency leaves just enough
slack for the sibling thread to hide, so SMT nets +2.5%. With every core
hashing at once, the shared L3 and the memory controller become the limit,
and the extra SMT threads add more traffic than they hide — so SMT nets
−2.5%.

**So more threads choke a busy machine.** Going from 14 threads (one per
physical core) to 20 (adding the SMT siblings) piles work onto cores whose
vector units are already saturated and a memory subsystem that's already
the shared bottleneck — total throughput drops slightly instead of rising.
One thread per physical core drives every real execution unit without the
SMT port contention or the extra memory pressure: the sweet spot.

(The efficiency cores are ~2x slower per core — narrower vector units,
lower clocks — but there are more of them, so putting one thread on each
E-core too adds ~40% aggregate throughput. That's why both modes span all
physical cores, not just the P-cores.)

## How much does this actually matter?

Be clear-eyed about the gains. On the reference machine, each layer of
optimization bought roughly an order of magnitude *less* than the one above:

- **Algorithm choice dominates.** BLAKE3 over hardware-accelerated SHA-256
  is ~1.5–2x — the one lever that actually moves the needle.
- **Thread placement is a refinement.** One thread per physical core beats
  the naive "use every logical thread" by only ~9%.
- **Instruction-level tuning is noise.** The SMT effect is ±2.5% — small
  enough that confirming its sign needed a t-test.

So: **pick the right algorithm; don't agonize over the rest.** If you just
want fast hashing, calling the [`blake3`](https://crates.io/crates/blake3)
crate with its default thread pool already gets you ~90% of what this crate
does, with no dependency and no config.

`core-blake3` earns its keep only in narrow cases — where hashing shares
the machine with other work and you'd rather not saturate every logical
thread (a service that hashes *and* serves, latency-sensitive or shared
boxes, laptops minding thermals). There it delivers the same throughput on
fewer, better-placed threads, leaving the SMT siblings free. Outside that,
reach for `blake3` directly.

This project started as an exercise in taking one question — "which hash is
faster?" — all the way down to instruction-level scheduling. The honest
conclusion is the classic one, now measured rather than assumed: **the
algorithm rules; fine-tuning is a rounding error.**

## Platform support

- **Linux**: verified on real hybrid hardware (Intel i9-13900HK). Detection
  via the kernel's hybrid-core sysfs markers; pinning via
  `sched_setaffinity`.
- **Windows**: detection via `GetSystemCpuSetInformation` (vendor-agnostic —
  Intel and AMD); built and tested in CI on `windows-latest`, but the
  hybrid-topology path hasn't yet run on real hybrid Windows hardware — see
  [PORT_VALIDATION.md](PORT_VALIDATION.md).
- Other platforms: works, treating every CPU as a performance core.

## Validation

The core-detection code is a Rust port of a C reference implementation,
validated against it by differential testing — see
[PORT_VALIDATION.md](PORT_VALIDATION.md). Changes are in
[CHANGELOG.md](CHANGELOG.md).

## License

MIT ([LICENSE-MIT](LICENSE-MIT)) or Apache-2.0 ([LICENSE-APACHE](LICENSE-APACHE)), at your option.
