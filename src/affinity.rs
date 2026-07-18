//! Cross-platform detection of performance ("P") vs efficiency ("E") CPU
//! cores on hybrid CPUs, plus thread pinning.
//!
//! Linux: reads the kernel's hybrid-core sysfs markers
//! (`/sys/devices/cpu_core/cpus`, `/sys/devices/cpu_atom/cpus`), present
//! since Linux 5.16 on Intel Alder Lake+. Verified against real hardware
//! (a 13th-gen Intel i9).
//!
//! Windows: uses `GetSystemCpuSetInformation` and each logical processor's
//! `EfficiencyClass` (higher value = faster/less power-efficient =
//! performance core, per Microsoft's `SYSTEM_CPU_SET_INFORMATION` docs).
//! This is a Windows scheduler abstraction, not an Intel-specific one, so
//! it applies to AMD hybrid parts too. **Not yet built or run on real
//! Windows hardware** — written from documentation only.

use std::io;

/// Whether this machine exposes a hybrid P-core/E-core topology.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Topology {
    /// No P/E distinction detected (or detection unavailable).
    Homogeneous,
    /// Hybrid P-core/E-core topology detected.
    Hybrid,
}

/// Detects this machine's CPU topology kind.
pub fn topology() -> Topology {
    imp::topology()
}

/// Logical CPU IDs of performance cores. On a non-hybrid system, this is
/// every available logical CPU (there's no distinction to make).
pub fn performance_cpus() -> Vec<usize> {
    imp::performance_cpus()
}

/// Logical CPU IDs of efficiency cores. Empty on a non-hybrid system.
pub fn efficiency_cpus() -> Vec<usize> {
    imp::efficiency_cpus()
}

/// Collapses SMT siblings: given a set of logical CPU IDs, returns one
/// representative (the lowest-numbered sibling present in the input) per
/// distinct physical core, preserving first-seen order.
///
/// On a machine with SMT/Hyper-Threading this roughly halves the list;
/// with no SMT it returns the input unchanged. If per-CPU topology can't
/// be read, it conservatively returns the input as-is (every CPU treated
/// as its own core).
///
/// Useful because some workloads — notably BLAKE3, which saturates a
/// core's SIMD units from a single thread — gain nothing from the second
/// SMT thread on a core, so one thread per *physical* core delivers the
/// same throughput with half the threads.
pub fn physical_core_leaders(cpus: &[usize]) -> Vec<usize> {
    imp::physical_leaders(cpus)
}

/// One logical CPU per physical performance core (SMT siblings collapsed).
/// Convenience for `physical_core_leaders(&performance_cpus())`.
pub fn performance_physical_cpus() -> Vec<usize> {
    physical_core_leaders(&performance_cpus())
}

/// One logical CPU per physical core across the WHOLE machine — every
/// performance core and every efficiency core, SMT siblings collapsed.
/// This is the set that maximizes aggregate throughput when you're willing
/// to use the E-cores too (they simply pull fewer work items than P-cores).
pub fn all_physical_cpus() -> Vec<usize> {
    let mut cpus = performance_cpus();
    cpus.extend(efficiency_cpus());
    physical_core_leaders(&cpus)
}

/// Pins the calling thread to a single logical CPU.
///
/// Linux: `sched_setaffinity(0, ...)`, which affects only the calling
/// thread, not the whole process.
/// Windows: `SetThreadAffinityMask` on the current thread. CPU IDs only
/// map to the right physical core when the machine fits in one processor
/// group (<=64 logical CPUs) — true for virtually all consumer/laptop
/// chips. **Not yet verified on real Windows hardware.**
pub fn pin_current_thread_to_cpu(cpu: usize) -> io::Result<()> {
    imp::pin_current_thread_to_cpu(cpu)
}

/// Largest CPU ID the cpulist parser accepts. Linux caps `NR_CPUS` at
/// 8192 on the largest configurations, so valid IDs are 0..=8191. The cap
/// exists for safety, not just hygiene: without it, a malformed range like
/// "0-999999999999" would make the parser try to materialize a
/// trillion-element Vec.
const MAX_CPU_ID: usize = 8191;

