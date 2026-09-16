//! Maintain a feasible partition of one repository's unlaunched jobs.
//!
//! Existing assignments are an incumbent, not a commitment. Replanning may
//! replace them only with feasible groups that strictly improve the policy's
//! score. Launched jobs never enter this module again.

use crate::model::{Checks, Costs, Pending, Policy, Stats};
use std::cmp::Reverse;
use std::collections::{BTreeMap, BTreeSet};

// Subset partitioning grows as 3^k. Keep it confined to small pattern counts;
// larger inputs still get a valid plan and bounded improvement in search.rs.
// This is a resource tradeoff, not a correctness requirement or workload rule.
const EXACT_PATTERN_LIMIT: usize = 12;

#[derive(Clone, Debug)]
pub(crate) struct Group {
    pub checks: Checks,
    pub deadline: u64,
    pub members: Vec<usize>,
}

impl Group {
    pub fn latest(&self, costs: &Costs) -> u64 {
        costs
            .latest(&self.checks, self.deadline)
            .expect("feasible planned group")
    }

    pub fn merge(&self, other: &Self) -> Self {
        let mut members = self.members.clone();
        members.extend_from_slice(&other.members);
        Self {
            checks: self.checks.union(&other.checks),
            deadline: self.deadline.min(other.deadline),
            members,
        }
    }
}

pub(crate) fn summarize<'a>(jobs: impl Iterator<Item = &'a Pending>) -> Option<Group> {
    let mut group: Option<Group> = None;
    for job in jobs {
        if let Some(group) = &mut group {
            group.checks = group.checks.union(&job.checks);
            group.deadline = group.deadline.min(job.deadline);
            group.members.push(job.token);
        } else {
            group = Some(Group {
                checks: job.checks.clone(),
                deadline: job.deadline,
                members: vec![job.token],
            });
        }
    }
    group
}

// For the current queue, identical check sets can travel together for free.
// Put every copy with the earliest-deadline copy: no new work is added there,
// later copies remain on time, and removing copies elsewhere cannot add cost.
fn patterns<'a>(jobs: impl Iterator<Item = &'a Pending>) -> Vec<Group> {
    let mut by_checks: BTreeMap<Checks, Vec<&Pending>> = BTreeMap::new();
    for job in jobs {
        by_checks.entry(job.checks.clone()).or_default().push(job);
    }
    by_checks
        .into_values()
        .map(|jobs| summarize(jobs.into_iter()).expect("nonempty pattern"))
        .collect()
}

pub(crate) fn score(groups: &[Group], costs: &Costs) -> (usize, u128) {
    (
        groups.len(),
        groups
            .iter()
            .map(|group| costs.duration(&group.checks))
            .sum(),
    )
}

/// Insert by earliest individual latest-launch time, then cheapest feasible fit.
fn insert(groups: &mut Vec<Group>, incoming: Vec<Group>, costs: &Costs, now: u64) {
    let mut incoming = incoming;
    incoming.sort_by(|a, b| (a.latest(costs), &a.checks).cmp(&(b.latest(costs), &b.checks)));
    for job in incoming {
        let best = groups
            .iter()
            .enumerate()
            .filter_map(|(i, group)| {
                let union = group.checks.union(&job.checks);
                let latest = costs.latest(&union, group.deadline.min(job.deadline))?;
                (latest >= now).then(|| {
                    (
                        costs.duration(&union) - costs.duration(&group.checks),
                        group.latest(costs),
                        group.checks.clone(),
                        i,
                    )
                })
            })
            .min();
        if let Some((_, _, _, i)) = best {
            groups[i] = groups[i].merge(&job);
        } else {
            groups.push(job);
        }
    }
}

/// Lower bounds on any feasible partition of these already-arrived patterns.
pub(crate) fn lower_bound(patterns: &[Group], costs: &Costs, now: u64) -> (usize, u128) {
    let union = patterns
        .iter()
        .fold(Checks::default(), |a, p| a.union(&p.checks));
    let work = costs.work(&union);
    let max_deadline = patterns
        .iter()
        .map(|p| p.deadline)
        .max()
        .expect("nonempty patterns");
    // Every pattern is still feasible alone. Use the most generous deadline:
    // this overestimates each VM's capacity, so the resulting bound stays safe.
    let capacity = max_deadline - now - costs.boot;
    let mut count = if capacity == 0 {
        1
    } else {
        work.div_ceil(u128::from(capacity)).max(1) as usize
    };
    let mut order: Vec<_> = (0..patterns.len()).collect();
    order.sort_by(|&a, &b| {
        (patterns[a].latest(costs), &patterns[a].checks)
            .cmp(&(patterns[b].latest(costs), &patterns[b].checks))
    });
    // A set of mutually incompatible patterns needs one VM per pattern. A
    // greedy set need not be maximal to provide a valid lower bound.
    let mut witnesses: Vec<usize> = Vec::new();
    for i in order {
        if witnesses.iter().all(|&j| {
            let union = patterns[i].checks.union(&patterns[j].checks);
            costs
                .latest(&union, patterns[i].deadline.min(patterns[j].deadline))
                .is_none_or(|latest| latest < now)
        }) {
            witnesses.push(i);
        }
    }
    count = count.max(witnesses.len());
    (count, count as u128 * u128::from(costs.boot) + work)
}

