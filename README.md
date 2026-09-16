# CI batch scheduler

This simulator groups CI jobs from the same repository onto fewer VMs. Jobs in a batch share startup and run each distinct check once. Every job must finish within its deadline.

The Rust implementation produces **46 VMs, 1,728 VM-seconds, and no missed deadlines** on the supplied workload.

## Run

Requires Rust 1.88 or newer. Keep the supplied workload and evaluator outside this repo.

```sh
cargo build --release --locked
./target/release/ci-batch-scheduler /path/to/workload.json > /tmp/schedule.json
python3 /path/to/evaluate.py /path/to/workload.json /tmp/schedule.json
```

To try the included example:

```sh
cargo run --release --locked -- examples/regroup.json
```

The program writes schedule JSON to stdout. Add `--stats` for planning counters on stderr, or use `-` as the input path to read stdin.

## How it works

Each repository has a pool of waiting jobs. When jobs arrive, the scheduler tries adding them to the planned batches. It then looks for a better grouping: fewer VMs first, less total execution time second. It keeps the existing plan when the scores tie.

A batch's duration is startup plus the time needed to run its distinct checks. Its earliest job deadline tells us how long we can wait. If a batch needs 40 seconds and must finish by time 60, it must launch by time 20.

Until it launches, we can change its membership. That matters because a later arrival may make a different grouping cheaper.

### An example

Startup takes 10 seconds. Checks `x`, `y`, and `z` take 20, 10, and 10 seconds. Each job has 60 seconds to finish.

| Job | Arrives at | Needs | Must finish by |
| --- | ---: | --- | ---: |
| A | 0 | x | 60 |
| B | 0 | y | 60 |
| C | 20 | y, z | 80 |

At time zero, A and B can share a VM. Their batch needs 40 seconds, so we plan to launch at time 20.

C arrives at time 20, before we launch. All three jobs together would take 50 seconds and finish at time 70, too late for A and B.

We could keep A and B together and give C another VM. That costs 40 + 30 = 70 VM-seconds.

Instead, move B into C's batch. A alone takes 30 seconds. B and C together also take 30 seconds, because they share `y`. Both batches can launch at time 30 and finish at time 60. We still use two VMs, but pay for 60 seconds instead of 70.

After a launch, the scheduler removes those jobs and replans the remainder. It can launch several VMs at once. A launched batch cannot change.

## Results

| Policy | VMs | VM-seconds | Missed deadlines |
| --- | ---: | ---: | ---: |
| Launch every job immediately | 154 | 4,484 | 0 |
| Regroup only when it saves a VM | 46 | 1,740 | 0 |
| Also regroup when it saves execution time, the default | 46 | 1,728 | 0 |

Run `--policy count` to use the second policy.

The 46-VM result is minimal for this input: a separate check found 46 jobs that cannot share batches with one another. The 1,728-second result is not proved minimal. A calculation allowed to see future arrivals reached 1,692 seconds; the scheduler cannot use that information.

The default also does not win on every workload. Across 512 additional synthetic streams, the count policy used 39 fewer VMs overall, while the default saved 3,795 VM-seconds. Both met every deadline. I kept the default for its supplied result and retained the other policy for comparison. Choosing between them for a service would need representative traffic.

## Tests

```sh
cargo fmt --all -- --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
```

The tests compare 500 small cases with an independent exhaustive solver and check both policies on 256 generated streams. They also cover simultaneous arrivals, deadlines, invalid input, search limits, and whether changing future jobs or job IDs changes past decisions.

The [design notes](docs/design.md) explain the search limits, alternatives, performance measurements, and why VM cancellation is not implemented. The supplied materials and private evaluation inputs are not included in this repo.