/// Parses a Linux cpulist string ("0-3,8,10-11") into individual CPU IDs.
///
/// Strict canonical grammar, enforced identically by the C reference
/// implementation in batchSigner's `pcore-lib` (kept in lock-step via a
/// shared differential test corpus):
///
/// ```text
/// list := "" | term ("," term)* [","]      (one trailing comma tolerated)
/// term := num | num "-" num                (low <= high)
/// num  := [0-9]+                           (value <= MAX_CPU_ID)
/// ```
///
/// Anything else — whitespace, negatives, trailing garbage, empty terms in
/// the middle, out-of-range values — is an error (`None`). Callers strip
/// the sysfs trailing newline before calling; the parser itself never
/// trims.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn parse_cpu_list(s: &str) -> Option<Vec<usize>> {
    fn parse_num(t: &str) -> Option<usize> {
        if t.is_empty() || !t.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        let v: usize = t.parse().ok()?; // fails on overflow
        (v <= MAX_CPU_ID).then_some(v)
    }

    let parts: Vec<&str> = s.split(',').collect();
    let mut out = Vec::new();
    for (i, part) in parts.iter().enumerate() {
        if part.is_empty() {
            if i + 1 == parts.len() {
                break; // trailing comma (or empty input)
            }
            return None; // empty term in the middle, e.g. "5,,6"
        }
        match part.split_once('-') {
            Some((a, b)) => {
                let a = parse_num(a)?;
                let b = parse_num(b)?;
                if b < a {
                    return None;
                }
                out.extend(a..=b);
            }
            None => out.push(parse_num(part)?),
        }
    }
    Some(out)
}

#[cfg(target_os = "linux")]
mod imp {
    use super::{parse_cpu_list, Topology};
    use std::io;

    const SYSFS_CPU_CORE_CPUS: &str = "/sys/devices/cpu_core/cpus";
    const SYSFS_CPU_ATOM_CPUS: &str = "/sys/devices/cpu_atom/cpus";
    const SYSFS_ONLINE_CPUS: &str = "/sys/devices/system/cpu/online";

    fn read_cpu_list(path: &str) -> Option<Vec<usize>> {
        let s = std::fs::read_to_string(path).ok()?;
        // Strip only the sysfs trailing newline(s), exactly like the C
        // reference's read_sysfs_line(); the parser itself is strict and
        // rejects any other whitespace.
        parse_cpu_list(s.trim_end_matches(['\n', '\r']))
    }

    pub fn topology() -> Topology {
        // Openability, not mere existence, to match the C reference's
        // fopen() semantics (an unreadable marker counts as absent).
        if std::fs::File::open(SYSFS_CPU_CORE_CPUS).is_ok() {
            Topology::Hybrid
        } else {
            Topology::Homogeneous
        }
    }

    pub fn performance_cpus() -> Vec<usize> {
        read_cpu_list(SYSFS_CPU_CORE_CPUS)
            .or_else(|| read_cpu_list(SYSFS_ONLINE_CPUS))
            .unwrap_or_default()
    }

    pub fn efficiency_cpus() -> Vec<usize> {
        read_cpu_list(SYSFS_CPU_ATOM_CPUS).unwrap_or_default()
    }

    /// The logical CPUs sharing a physical core with `cpu` (including
    /// itself), from `topology/thread_siblings_list`.
    fn thread_siblings(cpu: usize) -> Option<Vec<usize>> {
        let path = format!("/sys/devices/system/cpu/cpu{cpu}/topology/thread_siblings_list");
        let s = std::fs::read_to_string(path).ok()?;
        parse_cpu_list(s.trim_end_matches(['\n', '\r']))
    }

    pub fn physical_leaders(cpus: &[usize]) -> Vec<usize> {
        use std::collections::HashSet;
        let want: HashSet<usize> = cpus.iter().copied().collect();
        let mut seen_cores: HashSet<usize> = HashSet::new();
        let mut out = Vec::new();
        for &cpu in cpus {
            let siblings = thread_siblings(cpu).unwrap_or_else(|| vec![cpu]);
            // A core is keyed by its lowest sibling id, so the key is the
            // same whichever sibling we visit first.
            let core_key = siblings.iter().copied().min().unwrap_or(cpu);
            if seen_cores.insert(core_key) {
                // Representative: lowest sibling that's actually in the
                // requested set (so we never emit a CPU the caller excluded).
                let rep = siblings.iter().copied().filter(|s| want.contains(s)).min().unwrap_or(cpu);
                out.push(rep);
            }
        }
        out
    }