pub(crate) fn plan(
    pending: &BTreeMap<usize, Pending>,
    previous: &[Group],
    costs: &Costs,
    now: u64,
    policy: Policy,
    stats: &mut Stats,
) -> Vec<Group> {
    stats.planning_calls += 1;
    // Retain assignments for jobs that were not absorbed by an earlier launch.
    // Removing members only shrinks work and relaxes the group's deadline.
    let mut assigned = BTreeSet::new();
    let mut stable = Vec::new();
    for old in previous {
        if let Some(group) = summarize(old.members.iter().filter_map(|id| pending.get(id))) {
            assigned.extend(group.members.iter().copied());
            stable.push(group);
        }
    }
    let incoming = patterns(
        pending
            .values()
            .filter(|job| !assigned.contains(&job.token)),
    );
    insert(&mut stable, incoming, costs, now);
    let all = patterns(pending.values());
    stats.peak_pending_patterns = stats.peak_pending_patterns.max(all.len());
    if stable.len() <= 1 {
        return stable;
    }
    let old_score = score(&stable, costs);
    let bound = lower_bound(&all, costs, now);
    if old_score == bound || (policy == Policy::Count && old_score.0 == bound.0) {
        return stable;
    }
    let everything = all
        .iter()
        .cloned()
        .reduce(|a, b| a.merge(&b))
        .expect("pending jobs");
    let alternative = if costs
        .latest(&everything.checks, everything.deadline)
        .is_some_and(|latest| latest >= now)
    {
        vec![everything]
    } else if all.len() <= EXACT_PATTERN_LIMIT {
        stats.exact_searches += 1;
        exact_partition(&all, costs, now)
    } else {
        stats.bounded_searches += 1;
        let mut greedy = Vec::new();
        insert(&mut greedy, all.clone(), costs, now);
        crate::search::improve(&all, costs, now, greedy, stats)
    };
    let new_score = score(&alternative, costs);
    // Equal-cost rearrangements have no demonstrated benefit and can change
    // future launch opportunities. Keep the incumbent rather than churning.
    let better = match policy {
        Policy::Cost => new_score < old_score,
        Policy::Count => new_score.0 < old_score.0,
    };
    if better { alternative } else { stable }
}

#[derive(Clone)]
struct Solution {
    groups: Vec<usize>,
    seconds: u128,
    first_launch: u64,
}

fn exact_partition(patterns: &[Group], costs: &Costs, now: u64) -> Vec<Group> {
    let size = 1 << patterns.len();
    let mut unions = vec![Checks::default(); size];
    let mut deadlines = vec![u64::MAX; size];
    let mut durations = vec![0; size];
    let mut latest = vec![None; size];
    for subset in 1..size {
        let index = subset.trailing_zeros() as usize;
        let rest = subset & (subset - 1);
        unions[subset] = unions[rest].union(&patterns[index].checks);
        deadlines[subset] = deadlines[rest].min(patterns[index].deadline);
        durations[subset] = costs.duration(&unions[subset]);
        latest[subset] = costs
            .latest(&unions[subset], deadlines[subset])
            .filter(|&t| t >= now);
    }
    let mut best: Vec<Option<Solution>> = vec![None; size];
    best[0] = Some(Solution {
        groups: Vec::new(),
        seconds: 0,
        first_launch: u64::MAX,
    });
    for remaining in 1..size {
        // Every partition has exactly one group containing this anchor. Only
        // consider those subsets, avoiding permutations of the same partition.
        // Removing a nonempty subset also makes the DP dependency smaller.
        let anchor = 1 << remaining.trailing_zeros();
        let mut subset = remaining;
        while subset != 0 {
            if subset & anchor != 0
                && let Some(launch) = latest[subset]
                && let Some(tail) = &best[remaining ^ subset]
            {
                // After count and seconds tie, prefer more time before the
                // first launch. This is a deterministic opportunity-preserving
                // tie-break, not a guarantee about unseen arrivals.
                let rank = (
                    tail.groups.len() + 1,
                    tail.seconds + durations[subset],
                    Reverse(tail.first_launch.min(launch)),
                );
                let incumbent = best[remaining]
                    .as_ref()
                    .map(|b| (b.groups.len(), b.seconds, Reverse(b.first_launch)));
                if incumbent.is_none_or(|old| rank <= old) {
                    let mut groups = tail.groups.clone();
                    groups.push(subset);
                    groups.sort_unstable();
                    if incumbent.is_none_or(|old| rank < old)
                        || best[remaining]
                            .as_ref()
                            .is_some_and(|old| groups < old.groups)
                    {
                        best[remaining] = Some(Solution {
                            groups,
                            seconds: rank.1,
                            first_launch: rank.2.0,
                        });
                    }
                }
            }
            subset = (subset - 1) & remaining;
        }
    }
    best[size - 1]
        .take()
        .expect("each job is feasible alone")
        .groups
        .into_iter()
        .map(|subset| {
            let members = patterns
                .iter()
                .enumerate()
                .filter(|(i, _)| subset & (1 << i) != 0)
                .flat_map(|(_, pattern)| pattern.members.iter().copied())
                .collect();
            Group {
                checks: unions[subset].clone(),
                deadline: deadlines[subset],
                members,
            }
        })
        .collect()
}
