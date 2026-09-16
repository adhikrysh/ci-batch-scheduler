//! Deterministic simulation of an online, deadline-constrained batch scheduler.
//! File parsing and future arrivals belong to the driver, not the scheduler.

mod model;
mod planner;
mod scheduler;
mod search;

pub use model::{Batch, InputError, Job, Policy, Schedule, Stats, Workload};

/// Validate a workload and schedule it without exposing future jobs to the policy.
///
/// Simulation uses integer time, exact check durations, and unlimited parallel
/// VMs. Every accepted job can finish on its own when launched on arrival.
/// Returns an error before producing a schedule if that condition or the input
/// identity and duration constraints are violated.
pub fn simulate(workload: Workload, policy: Policy) -> Result<(Schedule, Stats), InputError> {
    let prepared = workload.prepare()?;
    let mut scheduler = scheduler::Scheduler::new(prepared.costs, policy);
    let mut stream = prepared.arrivals.into_iter().peekable();
    while let Some(next) = stream.peek() {
        let now = next.at;
        // Timers before this arrival must fire first. Timers at exactly `now`
        // must wait until every arrival at this timestamp has been revealed.
        scheduler.advance(now, false);
        let mut arrived = Vec::new();
        while stream.peek().is_some_and(|next| next.at == now) {
            arrived.push(stream.next().expect("peeked arrival"));
        }
        scheduler.arrive(now, arrived);
        scheduler.advance(now, true);
    }
    // EOF is a driver concern. Finish by delivering the already registered
    // timers, exactly as if the next arrival were far in the future.
    while let Some(now) = scheduler.next_wake() {
        scheduler.advance(now, true);
    }
    Ok(scheduler.output(&prepared.labels))
}
