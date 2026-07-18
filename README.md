# pcore-blake3

[![CI](https://github.com/clicraft/pcore-blake3/actions/workflows/ci.yml/badge.svg)](https://github.com/clicraft/pcore-blake3/actions/workflows/ci.yml)

BLAKE3 hashing that auto-detects performance ("P") cores on hybrid CPUs
(Intel Alder Lake+ and AMD hybrid parts) and picks a thread split between
BLAKE3's internal tree parallelism and concurrent-file parallelism,
instead of blending P-cores and E-cores into one undifferentiated
"use all cores" pool. Library + CLI, pure Rust, no C toolchain required.

## Why

On a hybrid CPU, "use all logical CPUs" mixes two different core speeds
into one throughput number, and it isn't obvious how to split N available
threads between "one file's internal BLAKE3 tree" and "how many files
hash concurrently" — 1 thread/file and N threads/file are usually both
wrong. Benchmarked on a real 13th-gen Intel i9 (6 P-cores / 12 threads, 8
E-cores) across P-core counts from 2 to 6:

| P-cores | threads | best split |
|---|---|---|
| 2 | 4 | 4 threads/file x 1 file (no split) |
| 3 | 6 | 3 threads/file x 2 files |
| 4 | 8 | ~4 threads/file x 2 files |
| 5 | 10 | ~5 threads/file x 2 files |
| 6 | 12 | ~3-4 threads/file x 3-4 files |

The pattern: **`threads / 2`, snapped to the nearest divisor of the
thread count, from above 4 threads up; no file-splitting at or below
4.** BLAKE3 with this split beat hardware-accelerated (SHA-NI) SHA-256 by
roughly 1.5-2x in every fair, same-thread-count comparison run during
this benchmarking. `optimal_split()` implements exactly that heuristic.

Core choice on the reference machine (256 MiB buffer, `pcore_vs_ecore`
example). At **equal thread count** (8 vs 8, the fair comparison) P- and
E-cores are nearly tied — because those 8 P-threads pack onto just 4
physical P-cores via SMT, so 4 SMT'd P-cores ≈ 8 E-cores here:

| configuration | throughput |
|---|---|
| single thread | ~2.2 GiB/s |
| 8 E-core threads (4x2) | ~8.3 GiB/s |
| 8 P-core threads (4x2) | ~8.6 GiB/s |

The reason to pin to P-cores isn't a big per-thread win — it's that
`PcoreHasher::new()` then gets to use **all 12 P-threads** (~13 GiB/s)
without slow E-cores dragging down a shared work-stealing pool as
stragglers.

## Install

Not yet on crates.io. As a git dependency:

```toml
[dependencies]
pcore-blake3 = { git = "https://github.com/clicraft/pcore-blake3", tag = "v0.2.0" }
```

The CLI:

```sh
cargo install --git https://github.com/clicraft/pcore-blake3
```

Prebuilt Linux and Windows binaries are attached to
[GitHub Releases](https://github.com/clicraft/pcore-blake3/releases)
(built by the release workflow on real Linux and Windows runners).

## Library usage

```rust
use pcore_blake3::PcoreHasher;

let hasher = PcoreHasher::new(); // auto-detects P-cores, picks the split

// Single file
let hash = hasher.hash_file("document.pdf")?;
println!("{}", hash.to_hex());

// In-memory bytes
let hash = hasher.hash_bytes(b"some data");

// A batch, spread across the hasher's pinned pools; results come back
// in input order, one io::Result per file:
let results = hasher.hash_files(&["a.pdf".into(), "b.pdf".into()]);
```

## CLI usage

```console
$ pcore-blake3 --info
Topology: hybrid
Performance cores: [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11]
Efficiency cores: [12, 13, 14, 15, 16, 17, 18, 19]
Thread split: 6 threads/file x 2 concurrent files

$ pcore-blake3 student_01.pdf
f6d63989f74942e5b2789d170a8ef583aedfd99ff6df21f851941ea465d22e27  student_01.pdf
```

Output format is `b3sum`-compatible: `<hex digest><2 spaces><path>`.

## Examples

Each example is self-contained (generates temp data when run without
arguments) and validated on real hybrid hardware:

| example | shows | run |
|---|---|---|
| `detect_topology` | detection results + the heuristic across machine sizes | `cargo run --release --example detect_topology` |
| `hash_file` | single file, throughput, digest self-check vs `blake3::hash` | `cargo run --release --example hash_file -- <path>` |
| `hash_batch` | order-preserving batch over a directory, per-file error isolation | `cargo run --release --example hash_batch -- <dir>` |
| `pcore_vs_ecore` | P-core vs E-core pool throughput on hybrid CPUs | `cargo run --release --example pcore_vs_ecore` |

## API overview

| item | purpose |
|---|---|
| `topology() -> Topology` | `Hybrid` or `Homogeneous` |
| `performance_cpus() -> Vec<usize>` | logical CPU ids of P-cores (all CPUs on homogeneous machines) |
| `efficiency_cpus() -> Vec<usize>` | logical CPU ids of E-cores (empty on homogeneous machines) |
| `pin_current_thread_to_cpu(usize)` | pin the calling thread to one logical CPU |
| `optimal_split(threads) -> (tpf, cf)` | the threads/2 heuristic: threads per file x concurrent files |
| `PcoreHasher::new()` | pools pinned to auto-detected P-cores |
| `PcoreHasher::with_cpus(&[usize])` | pools pinned to an explicit CPU set |
| `PcoreHasher::split()` | the (threads/file, concurrent files) this hasher chose |
| `PcoreHasher::hash_bytes(&[u8])` | hash an in-memory buffer |
| `PcoreHasher::hash_file(path)` | hash one file |
| `PcoreHasher::hash_files(&[PathBuf])` | hash a batch, results in input order |

## Design

```
                       PcoreHasher::new()
                              |
              detect P-cores (affinity module)
                              |
             optimal_split(p_threads) -> (tpf, cf)
                              |
        +---------------------+---------------------+
        |                                           |
   pool #1 (tpf workers,                  pool #cf (tpf workers,
   pinned to P-cpus[0..tpf])              pinned to its own P-cpu slice)
        |                                           |
        +---- shared atomic work queue of files ----+
              (order-preserving results via channel)
```

- **Detection** (`src/affinity.rs`): Linux reads the kernel's hybrid-core
  sysfs markers (`/sys/devices/cpu_core/cpus`, `/sys/devices/cpu_atom/cpus`);
  Windows walks `GetSystemCpuSetInformation`, classifying by
  `EfficiencyClass` (highest class = P-core — an OS abstraction, so it is
  vendor-agnostic).
- **Pools**: one rayon pool per concurrent-file slot, each worker pinned
  to its own CPU from a disjoint slice, so slots never contend for the
  same core and E-cores never steal tree work.
- **Batching**: workers pull file indices from a shared atomic counter
  (no fixed chunking), so an uneven file count can't strand a mostly-idle
  round — a real measurement artifact found and fixed during the original
  benchmarking.

## Platform support

- **Linux**: hybrid detection verified on real hardware (i9-13900HK);
  pinning verified by `sched_getcpu()` readback in the test suite.
- **Windows**: typechecked and clippy-clean against
  `x86_64-pc-windows-gnu`; CI builds, tests, and runs it on
  `windows-latest` runners (homogeneous CPUs). **Hybrid-topology
  behavior has not yet run on real hybrid Windows hardware** — see
  [PORT_VALIDATION.md](PORT_VALIDATION.md) for status and the remaining
  checklist. Single processor group only (≤ 64 logical CPUs).
- Anything else: compiles and works, treating every CPU as a performance
  core (no pinning).

## Validation

This crate is a Rust port of a C reference implementation; the port was
validated by differential testing (28-case shared parser corpus, runtime
comparison on real hardware, cross-target typechecking) with all findings
and residual risks documented in [PORT_VALIDATION.md](PORT_VALIDATION.md).
Both implementations enforce the same strict cpulist grammar and are kept
in lock-step by the shared corpus.

## Changelog

See [CHANGELOG.md](CHANGELOG.md).

## License

Licensed under either of

- MIT license ([LICENSE-MIT](LICENSE-MIT))
- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))

at your option.
