//! Compare BLAKE3 throughput when the hasher is pinned to performance
//! cores vs efficiency cores. Hybrid machines only — on a homogeneous CPU
//! it explains why there's nothing to compare and exits cleanly.
//!
//! Run: `cargo run --release --example pcore_vs_ecore`
//!
//! Covers: [`PcoreHasher::with_cpus`], [`PcoreHasher::hash_bytes`],
//! [`topology`] gating.
//!
//! This is a demo, not a benchmark: single buffer, best-of-5 timing, no
//! shuffling or cache control. For rigorous numbers, see the methodology
//! in the README's "Why" section.

use pcore_blake3::{efficiency_cpus, performance_cpus, topology, PcoreHasher, Topology};
use std::time::{Duration, Instant};

const SIZE: usize = 256 * 1024 * 1024;
const REPS: usize = 5;

fn best_of(reps: usize, mut f: impl FnMut()) -> Duration {
    let mut best = Duration::MAX;
    for _ in 0..reps {
        let start = Instant::now();
        f();
        best = best.min(start.elapsed());
    }
    best
}

fn mib_s(bytes: usize, d: Duration) -> f64 {
    bytes as f64 / (1024.0 * 1024.0) / d.as_secs_f64()
}

fn main() {
    if topology() != Topology::Hybrid {
        println!("This machine reports a homogeneous CPU topology: every core is the");
        println!("same kind, so a P-core vs E-core comparison is not possible here.");
        return;
    }
    let p_cpus = performance_cpus();
    let e_cpus = efficiency_cpus();

    // Fair comparison: EQUAL thread counts on both core types. Using all
    // P-threads vs all E-threads would conflate "P-cores are faster" with
    // "there are simply more P-threads", so cap both sides to the smaller
    // pool's size and take a prefix of each.
    let n = p_cpus.len().min(e_cpus.len());
    let p_subset = &p_cpus[..n];
    let e_subset = &e_cpus[..n];

    let p_hasher = PcoreHasher::with_cpus(p_subset);
    let e_hasher = PcoreHasher::with_cpus(e_subset);
    // Same thread count -> identical split for both, so the only variable
    // is which physical cores run the work.
    let (tpf, cf) = p_hasher.split();

    println!("Buffer: {} MiB, best of {REPS} runs each", SIZE >> 20);
    println!("Fair comparison: {n} threads each, {tpf} threads/file x {cf} concurrent files");
    println!("  P-core threads: {p_subset:?}");
    println!("  E-core threads: {e_subset:?}");
    if n < p_cpus.len() {
        println!(
            "  (note: this machine has {} P-threads and {} E-threads; capping to {n} for parity.",
            p_cpus.len(),
            e_cpus.len()
        );
        println!("   Intel P-cores use SMT, so these {n} P-threads sit on {} physical cores.)", n / 2);
    }
    println!();

    let data: Vec<u8> = (0..SIZE).map(|i| (i % 251) as u8).collect();

    // Warm-up (page in the buffer, spin up the pools) before timing.
    let p_digest = p_hasher.hash_bytes(&data);
    let e_digest = e_hasher.hash_bytes(&data);
    assert_eq!(p_digest, e_digest, "digest must not depend on which cores computed it");

    let t_single = best_of(REPS, || {
        blake3::hash(&data);
    });
    let t_p = best_of(REPS, || {
        p_hasher.hash_bytes(&data);
    });
    let t_e = best_of(REPS, || {
        e_hasher.hash_bytes(&data);
    });

    println!("{:<34} {:>12} {:>12}", "configuration", "time (ms)", "MiB/s");
    println!(
        "{:<34} {:>12.1} {:>12.0}",
        "single thread (reference)",
        t_single.as_secs_f64() * 1e3,
        mib_s(SIZE, t_single)
    );
    println!(
        "{:<34} {:>12.1} {:>12.0}",
        format!("{n} P-core threads ({tpf}x{cf})"),
        t_p.as_secs_f64() * 1e3,
        mib_s(SIZE, t_p)
    );
    println!(
        "{:<34} {:>12.1} {:>12.0}",
        format!("{n} E-core threads ({tpf}x{cf})"),
        t_e.as_secs_f64() * 1e3,
        mib_s(SIZE, t_e)
    );

    println!(
        "\nAt equal thread count, P-cores are {:.2}x the E-cores here (digests identical).",
        t_e.as_secs_f64() / t_p.as_secs_f64()
    );
}
