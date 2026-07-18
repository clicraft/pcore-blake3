//! BLAKE3 hashing that auto-detects performance ("P") cores on hybrid
//! CPUs and picks a thread split between BLAKE3's internal tree
//! parallelism and concurrent-file parallelism.
//!
//! Background: on a hybrid CPU, mixing P-cores and E-cores in one
//! "use all cores" thread pool blends two different core speeds, and the
//! optimal way to split N available threads between "one file's internal
//! BLAKE3 tree" and "how many files run at once" isn't 1 thread/file or
//! N threads/file — empirically (see the `pcore-blake3` README) it's
//! close to `threads / 2` for anything above ~4 threads, and "don't
//! split at all" below that.
//!
//! ```no_run
//! use pcore_blake3::PcoreHasher;
//! use std::path::PathBuf;
//!
//! let hasher = PcoreHasher::new(); // auto-detects P-cores
//! let hash = hasher.hash_file("document.pdf").unwrap();
//! println!("{}", hash.to_hex());
//! ```

mod affinity;

pub use affinity::{
    all_physical_cpus, efficiency_cpus, performance_cpus, performance_physical_cpus,
    physical_core_leaders, pin_current_thread_to_cpu, topology, Topology,
};

use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{mpsc, Arc};

/// All divisors of `n`, ascending.
fn divisors(n: usize) -> Vec<usize> {
    (1..=n.max(1)).filter(|d| n.is_multiple_of(*d)).collect()
}

/// Splits `total_threads` into (threads devoted to one file's internal
/// BLAKE3 tree, how many files run concurrently).
///
/// Empirically confirmed (see README) on a real hybrid CPU across 2-6
/// P-cores (4-12 threads): `threads/2`, snapped to the nearest divisor of
/// `total_threads`, is a strong default from 4 threads up. Below that,
/// splitting across files loses more from a thinner internal tree than it
/// gains from concurrency, so the whole budget goes to one file at a time.
pub fn optimal_split(total_threads: usize) -> (usize, usize) {
    let total_threads = total_threads.max(1);
    if total_threads <= 4 {
        return (total_threads, 1);
    }
    let target = total_threads as f64 / 2.0;
    let threads_per_file = divisors(total_threads)
        .into_iter()
        .min_by(|a, b| (*a as f64 - target).abs().partial_cmp(&(*b as f64 - target).abs()).unwrap())
        .unwrap_or(total_threads);
    let concurrent_files = (total_threads / threads_per_file).max(1);
    (threads_per_file, concurrent_files)
}

/// Which logical CPUs to hash on. Defaults to P-cores on a hybrid
/// machine, all available CPUs otherwise.
fn default_cpus() -> Vec<usize> {
    match topology() {
        Topology::Hybrid => {
            let cpus = performance_cpus();
            if cpus.is_empty() {
                (0..std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1)).collect()
            } else {
                cpus
            }
        }
        Topology::Homogeneous => (0..std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1)).collect(),
    }
}

/// Like [`default_cpus`] but with SMT siblings collapsed: one logical CPU
/// per physical core.
fn default_physical_cpus() -> Vec<usize> {
    physical_core_leaders(&default_cpus())
}

/// Builds a rayon thread pool whose worker threads are each pinned, in
/// round-robin order, to one CPU from `cpus`.
fn build_pinned_pool(num_threads: usize, cpus: Vec<usize>) -> rayon::ThreadPool {
    let counter = Arc::new(AtomicUsize::new(0));
    let cpus = Arc::new(cpus);
    rayon::ThreadPoolBuilder::new()
        .num_threads(num_threads.max(1))
        .spawn_handler(move |thread| {
            let counter = Arc::clone(&counter);
            let cpus = Arc::clone(&cpus);
            std::thread::Builder::new().spawn(move || {
                if !cpus.is_empty() {
                    let idx = counter.fetch_add(1, Ordering::SeqCst);
                    let _ = pin_current_thread_to_cpu(cpus[idx % cpus.len()]);
                }
                thread.run();
            })?;
            Ok(())
        })
        .build()
        .expect("build pinned rayon thread pool")
}

/// A BLAKE3 hasher tuned to this machine's P-cores (or all cores, on a
/// non-hybrid machine), with `concurrent_files` independent worker pools
/// of `threads_per_file` pinned threads each — see [`optimal_split`].
pub struct PcoreHasher {
    pools: Vec<rayon::ThreadPool>,
    threads_per_file: usize,
    concurrent_files: usize,
}

impl PcoreHasher {
    /// Auto-detects P-cores (falling back to all available CPUs on a
    /// non-hybrid machine) and builds the pinned pools, using every P-core
    /// hardware thread (both SMT siblings, where present).
    ///
    /// Best when hashing may stall on I/O (e.g. [`Self::hash_files`] reading
    /// from disk): the second SMT thread can hash while its sibling waits.
    pub fn new() -> Self {
        Self::with_cpus(&default_cpus())
    }

