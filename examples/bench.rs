//! In-process timing. Input parsing and cloning are outside the measured region.
use ci_batch_scheduler::{Policy, Workload, simulate};
use std::error::Error;
use std::fs::File;
use std::io::BufReader;
use std::time::Instant;

fn main() -> Result<(), Box<dyn Error>> {
    let path = std::env::args_os()
        .nth(1)
        .ok_or("usage: cargo run --release --example bench -- INPUT.json")?;
    let workload: Workload = serde_json::from_reader(BufReader::new(File::open(path)?))?;
    let mut times = Vec::new();
    let mut batches = 0;
    for _ in 0..100 {
        let input = workload.clone();
        let start = Instant::now();
        let (schedule, _) = simulate(input, Policy::Cost)?;
        times.push(start.elapsed().as_secs_f64() * 1000.0);
        batches = schedule.batches.len();
        std::hint::black_box(schedule);
    }
    times.sort_by(f64::total_cmp);
    println!(
        "{}",
        serde_json::json!({"runs": times.len(), "jobs": workload.jobs.len(), "batches": batches,
        "median_ms": times[times.len()/2], "p95_ms": times[times.len()*95/100],
        "scope": "In-process validation and simulation. Excludes JSON parsing, input cloning, process startup, and output serialization."})
    );
    Ok(())
}
