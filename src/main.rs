use ci_batch_scheduler::{Policy, Workload, simulate};
use std::error::Error;
use std::ffi::OsString;
use std::fs::File;
use std::io::{self, BufReader, BufWriter, Write};
use std::path::PathBuf;
use std::process::ExitCode;

const USAGE: &str = "Usage: ci-batch-scheduler [--policy cost|count] [--stats] INPUT.json\n\
Writes schedule JSON to stdout. Use - to read JSON from stdin.\n\
The default policy minimizes current VM count, then VM-seconds.\n\
--stats writes planner diagnostics to stderr.";

struct Options {
    input: PathBuf,
    policy: Policy,
    stats: bool,
}

fn options(args: impl Iterator<Item = OsString>) -> Result<Option<Options>, String> {
    let mut args = args.peekable();
    let mut input = None;
    let mut policy = Policy::Cost;
    let mut stats = false;
    let mut positional = false;
    while let Some(arg) = args.next() {
        match arg.to_str() {
            Some("--help" | "-h") if !positional => return Ok(None),
            Some("--") if !positional => positional = true,
            Some("--stats") if !positional => stats = true,
            Some("--policy") if !positional => {
                policy = match args.next().as_deref().and_then(|s| s.to_str()) {
                    Some("cost") => Policy::Cost,
                    Some("count") => Policy::Count,
                    _ => return Err("--policy requires cost or count".into()),
                };
            }
            Some(flag) if !positional && flag.starts_with('-') && flag != "-" => {
                return Err(format!("unknown option: {flag}"));
            }
            _ if input.is_none() => input = Some(PathBuf::from(arg)),
            _ => return Err("expected exactly one input path".into()),
        }
    }
    Ok(Some(Options {
        input: input.ok_or("missing input path")?,
        policy,
        stats,
    }))
}

fn run(options: Options) -> Result<(), Box<dyn Error>> {
    let workload: Workload = if options.input.as_os_str() == "-" {
        serde_json::from_reader(io::stdin().lock())?
    } else {
        serde_json::from_reader(BufReader::new(File::open(&options.input)?))?
    };
    let (schedule, stats) = simulate(workload, options.policy)?;
    // Do not emit partial schedule JSON for invalid input or planning failures.
    let mut stdout = BufWriter::new(io::stdout().lock());
    serde_json::to_writer_pretty(&mut stdout, &schedule)?;
    writeln!(stdout)?;
    stdout.flush()?;
    if options.stats {
        serde_json::to_writer(io::stderr().lock(), &stats)?;
        eprintln!();
    }
    Ok(())
}

fn main() -> ExitCode {
    match options(std::env::args_os().skip(1)) {
        Ok(None) => {
            println!("{USAGE}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("{error}\n\n{USAGE}");
            ExitCode::from(2)
        }
        Ok(Some(options)) => match run(options) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("ci-batch-scheduler: {error}");
                ExitCode::FAILURE
            }
        },
    }
}