    /// Like [`Self::new`] but pins **one thread per physical P-core**,
    /// collapsing SMT siblings — half the threads of [`Self::new`].
    ///
    /// This is **not** a throughput win: rigorous DRAM-fed measurement on a
    /// 13th-gen i9 (n=15, t-test) found the second SMT thread per P-core
    /// *helps* in-memory hashing by a small but significant ~2.5%, so
    /// `new_physical()` runs ~2–3% slower than [`Self::new`]. Its value is
    /// a smaller thread footprint that leaves the SMT-sibling logical CPUs
    /// free for other work. **Prefer [`Self::new`] for maximum
    /// throughput**, especially on I/O-bound batches where the idle sibling
    /// hides read latency.
    pub fn new_physical() -> Self {
        Self::with_cpus(&default_physical_cpus())
    }

    /// Pins **one thread per physical core across the whole machine** —
    /// every P-core and every E-core (SMT siblings collapsed), one file per
    /// core with no internal tree split (`threads_per_file == 1`).
    ///
    /// This is the maximum-aggregate-throughput mode when you're willing to
    /// use the E-cores: on the reference i9 the 14 physical cores hit
    /// ~1.4x the 6 P-cores alone. One-file-per-core (rather than splitting a
    /// file's BLAKE3 tree across a mixed-speed pool) is deliberate — it
    /// avoids slow E-cores becoming stragglers *inside* a single file's
    /// hash; instead each core independently pulls whole files off the
    /// shared queue, so fast P-cores simply hash more files than E-cores.
    ///
    /// Best for large batches ([`Self::hash_files`]). For a single file it
    /// uses just one core, so prefer [`Self::new`] there. Note the E-cores
    /// are less power-efficient per unit work — on a laptop this trades
    /// battery/thermal for speed.
    pub fn new_all_physical() -> Self {
        let cpus = all_physical_cpus();
        if cpus.is_empty() {
            return Self::new();
        }
        // One thread per file, one file per core: pure files-parallel.
        Self::build(&cpus, 1, cpus.len())
    }

    /// Same as [`Self::new`] but pinned only to the given logical CPU IDs
    /// — useful for testing, or to deliberately restrict to a subset
    /// (e.g. efficiency cores, for comparison).
    pub fn with_cpus(cpus: &[usize]) -> Self {
        let (threads_per_file, concurrent_files) = optimal_split(cpus.len().max(1));
        Self::build(cpus, threads_per_file, concurrent_files)
    }

    /// Builds the pinned pools for an explicit split. `concurrent_files`
    /// pools, each of `threads_per_file` threads pinned to its own disjoint
    /// slice of `cpus`.
    fn build(cpus: &[usize], threads_per_file: usize, concurrent_files: usize) -> Self {
        let pools = (0..concurrent_files)
            .map(|slot| {
                if cpus.is_empty() {
                    return build_pinned_pool(threads_per_file, Vec::new());
                }
                let start = (slot * threads_per_file).min(cpus.len());
                let end = (start + threads_per_file).min(cpus.len());
                build_pinned_pool(threads_per_file, cpus[start..end].to_vec())
            })
            .collect();

        PcoreHasher { pools, threads_per_file, concurrent_files }
    }

    /// How this hasher split available threads: (threads per file, files
    /// hashed concurrently).
    pub fn split(&self) -> (usize, usize) {
        (self.threads_per_file, self.concurrent_files)
    }

    /// Hashes an in-memory buffer, using this hasher's first pool's
    /// internal BLAKE3 tree parallelism. The digest is identical to
    /// `blake3::hash(data)` — parallelism never changes the result.
    pub fn hash_bytes(&self, data: &[u8]) -> blake3::Hash {
        self.pools[0].install(|| {
            let mut hasher = blake3::Hasher::new();
            hasher.update_rayon(data);
            hasher.finalize()
        })
    }

    /// Hashes a single file, using this hasher's first pool's internal
    /// BLAKE3 tree parallelism.
    pub fn hash_file(&self, path: impl AsRef<Path>) -> io::Result<blake3::Hash> {
        let data = std::fs::read(path)?;
        Ok(self.hash_bytes(&data))
    }

    /// Hashes many files, spreading them across this hasher's concurrent
    /// pools (a shared work queue, not fixed chunking, so progress isn't
    /// wasted when `paths.len()` isn't a multiple of `concurrent_files`).
    /// Results are returned in the same order as `paths`.
    pub fn hash_files(&self, paths: &[PathBuf]) -> Vec<io::Result<blake3::Hash>> {
        let next = AtomicUsize::new(0);
        let (tx, rx) = mpsc::channel();

        std::thread::scope(|scope| {
            for pool in &self.pools {
                let tx = tx.clone();
                let next = &next;
                scope.spawn(move || loop {
                    let idx = next.fetch_add(1, Ordering::Relaxed);
                    if idx >= paths.len() {
                        break;
                    }
                    let result = std::fs::read(&paths[idx]).map(|data| {
                        pool.install(|| {
                            let mut hasher = blake3::Hasher::new();
                            hasher.update_rayon(&data);
                            hasher.finalize()
                        })
                    });
                    let _ = tx.send((idx, result));
                });
            }
        });
        drop(tx);

        let mut out: Vec<Option<io::Result<blake3::Hash>>> = (0..paths.len()).map(|_| None).collect();
        for (idx, result) in rx {
            out[idx] = Some(result);
        }
        out.into_iter()
            .map(|o| o.unwrap_or_else(|| Err(io::Error::other("missing result"))))
            .collect()
    }
}

