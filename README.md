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
`PcoreHasher::new()` then gets to use **all 6 physical P-cores** (~13
GiB/s) without slow E-cores dragging down a shared work-stealing pool as
stragglers.

### Threads vs cores: SMT is a small, contention-dependent effect

BLAKE3 leans hard on a core's AVX2 (SIMD) units, so the second
SMT/Hyper-Threading thread on a P-core has little idle capacity to exploit
— but "little" is not "none." Measured rigorously on the reference i9
(distinct random DRAM-fed buffers so the memory ceiling isn't hidden by
cache; 15 interleaved trials each, t-test), the effect is **small but
statistically significant, and its sign flips with memory contention:**

| scope | 1 thread/core → 2 threads/core (SMT) | verdict (n=15, 3 runs) |
|---|---|---|
| 6 P-cores alone | **+2.5%** | significant, stable |
| whole machine (14 physical cores) | **−2.5%** | significant, stable |

On the P-cores alone there's enough memory-stall slack for the SMT thread
to add ~+2.5%; once all 14 physical cores are hammering DRAM the machine
is bandwidth-bound and the extra 6 threads cost ~−2.5%.

`PcoreHasher::new_physical()` pins **one thread per physical P-core** —
half the threads of `new()`. It is *not* a throughput win (it's ~2–3%
slower in-memory, since the library uses P-cores where SMT helps); its
value is a smaller thread footprint that leaves the SMT siblings free.
(An earlier, cache-hot measurement wrongly suggested SMT was worthless;
the numbers above are the corrected, DRAM-fed result.)

### Using the E-cores too: maximum throughput

The modes above stay on P-cores. To hash as fast as the machine can,
`PcoreHasher::new_all_physical()` uses **one thread per physical core
across P *and* E cores**, one file per core. Measured on the reference i9:

| comparison | result |
|---|---|
| 6 P-cores vs 14 physical cores, in-memory (isolates E-cores) | **+43%** (t=+49, n=15) |
| batch of 64 diverse files, `new_all_physical()` vs `new()` | **~3x** |

The batch win is larger than the raw +43% because one-file-per-core also
beats splitting each small file's BLAKE3 tree across a pool. It uses the
E-cores (less power-efficient per unit work — a battery/thermal tradeoff
on laptops), and for a *single* file it uses just one core, so prefer
`new()` there. The slow E-cores never straggle: each core pulls whole
files off a shared queue, so P-cores simply hash more files than E-cores.

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

// One thread per PHYSICAL P-core (collapse SMT siblings) — half the
// threads of new(), a smaller footprint (not a throughput win).
let hasher = PcoreHasher::new_physical();

// MAXIMUM throughput: one thread per physical core across P AND E cores,
// one file per core. On a batch of many files this was ~3x new() on the
// reference i9 (E-cores add ~+43% raw; one-file-per-core also beats
// tree-splitting small files). Best for large batches; uses the E-cores.
let hasher = PcoreHasher::new_all_physical();
```

## CLI usage

```console
$ pcore-blake3 --info
Topology: hybrid
Performance cores: [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11] (12 threads, 6 physical)
Efficiency cores: [12, 13, 14, 15, 16, 17, 18, 19] (8 threads)
Thread split: 6 threads/file x 2 concurrent files

$ pcore-blake3 student_01.pdf
f6d63989f74942e5b2789d170a8ef583aedfd99ff6df21f851941ea465d22e27  student_01.pdf

$ pcore-blake3 --physical *.pdf   # one thread per physical P-core
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
| `performance_physical_cpus() -> Vec<usize>` | one logical CPU per physical P-core (SMT siblings collapsed) |
| `all_physical_cpus() -> Vec<usize>` | one logical CPU per physical core, P and E (SMT collapsed) |
| `efficiency_cpus() -> Vec<usize>` | logical CPU ids of E-cores (empty on homogeneous machines) |
| `physical_core_leaders(&[usize]) -> Vec<usize>` | collapse SMT siblings in any CPU set |
| `pin_current_thread_to_cpu(usize)` | pin the calling thread to one logical CPU |
| `optimal_split(threads) -> (tpf, cf)` | the threads/2 heuristic: threads per file x concurrent files |
| `PcoreHasher::new()` | pools pinned to auto-detected P-cores (all P-threads) |
| `PcoreHasher::new_physical()` | one thread per physical P-core (SMT siblings collapsed) |
| `PcoreHasher::new_all_physical()` | one thread per physical core incl. E-cores; max batch throughput |
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
