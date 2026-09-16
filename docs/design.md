# Design notes

## What I tried

Starting every job immediately gives a useful baseline: 154 VMs and 4,484 VM-seconds. It meets the deadlines but repeats startup and shared checks.

My next idea was to queue jobs by repository and keep adding them until there was no time left. The problem is that the whole queue may not fit one VM. Several smaller batches can finish in parallel, and jobs may fit better in a different batch from the one they first joined.

Waiting until an individual job must launch has another weakness. By then, there may be too little time to add a useful check from another job. I therefore calculate the launch deadline for each proposed batch, including all the work it would do.

This led to two policies. Both keep their planned groups until rearranging them saves a VM. The default also rearranges them when it saves execution time without adding a VM. The README's example shows that second step.

On the supplied input, it saves 12 seconds. Across 512 other synthetic streams, the results were mixed:

| Policy | Total VMs | Total VM-seconds |
| --- | ---: | ---: |
| Regroup to save VMs | 31,429 | 1,300,351 |
| Regroup to save VMs or execution time | 31,468 | 1,296,556 |

Neither policy missed a deadline. Under the assignment's VM-first scoring, the first policy wins this aggregate comparison. Rearranging today's jobs can save work now and leave a worse match for a later arrival. I cannot promise the best result for every possible future.

I also tried restrictions on which jobs could join groups and predictions based on earlier arrivals. They did not justify their added complexity in the experiments. The final scheduler uses the jobs already waiting and the supplied check durations.

## Keeping deadlines

The scheduler always keeps a plan in which every waiting job can finish on time. A newly arrived job can run alone if it fits nowhere else. A proposed merge or replacement plan must pass the same deadline check.

For each group, the latest launch is its earliest deadline minus its duration. The next timer is the earliest of those launch times. All arrivals at that timestamp are processed before the timer's launch decision.

When a batch launches, it can take additional waiting jobs whose checks it already covers, provided they can finish on time. Removing those jobs from other groups can only reduce their work or relax their deadline. The scheduler recalculates those groups before launching them.

Jobs leave the waiting pool exactly once. At the end of the file, the driver keeps delivering timers rather than launching everything early.

This relies on the assignment's exact durations and unlimited concurrent VMs. A real service would need room for runtime variation, retries, and capacity limits.

## Keeping search small

Jobs with identical check sets share one entry during search. The entry keeps their earliest deadline. Running later copies with the earliest copy adds no work and meets their deadlines too.

For up to 12 distinct check sets, the planner searches every possible partition. This grows roughly as `3^k`, where `k` is the number of distinct sets, with a table of `2^k` entries. Set operations and stored group lists add further work and memory.

Larger queues start with a valid plan and get at most 30,000 search steps to improve it. Above 64 distinct sets, the planner uses the heuristic plan without that search. A 13-set test case showed why stopping at a greedy plan could waste VMs; the extra search fixes that case.

Sometimes a cheap calculation proves no improvement is possible. For example, if five jobs cannot share a batch with one another, we need at least five VMs. If our plan uses five and executes every distinct check only once, it has reached both the VM and execution-cost minimum for the current queue. We can skip the search.

The limits of 12, 30,000, and 64 are engineering choices. They keep search manageable while preserving a valid schedule. They do not impose a total memory limit or guarantee a particular response time for arbitrarily large input.

## Code and measurements

[lib.rs](../src/lib.rs) owns the arrival stream and reveals one timestamp at a time. [scheduler.rs](../src/scheduler.rs) owns waiting jobs, timers, and launches. [planner.rs](../src/planner.rs) and [search.rs](../src/search.rs) choose groups. The planner never receives future jobs or an end-of-file signal. Job IDs are labels used to produce the output.

The simulation jumps between events, so a long idle gap adds no per-second work. On an Apple M2, 100 release-mode runs of the supplied input took a median of 0.68 ms and a 95th percentile of 1.00 ms. That includes validation and simulation, but excludes JSON parsing, input cloning, serialization, and process startup. To measure another input:

```sh
cargo run --release --locked --example bench -- /path/to/workload.json
```

The external evaluator checked the supplied result and both policies on the 512 additional streams, with 61,440 jobs per policy. Those development inputs are not published here. The public tests have their own generated workloads and validate results independently of the planner.

## Cancelling a VM

I considered stopping a VM during startup so its jobs could join a better batch. The evaluator cannot represent this: once a batch launches, it assigns a fixed completion time. Implementing cancellation would require changing that model and accounting for every job in the stopped batch.

It could save money in a real system, but only if the avoided charges exceed the cost of the replacement work. Cancelling does not refund time already billed.

AWS documents EC2 `pending` time as unbilled and `running` time as billed. Standard per-second On-Demand billing has a 60-second minimum. The CI runner can still be starting after EC2 enters `running`. Storage may be charged separately. See [EC2 states](https://docs.aws.amazon.com/AWSEC2/latest/UserGuide/ec2-instance-lifecycle.html) and [pricing](https://aws.amazon.com/ec2/pricing/on-demand/).

Google Compute Engine also has a one-minute minimum for vCPU, GPU, and memory usage. Azure bills starting VMs and stopped VMs that are still allocated; releasing that capacity matters. See [Google pricing](https://cloud.google.com/products/compute/pricing) and [Azure billing states](https://learn.microsoft.com/en-us/azure/virtual-machines/states-billing).

For example, stopping a VM after 20 billed seconds may still cost 60 seconds. If it would otherwise finish within that minute, cancellation saves no instance charge. If it would run for five minutes, stopping can avoid later charges, but any replacement must be included in the comparison.

I would test this against simply waiting before launch. Both approaches must return every job's results on time.

## With more time

I would first test on representative unseen traffic and profile large queues. The broader comparison already shows that improving one workload can hide a worse result elsewhere.

Warm VMs could spread startup cost across batches, at the price of paying for idle machines. Saving results between batches could avoid more duplicate work, but would require reliable check identity and cache isolation. Running checks in parallel could reduce latency, but would change the resource model. None of these behaviours is available in the current evaluator.

For a service, I would also need durable launch records, safe retries after failures, VM quotas, and a margin for checks that run longer than expected. This simulator assumes every check succeeds in its stated time.
