//! Larger waiting pools get a feasible plan first, then bounded improvement.
//! Exhausting the budget is a loss of optimality, never a loss of feasibility.

use crate::model::{Checks, Costs, Stats};
use crate::planner::{Group, lower_bound, score};
use std::cmp::Reverse;
use std::collections::{BTreeMap, HashSet};

// Count search nodes instead of elapsed wall time so identical inputs produce
// identical decisions on different machines. The pattern cap also bounds
// recursion depth. Neither limit bounds the driver's total input memory.
const NODE_BUDGET: usize = 30_000;
const PATTERN_LIMIT: usize = 64;

#[derive(Clone)]
struct Bin {
    checks: Checks,
    deadline: u64,
    patterns: Vec<usize>,
}

type State = (usize, Vec<(Checks, u64)>);

struct Search<'a> {
    patterns: &'a [Group],
    costs: &'a Costs,
    now: u64,
    order: Vec<usize>,
    best: Vec<Vec<usize>>,
    best_score: (usize, u128),
    bound: (usize, u128),
    total_work: u128,
    nodes: usize,
    exhausted: bool,
    seen: HashSet<State>,
}

impl Search<'_> {
    fn visit(&mut self, position: usize, bins: &mut Vec<Bin>, seconds: u128, covered: &Checks) {
        if self.best_score == self.bound {
            return;
        }
        if self.nodes >= NODE_BUDGET {
            self.exhausted = true;
            return;
        }
        self.nodes += 1;
        let possible_count = self.bound.0.max(bins.len());
        // Every uncovered check must be paid for at least once. Already-open
        // bins cannot disappear in this branch, and reaching the VM lower
        // bound may require more startup charges. This deliberately ignores
        // duplicate future execution, keeping the cost estimate optimistic.
        let missing_work = self.total_work - self.costs.work(covered);
        let additional_starts = self.bound.0.saturating_sub(bins.len());
        let seconds_bound =
            seconds + missing_work + additional_starts as u128 * u128::from(self.costs.boot);
        if possible_count > self.best_score.0
            || (possible_count == self.best_score.0 && seconds_bound >= self.best_score.1)
        {
            return;
        }
        if position == self.order.len() {
            let candidate = (bins.len(), seconds);
            if candidate < self.best_score {
                self.best_score = candidate;
                self.best = bins.iter().map(|bin| bin.patterns.clone()).collect();
            }
            return;
        }
        // At a fixed position the unassigned patterns are identical. Future
        // choices depend only on each bin's check union and oldest deadline,
        // not its member identities or order. Such states have equal cost.
        let mut canonical: Vec<_> = bins
            .iter()
            .map(|bin| (bin.checks.clone(), bin.deadline))
            .collect();
        canonical.sort_unstable();
        if !self.seen.insert((position, canonical)) {
            return;
        }
        let pattern = self.order[position];
        let checks = self.patterns[pattern].checks.clone();
        let deadline = self.patterns[pattern].deadline;
        let next_covered = covered.union(&checks);
        let mut equivalent = HashSet::new();
        let mut fits = Vec::new();
        for (i, bin) in bins.iter().enumerate() {
            if !equivalent.insert((bin.checks.clone(), bin.deadline)) {
                continue;
            }
            let union = bin.checks.union(&checks);
            let oldest = bin.deadline.min(deadline);
            if self
                .costs
                .latest(&union, oldest)
                .is_some_and(|t| t >= self.now)
            {
                let extra = self.costs.duration(&union) - self.costs.duration(&bin.checks);
                fits.push((extra, i, union, oldest));
            }
        }
        fits.sort_unstable();
        for (extra, index, union, oldest) in fits {
            let old = bins[index].clone();
            bins[index].checks = union;
            bins[index].deadline = oldest;
            bins[index].patterns.push(pattern);
            self.visit(position + 1, bins, seconds + extra, &next_covered);
            bins[index] = old;
            if self.exhausted || self.best_score == self.bound {
                return;
            }
        }
        if bins.len() < self.best_score.0 {
            let duration = self.costs.duration(&checks);
            bins.push(Bin {
                checks,
                deadline,
                patterns: vec![pattern],
            });
            self.visit(position + 1, bins, seconds + duration, &next_covered);
            bins.pop();
        }
    }
}

pub(crate) fn improve(
    patterns: &[Group],
    costs: &Costs,
    now: u64,
    initial: Vec<Group>,
    stats: &mut Stats,
) -> Vec<Group> {
    let bound = lower_bound(patterns, costs, now);
    let best_score = score(&initial, costs);
    if best_score == bound {
        return initial;
    }
    if patterns.len() > PATTERN_LIMIT {
        stats.search_budget_exhaustions += 1;
        return initial;
    }
    let mut owners = BTreeMap::new();
    for (i, pattern) in patterns.iter().enumerate() {
        for token in &pattern.members {
            owners.insert(*token, i);
        }
    }
    let best = initial
        .iter()
        .map(|group| {
            let mut indexes: Vec<_> = group.members.iter().map(|token| owners[token]).collect();
            indexes.sort_unstable();
            indexes.dedup();
            indexes
        })
        .collect();
    let mut order: Vec<_> = (0..patterns.len()).collect();
    order.sort_by(|&a, &b| {
        (
            patterns[a].latest(costs),
            Reverse(costs.duration(&patterns[a].checks)),
            &patterns[a].checks,
        )
            .cmp(&(
                patterns[b].latest(costs),
                Reverse(costs.duration(&patterns[b].checks)),
                &patterns[b].checks,
            ))
    });
    let union = patterns
        .iter()
        .fold(Checks::default(), |a, p| a.union(&p.checks));
    let mut search = Search {
        patterns,
        costs,
        now,
        order,
        best,
        best_score,
        bound,
        total_work: costs.work(&union),
        nodes: 0,
        exhausted: false,
        seen: HashSet::new(),
    };
    search.visit(0, &mut Vec::new(), 0, &Checks::default());
    stats.search_nodes += search.nodes;
    stats.search_budget_exhaustions += usize::from(search.exhausted);
    search
        .best
        .into_iter()
        .map(|indexes| {
            indexes
                .into_iter()
                .map(|i| patterns[i].clone())
                .reduce(|a, b| a.merge(&b))
                .expect("nonempty search group")
        })
        .collect()
}
