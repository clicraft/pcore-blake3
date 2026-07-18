# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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
- `pcore-blake3` CLI (`b3sum`-style output, `--info`).
- Strict cpulist grammar shared with the C reference, locked by a
  28-case differential corpus; value cap 8191 (fixes an OOM-on-malformed-
  input defect found during port validation).
- CI: build/clippy/test on `ubuntu-latest` and `windows-latest`, plus a
  Linux-side cross-target typecheck of the Windows module.

[Unreleased]: https://github.com/clicraft/pcore-blake3/compare/v0.2.0...HEAD
[0.2.0]: https://github.com/clicraft/pcore-blake3/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/clicraft/pcore-blake3/releases/tag/v0.1.0