    pub fn pin_current_thread_to_cpu(cpu: usize) -> io::Result<()> {
        unsafe {
            let mut set: libc::cpu_set_t = std::mem::zeroed();
            libc::CPU_ZERO(&mut set);
            libc::CPU_SET(cpu, &mut set);
            // pid 0 means "the calling thread" on Linux, not the whole process.
            let rc = libc::sched_setaffinity(0, std::mem::size_of::<libc::cpu_set_t>(), &set);
            if rc != 0 {
                return Err(io::Error::last_os_error());
            }
        }
        Ok(())
    }
}

#[cfg(target_os = "windows")]
mod imp {
    use super::Topology;
    use std::io;
    use windows_sys::Win32::System::SystemInformation::{
        CpuSetInformation, GetSystemCpuSetInformation, SYSTEM_CPU_SET_INFORMATION,
    };
    use windows_sys::Win32::System::Threading::{GetCurrentThread, SetThreadAffinityMask};

    /// One logical processor's identity from the CPU-set table.
    struct CpuInfo {
        logical: usize,
        efficiency: u8,
        /// `CoreIndex`: same value for logical processors sharing a
        /// physical core (i.e. SMT siblings).
        core: u8,
    }

    /// Returns a `CpuInfo` for every logical processor on the system.
    fn query_cpu_sets() -> Option<Vec<CpuInfo>> {
        unsafe {
            let mut needed: u32 = 0;
            // Signature: GetSystemCpuSetInformation(Information, BufferLength,
            // ReturnedLength, Process, Flags). NULL/0 buffer first to learn
            // the required size; Process = NULL queries system-wide state.
            GetSystemCpuSetInformation(std::ptr::null_mut(), 0, &mut needed, std::ptr::null_mut(), 0);
            if needed == 0 {
                return None;
            }

            // Back the byte buffer with u64s so its base address satisfies
            // SYSTEM_CPU_SET_INFORMATION's 8-byte alignment (it contains a
            // u64 AllocationTag); a Vec<u8> allocation carries no such
            // guarantee, and casting a misaligned pointer to a struct
            // reference would be UB. Entries past the first are still read
            // with read_unaligned so no assumption about Size keeping
            // alignment is needed either.
            let words = (needed as usize).div_ceil(std::mem::size_of::<u64>());
            let mut buf: Vec<u64> = vec![0u64; words];
            let ok = GetSystemCpuSetInformation(
                buf.as_mut_ptr().cast::<SYSTEM_CPU_SET_INFORMATION>(),
                needed,
                &mut needed,
                std::ptr::null_mut(),
                0,
            );
            if ok == 0 {
                return None;
            }

            let base = buf.as_ptr().cast::<u8>();
            let len = needed as usize;
            let entry_size = std::mem::size_of::<SYSTEM_CPU_SET_INFORMATION>();

            let mut result = Vec::new();
            let mut offset = 0usize;
            // Size (u32) sits at offset 0 and Type (i32) at offset 4 of
            // every entry; only read the full struct once both the declared
            // Size and our struct definition fit inside the buffer.
            while offset + 8 <= len {
                let size = std::ptr::read_unaligned(base.add(offset).cast::<u32>()) as usize;
                if size == 0 {
                    break; // malformed; avoid infinite loop
                }
                let ty = std::ptr::read_unaligned(base.add(offset + 4).cast::<i32>());
                if ty == CpuSetInformation && offset + entry_size <= len && size >= entry_size {
                    let info = std::ptr::read_unaligned(base.add(offset).cast::<SYSTEM_CPU_SET_INFORMATION>());
                    let cpu_set = info.Anonymous.CpuSet;
                    result.push(CpuInfo {
                        logical: cpu_set.LogicalProcessorIndex as usize,
                        efficiency: cpu_set.EfficiencyClass,
                        core: cpu_set.CoreIndex,
                    });
                }
                offset += size;
            }
            Some(result)
        }
    }