impl Default for PcoreHasher {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn optimal_split_below_threshold_never_splits_files() {
        for threads in 1..=4 {
            assert_eq!(optimal_split(threads), (threads, 1));
        }
    }

    #[test]
    fn new_physical_hashes_correctly_with_no_more_threads_than_new() {
        let data: Vec<u8> = (0..1 << 20).map(|i| (i % 251) as u8).collect();
        let phys = PcoreHasher::new_physical();
        // Correctness is the invariant that must always hold, regardless of
        // how many cores the split ended up using.
        assert_eq!(phys.hash_bytes(&data), blake3::hash(&data));

        // The physical hasher never spins up more total threads than the
        // all-threads hasher (it collapses SMT siblings).
        let phys_threads = { let (t, c) = phys.split(); t * c };
        let all_threads = { let (t, c) = PcoreHasher::new().split(); t * c };
        assert!(phys_threads <= all_threads, "physical uses <= threads than all-threads");
    }

    #[test]
    fn new_all_physical_hashes_correctly_and_is_files_parallel() {
        let data: Vec<u8> = (0..1 << 20).map(|i| (i % 251) as u8).collect();
        let h = PcoreHasher::new_all_physical();
        assert_eq!(h.hash_bytes(&data), blake3::hash(&data));
        // One thread per file (no intra-file tree split): tpf == 1, and it
        // uses at least as many concurrent files as the P-only physical set.
        let (tpf, cf) = h.split();
        assert_eq!(tpf, 1, "all-physical is one-file-per-core");
        assert!(cf >= PcoreHasher::new_physical().split().1.max(1));

        // Batch correctness across the mixed-speed pools.
        let files: Vec<_> = (0..5u8)
            .map(|i| {
                let mut p = std::env::temp_dir();
                p.push(format!("pcore-allphys-test-{i}-{:?}", std::thread::current().id()));
                std::fs::write(&p, [i; 4096]).unwrap();
                p
            })
            .collect();
        let got = h.hash_files(&files);
        for (i, r) in got.into_iter().enumerate() {
            assert_eq!(r.unwrap(), blake3::hash(&[i as u8; 4096]));
        }
        for f in &files {
            let _ = std::fs::remove_file(f);
        }
    }

    #[test]
    fn optimal_split_stays_within_bounds() {
        for threads in 1..=64 {
            let (tpf, cf) = optimal_split(threads);
            assert!(tpf >= 1 && tpf <= threads.max(1));
            assert!(cf >= 1);
            assert!(tpf * cf <= threads.max(1) || threads == 0);
        }
    }

    #[test]
    fn hash_bytes_matches_reference_blake3() {
        let hasher = PcoreHasher::with_cpus(&[0, 1]);
        // Cover both sides of BLAKE3's 1024-byte chunk boundary and a
        // multi-chunk buffer where the parallel tree path actually engages.
        for len in [0usize, 1, 1023, 1024, 1025, 1 << 20] {
            let data: Vec<u8> = (0..len).map(|i| (i % 251) as u8).collect();
            assert_eq!(hasher.hash_bytes(&data), blake3::hash(&data), "len {len}");
        }
    }

    #[test]
    fn hash_file_matches_reference_blake3() {
        let mut tmp = tempfile_with_bytes(b"hello pcore-blake3");
        let hasher = PcoreHasher::with_cpus(&[0, 1]);
        let got = hasher.hash_file(tmp.path()).unwrap();
        let want = blake3::hash(b"hello pcore-blake3");
        assert_eq!(got, want);
        tmp.flush().unwrap();
    }

    #[test]
    fn hash_files_matches_reference_and_preserves_order() {
        let contents: Vec<&[u8]> = vec![b"one", b"two", b"three", b"four", b"five"];
        let files: Vec<_> = contents.iter().map(|c| tempfile_with_bytes(c)).collect();
        let paths: Vec<PathBuf> = files.iter().map(|f| f.path().to_path_buf()).collect();

        let hasher = PcoreHasher::with_cpus(&[0, 1, 2, 3]);
        let results = hasher.hash_files(&paths);

        assert_eq!(results.len(), contents.len());
        for (result, content) in results.into_iter().zip(contents) {
            assert_eq!(result.unwrap(), blake3::hash(content));
        }
    }

    struct TempFile {
        path: PathBuf,
    }

    impl TempFile {
        fn path(&self) -> &Path {
            &self.path
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl Drop for TempFile {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.path);
        }
    }

    fn tempfile_with_bytes(data: &[u8]) -> TempFile {
        let mut path = std::env::temp_dir();
        let unique = format!("pcore-blake3-test-{:p}-{}", data.as_ptr(), data.len());
        path.push(unique);
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(data).unwrap();
        TempFile { path }
    }
}
