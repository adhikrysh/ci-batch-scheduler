//! Launch immutable batches from feasible plans, driven only by arrivals and
//! deadlines. Running VMs need no state here: they cannot accept work, their
//! completion times are fixed, and they never block another VM from starting.

use crate::model::{Arrival, Batch, Costs, Pending, Policy, Schedule, Stats};
use crate::planner::{self, Group};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Default)]
struct Repository {
    pending: BTreeMap<usize, Pending>,
    plan: Vec<Group>,
    wake: Option<u64>,
}

pub(crate) struct Scheduler {
    costs: Costs,
    policy: Policy,
    repositories: BTreeMap<String, Repository>,
    // Exactly one live timer per nonempty repository; no stale heap entries.
    timers: BTreeSet<(u64, String)>,
    launches: Vec<(u64, Vec<usize>)>,
    stats: Stats,
}

impl Scheduler {
    pub fn new(costs: Costs, policy: Policy) -> Self {
        Self {
            costs,
            policy,
            repositories: BTreeMap::new(),
            timers: BTreeSet::new(),
            launches: Vec::new(),
            stats: Stats::default(),
        }
    }

    pub fn next_wake(&self) -> Option<u64> {
        self.timers.first().map(|(time, _)| *time)
    }

    pub fn arrive(&mut self, now: u64, arrivals: Vec<Arrival>) {
        let mut touched = BTreeSet::new();
        for arrival in arrivals {
            debug_assert_eq!(arrival.at, now);
            touched.insert(arrival.repo.clone());
            self.repositories
                .entry(arrival.repo)
                .or_default()
                .pending
                .insert(arrival.job.token, arrival.job);
        }
        // Planning before this loop would expose only part of a timestamp's
        // arrivals and could commit a batch before all eligible jobs are seen.
        for repo in touched {
            self.replan(&repo, now);
        }
    }

    fn replan(&mut self, name: &str, now: u64) {
        let repo = self.repositories.get_mut(name).expect("known repository");
        if let Some(old) = repo.wake.take() {
            self.timers.remove(&(old, name.to_owned()));
        }
        if repo.pending.is_empty() {
            self.repositories.remove(name);
            return;
        }
        repo.plan = planner::plan(
            &repo.pending,
            &repo.plan,
            &self.costs,
            now,
            self.policy,
            &mut self.stats,
        );
        let wake = repo
            .plan
            .iter()
            .map(|group| group.latest(&self.costs))
            .min()
            .expect("nonempty plan");
        assert!(wake >= now, "planner returned an expired group");
        repo.wake = Some(wake);
        self.timers.insert((wake, name.to_owned()));
    }

    pub fn advance(&mut self, limit: u64, inclusive: bool) {
        while self
            .next_wake()
            .is_some_and(|time| time < limit || inclusive && time == limit)
        {
            let (now, name) = self.timers.pop_first().expect("due timer");
            let repo = self
                .repositories
                .get_mut(&name)
                .expect("timer has a repository");
            repo.wake = None;
            let mut due: Vec<_> = repo
                .plan
                .iter()
                .filter(|g| g.latest(&self.costs) == now)
                .cloned()
                .collect();
            due.sort_by(|a, b| a.checks.cmp(&b.checks));
            for group in due {
                // An earlier simultaneous launch may have satisfied members of
                // this group for free. Recompute its actual work and urgency.
                let Some(live) =
                    planner::summarize(group.members.iter().filter_map(|id| repo.pending.get(id)))
                else {
                    continue;
                };
                if live.latest(&self.costs) > now {
                    continue;
                }
                let finish = u128::from(now) + self.costs.duration(&live.checks);
                // These jobs add no checks to this batch. Removing them from
                // other tentative groups can only make those groups easier
                // to finish; no launched batch is being modified.
                let take: Vec<_> = repo
                    .pending
                    .values()
                    .filter(|job| {
                        job.checks.is_subset(&live.checks) && finish <= u128::from(job.deadline)
                    })
                    .map(|job| job.token)
                    .collect();
                assert!(!take.is_empty(), "a due batch must make progress");
                for token in &take {
                    repo.pending.remove(token);
                }
                self.launches.push((now, take));
            }
            self.replan(&name, now);
        }
    }

    pub fn output(self, labels: &[String]) -> (Schedule, Stats) {
        debug_assert!(self.repositories.is_empty());
        let batches = self
            .launches
            .into_iter()
            .map(|(launched_at_s, tokens)| Batch {
                launched_at_s,
                job_ids: tokens
                    .into_iter()
                    .map(|token| labels[token].clone())
                    .collect(),
            })
            .collect();
        (Schedule { batches }, self.stats)
    }
}