    pub fn topology() -> Topology {
        match query_cpu_sets() {
            Some(sets) if !sets.is_empty() => {
                let min = sets.iter().map(|c| c.efficiency).min().unwrap();
                let max = sets.iter().map(|c| c.efficiency).max().unwrap();
                if max > min {
                    Topology::Hybrid
                } else {
                    Topology::Homogeneous
                }
            }
            _ => Topology::Homogeneous,
        }
    }

    pub fn performance_cpus() -> Vec<usize> {
        match query_cpu_sets() {
            Some(sets) if !sets.is_empty() => {
                let max = sets.iter().map(|c| c.efficiency).max().unwrap();
                sets.iter().filter(|c| c.efficiency == max).map(|c| c.logical).collect()
            }
            _ => Vec::new(),
        }
    }

    pub fn efficiency_cpus() -> Vec<usize> {
        match query_cpu_sets() {
            Some(sets) if !sets.is_empty() => {
                let min = sets.iter().map(|c| c.efficiency).min().unwrap();
                let max = sets.iter().map(|c| c.efficiency).max().unwrap();
                if max == min {
                    return Vec::new(); // homogeneous
                }
                sets.iter().filter(|c| c.efficiency < max).map(|c| c.logical).collect()
            }
            _ => Vec::new(),
        }
    }

    pub fn physical_leaders(cpus: &[usize]) -> Vec<usize> {
        use std::collections::{HashMap, HashSet};
        let Some(sets) = query_cpu_sets() else {
            return cpus.to_vec(); // no topology info; treat each as its own core
        };
        let core_of: HashMap<usize, u8> = sets.iter().map(|c| (c.logical, c.core)).collect();
        let mut seen_cores: HashSet<u8> = HashSet::new();
        let mut out = Vec::new();
        for &cpu in cpus {
            match core_of.get(&cpu) {
                // First logical CPU we see for a given physical core wins;
                // callers pass sorted lists, so that's the lowest sibling.
                Some(&core) => {
                    if seen_cores.insert(core) {
                        out.push(cpu);
                    }
                }
                None => out.push(cpu), // unknown CPU; treat as its own core
            }
        }
        out
    }

    pub fn pin_current_thread_to_cpu(cpu: usize) -> io::Result<()> {
        if cpu >= 64 {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "cpu id outside this thread's processor group (>=64)"));
        }
        unsafe {
            let mask: usize = 1usize << cpu;
            let prev = SetThreadAffinityMask(GetCurrentThread(), mask);
            if prev == 0 {
                return Err(io::Error::last_os_error());
            }
        }
        Ok(())
    }
}

#[cfg(not(any(target_os = "linux", target_os = "windows")))]
mod imp {
    use super::Topology;
    use std::io;

    pub fn topology() -> Topology {
        Topology::Homogeneous
    }

