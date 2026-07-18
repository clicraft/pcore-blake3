# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.5.0] - 2026-07-18

### Changed (breaking)

- **Renamed the crate `pcore-blake3` → `core-blake3`** (package, library
  `core_blake3`, CLI binary, and GitHub repo). It schedules across every
  core, P and E, so the P-core-only name was misleading.
- **Simplified to two modes.** `PcoreHasher` is now `CoreHasher` with just:
  - `CoreHasher::new()` — one thread per **physical core** (all cores, SMT
    siblings collapsed). The efficient default.
  - `CoreHasher::all_threads()` — one thread per **logical CPU** (every SMT
    thread). The conventional baseline.
  Removed `new_physical()`, `new_all_physical()`, `optimal_split()`, and the
  `(threads_per_file, concurrent_files)` `split()` accessor (replaced by
  `threads()`). Internally a single pinned pool now hashes a single file
  tree-parallel across all cores and a batch one-file-per-thread via rayon
  work-stealing.
- CLI: `--physical` / `--all-physical` flags replaced by a single
  `--all-threads` (default is one thread per physical core).
- Added `all_logical_cpus()`.

### Performance (reference i9-13900HK: 6 P-cores + 8 E-cores, 20 logical CPUs)

- Batch of 64 diverse files: `new()` (14 threads, one per physical core)
  **~14.5 GiB/s** vs `all_threads()` (20 logical threads) **~13.3 GiB/s** —
  fewer threads, more throughput.
- SMT (2nd thread per P-core), 15 interleaved trials + t-test: **+2.5% on
  the P-cores alone**, **−2.5% across the whole machine** — small,
  significant, and sign-flipping with memory contention.
- Per physical core, a P-core is **~2x** an E-core; adding the E-cores
  (one thread each) lifts aggregate in-memory throughput **~+40%** — which
  is why both modes span all physical cores, not just the P-cores.

## [0.4.0] - 2026-07-18

### Added

- `PcoreHasher::new_all_physical()` — one thread per physical core across
  **both P and E cores**, one file per core (no intra-file tree split, so
  slow E-cores never straggle inside a file; each core pulls whole files
  off the shared queue). Maximum aggregate throughput when using the
  E-cores is acceptable. Measured on the reference i9: +43% in-memory over
  P-cores-only (isolating the E-core contribution; t=+49, n=15), and ~3x
  `new()` on a batch of 64 diverse files (one-file-per-core also beats
  tree-splitting small files). For a single file it uses one core — prefer
  `new()` there.
- `all_physical_cpus()` helper and CLI `--all-physical` flag; `--info`
  reports the all-physical core count.

### Notes

- E-cores are less power-efficient per unit work, so `new_all_physical()`
  trades battery/thermal for speed on laptops; `new()` (P-cores) remains
  the default.

## [0.3.1] - 2026-07-18

### Changed

- Corrected the SMT characterization for `new_physical()` after
  statistically rigorous, cache-artifact-free measurement (distinct random
  DRAM-fed buffers; 15 interleaved trials + t-test, reproduced across 3
  runs). The v0.3.0 claim that the second SMT thread "adds essentially
  nothing" came from a cache-hot benchmark and was wrong. Corrected result:
  the SMT effect is small but **statistically significant and
  contention-dependent** — about **+2.5% on the P-cores alone** (in-memory)
  and about **−2.5% across the whole 14-physical-core machine** (memory-
  bandwidth-bound). Consequently `new_physical()` is documented as a
  smaller-footprint knob (~2–3% slower than `new()`), not a throughput win;
  `new()` remains the throughput default. Docs only — no API change.

## [0.3.0] - 2026-07-18

### Added

- Physical-core detection and a one-thread-per-physical-core hashing mode:
  - `PcoreHasher::new_physical()` — pins one thread per physical P-core
    (SMT siblings collapsed). Matches `new()`'s throughput with half the
    threads on CPU-bound in-memory hashing, because BLAKE3 saturates a
    core's SIMD units from a single thread (measured within noise on a
    13th-gen i9; the batch path was even marginally faster).
  - `performance_physical_cpus()` and the general `physical_core_leaders()`
    helper (Linux via `thread_siblings_list`; Windows via the CPU-set
    `CoreIndex`).
  - CLI `--physical` flag; `--info` now reports thread vs physical-core
    counts.

### Fixed

- `pcore_vs_ecore` example now compares P- and E-cores at **equal thread
  count** (the fair comparison) instead of all-P vs all-E, which conflated
  core speed with core count. README throughput table updated to match.

## [0.2.0] - 2026-07-18

### Added

- `PcoreHasher::hash_bytes` — hash an in-memory buffer with the tuned,
  pinned pools (previously only file paths could be hashed).
- Usage examples, all runnable and validated on real hybrid hardware:
  - `detect_topology` — inspect detection results and the threads/2
    heuristic table across machine sizes.
  - `hash_file` — single-file hashing with throughput and a self-check
    against reference `blake3::hash`.
  - `hash_batch` — order-preserving directory batches with per-file
    error isolation.
  - `pcore_vs_ecore` — P-core vs E-core pool comparison on hybrid CPUs
    (degrades gracefully on homogeneous machines).
- Release workflow: version tags (`v*`) build release binaries on Linux
  and Windows runners (real Windows compilation) and attach them to a
  GitHub Release.
- Test coverage for `hash_bytes` across BLAKE3 chunk boundaries
  (0/1/1023/1024/1025 bytes and 1 MiB).

### Changed

- `PcoreHasher::hash_file` now delegates to `hash_bytes` (no behavior
  change).
- Release profile strips symbols from binaries.

## [0.1.0] - 2026-07-18

### Added

- Initial release, ported from the C reference implementation
  (batchSigner's `pcore-lib`) and validated against it — see
  [PORT_VALIDATION.md](PORT_VALIDATION.md).
- P-core/E-core detection: Linux via the kernel's hybrid-core sysfs
  markers (verified on a 13th-gen Intel i9); Windows via
  `GetSystemCpuSetInformation`/`EfficiencyClass` (vendor-agnostic,
  covers Intel and AMD hybrid parts).
- Thread pinning (`sched_setaffinity` / `SetThreadAffinityMask`) and
  pinned rayon pools.
- `PcoreHasher` with the empirically derived threads/2 split between
  BLAKE3 tree parallelism and concurrent files; `optimal_split` exposes
  the heuristic directly.
- `core-blake3` CLI (`b3sum`-style output, `--info`).
- Strict cpulist grammar shared with the C reference, locked by a
  28-case differential corpus; value cap 8191 (fixes an OOM-on-malformed-
  input defect found during port validation).
- CI: build/clippy/test on `ubuntu-latest` and `windows-latest`, plus a
  Linux-side cross-target typecheck of the Windows module.

[Unreleased]: https://github.com/clicraft/core-blake3/compare/v0.5.0...HEAD
[0.5.0]: https://github.com/clicraft/core-blake3/compare/v0.4.0...v0.5.0
[0.4.0]: https://github.com/clicraft/core-blake3/compare/v0.3.1...v0.4.0
[0.3.1]: https://github.com/clicraft/core-blake3/compare/v0.3.0...v0.3.1
[0.3.0]: https://github.com/clicraft/core-blake3/compare/v0.2.0...v0.3.0
[0.2.0]: https://github.com/clicraft/core-blake3/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/clicraft/core-blake3/releases/tag/v0.1.0
