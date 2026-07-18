use pcore_blake3::{PcoreHasher, Topology};
use std::path::PathBuf;
use std::process::ExitCode;

fn print_usage(prog: &str) {
    eprintln!("Usage: {prog} [--info] [--physical | --all-physical] <file>...");
    eprintln!("  Hashes each file with BLAKE3, using this machine's performance cores");
    eprintln!("  and an optimal thread split. Prints \"<hex-digest>  <path>\" per file.");
    eprintln!("  --info          print detected CPU topology and thread split, then exit");
    eprintln!("  --physical      one thread per physical P-core (collapse SMT siblings)");
    eprintln!("  --all-physical  one thread per physical core incl. E-cores (max throughput)");
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    let prog = args.first().map(String::as_str).unwrap_or("pcore-blake3");

    // Collect flags (order-independent) and leave the rest as paths.
    let mut physical = false;
    let mut all_physical = false;
    let mut info = false;
    let mut paths: Vec<PathBuf> = Vec::new();
    for arg in &args[1..] {
        match arg.as_str() {
            "--physical" => physical = true,
            "--all-physical" => all_physical = true,
            "--info" => info = true,
            _ => paths.push(PathBuf::from(arg)),
        }
    }

    let hasher = if all_physical {
        PcoreHasher::new_all_physical()
    } else if physical {
        PcoreHasher::new_physical()
    } else {
        PcoreHasher::new()
    };

    if info {
        print_info(&hasher);
        return ExitCode::SUCCESS;
    }

    if paths.is_empty() {
        print_usage(prog);
        return ExitCode::FAILURE;
    }

    let results = hasher.hash_files(&paths);

    let mut ok = true;
    for (path, result) in paths.iter().zip(results) {
        match result {
            Ok(hash) => println!("{}  {}", hash.to_hex(), path.display()),
            Err(e) => {
                eprintln!("{}: {e}", path.display());
                ok = false;
            }
        }
    }

    if ok {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

fn print_info(hasher: &PcoreHasher) {
    let topology = pcore_blake3::topology();
    let p_cpus = pcore_blake3::performance_cpus();
    let p_phys = pcore_blake3::performance_physical_cpus();
    let e_cpus = pcore_blake3::efficiency_cpus();
    let (tpf, cf) = hasher.split();

    let all_phys = pcore_blake3::all_physical_cpus();
    println!("Topology: {}", if topology == Topology::Hybrid { "hybrid" } else { "homogeneous" });
    println!("Performance cores: {p_cpus:?} ({} threads, {} physical)", p_cpus.len(), p_phys.len());
    println!("Efficiency cores: {e_cpus:?} ({} threads)", e_cpus.len());
    println!("All physical cores (P+E): {} (for --all-physical)", all_phys.len());
    println!("Thread split: {tpf} threads/file x {cf} concurrent files");
}