    pub fn performance_cpus() -> Vec<usize> {
        (0..std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1)).collect()
    }

    pub fn efficiency_cpus() -> Vec<usize> {
        Vec::new()
    }

    pub fn physical_leaders(cpus: &[usize]) -> Vec<usize> {
        // No topology source on this platform; treat every CPU as its own core.
        cpus.to_vec()
    }

    pub fn pin_current_thread_to_cpu(_cpu: usize) -> io::Result<()> {
        Err(io::Error::new(io::ErrorKind::Unsupported, "CPU pinning not supported on this platform"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Differential regression corpus, shared verbatim with the C
    /// reference implementation (batchSigner's pcore-lib). Any change to
    /// either parser must keep both producing exactly these results —
    /// re-run the C harness (`parse_harness.c`) on this corpus to verify.
    const CORPUS: &[(&str, Option<&[usize]>)] = &[
        // canonical sysfs forms
        ("0-11", Some(&[0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11])),
        ("12-19", Some(&[12, 13, 14, 15, 16, 17, 18, 19])),
        ("0-19", Some(&[0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19])),
        ("0", Some(&[0])),
        ("0,2-5", Some(&[0, 2, 3, 4, 5])),
        ("0-3,8,10-11", Some(&[0, 1, 2, 3, 8, 10, 11])),
        ("007", Some(&[7])),
        ("8191", Some(&[8191])),
        ("", Some(&[])),
        // tolerated legacy form: one trailing comma
        ("5,", Some(&[5])),
        ("0-3,", Some(&[0, 1, 2, 3])),
        // rejected
        ("5,,6", None),
        ("5,,", None),
        ("0-3junk", None),
        ("5 , 6", None),
        (" 7", None),
        ("-1", None),
        ("3-0", None),
        ("0-", None),
        ("-", None),
        (",", None),
        ("abc", None),
        ("1-2-3", None),
        ("+5", None),
        ("8192", None), // > MAX_CPU_ID
        ("0-999999999999", None), // would OOM without the value cap
        ("18446744073709551616", None), // > usize::MAX
    ];

    #[test]
    fn parser_matches_c_reference_corpus() {
        for (input, expected) in CORPUS {
            let got = parse_cpu_list(input);
            match expected {
                Some(list) => assert_eq!(got.as_deref(), Some(*list), "input {input:?}"),
                None => assert_eq!(got, None, "input {input:?} should be rejected"),
            }
        }
    }

    #[test]
    fn parser_accepts_full_range_without_blowup() {
        let got = parse_cpu_list("0-8191").expect("maximal legal cpulist");
        assert_eq!(got.len(), 8192);
        assert_eq!(got[0], 0);
        assert_eq!(got[8191], 8191);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn detection_is_consistent() {
        let p = performance_cpus();
        let e = efficiency_cpus();
        assert!(!p.is_empty(), "at least the online CPUs must be reported");
        assert!(p.iter().all(|cpu| !e.contains(cpu)), "P and E sets must be disjoint");
        match topology() {
            Topology::Hybrid => assert!(!e.is_empty(), "hybrid implies a non-empty E-core list"),
            Topology::Homogeneous => assert!(e.is_empty(), "homogeneous implies no E-cores"),
        }
    }

    #[test]
    fn physical_core_leaders_are_idempotent_and_a_subset() {
        let cpus = performance_cpus();
        if cpus.is_empty() {
            return;
        }
        let leaders = physical_core_leaders(&cpus);
        // Every leader was in the input.
        assert!(leaders.iter().all(|c| cpus.contains(c)), "leaders must be a subset");
        // No duplicates.
        let mut sorted = leaders.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), leaders.len(), "leaders must be distinct");
        // Collapsing an already-collapsed set changes nothing.
        assert_eq!(physical_core_leaders(&leaders), leaders, "idempotent");
        // Never more leaders than inputs.
        assert!(leaders.len() <= cpus.len());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn physical_leaders_collapse_smt_siblings() {
        // On an SMT machine the P-core leader count equals the physical
        // P-core count, which is strictly fewer than the logical count.
        // This is only asserted when we can actually see SMT (siblings > 1).
        let cpus = performance_cpus();
        if cpus.len() < 2 {
            return;
        }
        let leaders = physical_core_leaders(&cpus);
        let first_siblings = std::fs::read_to_string(format!(
            "/sys/devices/system/cpu/cpu{}/topology/thread_siblings_list",
            cpus[0]
        ))
        .unwrap_or_default();
        let has_smt = first_siblings.contains(',') || first_siblings.contains('-');
        if has_smt {
            assert!(leaders.len() < cpus.len(), "SMT machine: fewer leaders than logical CPUs");
        } else {
            assert_eq!(leaders.len(), cpus.len(), "no SMT: one leader per CPU");
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn pin_lands_on_requested_cpu() {
        let cpus = performance_cpus();
        let target = *cpus.last().expect("some cpu");
        match pin_current_thread_to_cpu(target) {
            Ok(()) => {
                let running_on = unsafe { libc::sched_getcpu() };
                assert_eq!(running_on as usize, target, "thread should now run on the pinned CPU");
            }
            // A cgroup/cpuset-restricted environment (some CI runners) can
            // legitimately refuse; that's not a port defect.
            Err(e) => eprintln!("pin refused by environment, skipping assert: {e}"),
        }
    }
}

