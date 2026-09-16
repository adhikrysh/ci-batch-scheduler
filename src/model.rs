use serde::{Deserialize, Serialize};
use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Workload {
    pub boot_s: u64,
    pub sla_s: u64,
    pub check_costs_s: BTreeMap<String, u64>,
    pub jobs: Vec<Job>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Job {
    pub job_id: String,
    pub repo: String,
    pub arrived_at_s: u64,
    pub checks: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Schedule {
    pub batches: Vec<Batch>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Batch {
    pub launched_at_s: u64,
    pub job_ids: Vec<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Policy {
    /// Improve current batch count, then execution cost. Keep ties stable.
    #[default]
    Cost,
    /// Rearrange only to reduce current batch count; useful as a baseline.
    Count,
}

#[derive(Debug, Default, Serialize)]
pub struct Stats {
    pub planning_calls: usize,
    pub exact_searches: usize,
    pub bounded_searches: usize,
    pub search_nodes: usize,
    /// Includes node-budget exhaustion and skipping search above the pattern cap.
    pub search_budget_exhaustions: usize,
    pub peak_pending_patterns: usize,
}

#[derive(Debug)]
pub struct InputError(pub(crate) String);

impl fmt::Display for InputError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for InputError {}

/// Sorted, distinct check indexes. Sparse storage avoids a fixed check limit.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub(crate) struct Checks(pub Vec<usize>);

impl Ord for Checks {
    fn cmp(&self, other: &Self) -> Ordering {
        // Numeric-bitset order, without allocating a bit for every catalog entry.
        self.0.iter().rev().cmp(other.0.iter().rev())
    }
}

impl PartialOrd for Checks {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Checks {
    pub fn union(&self, other: &Self) -> Self {
        let (mut a, mut b) = (0, 0);
        let mut result = Vec::with_capacity(self.0.len() + other.0.len());
        while a < self.0.len() || b < other.0.len() {
            match (self.0.get(a), other.0.get(b)) {
                (Some(x), Some(y)) if x == y => {
                    result.push(*x);
                    a += 1;
                    b += 1;
                }
                (Some(x), Some(y)) if x < y => {
                    result.push(*x);
                    a += 1;
                }
                (Some(_), Some(y)) | (None, Some(y)) => {
                    result.push(*y);
                    b += 1;
                }
                (Some(x), None) => {
                    result.push(*x);
                    a += 1;
                }
                (None, None) => break,
            }
        }
        Self(result)
    }

    pub fn is_subset(&self, other: &Self) -> bool {
        let mut cursor = 0;
        for check in &self.0 {
            while cursor < other.0.len() && other.0[cursor] < *check {
                cursor += 1;
            }
            if other.0.get(cursor) != Some(check) {
                return false;
            }
        }
        true
    }
}

pub(crate) struct Costs {
    pub boot: u64,
    pub checks: Vec<u64>,
}

impl Costs {
    pub fn work(&self, checks: &Checks) -> u128 {
        // Individual durations fit u64, but their union need not. Widen before
        // summing so an impossible group is rejected rather than wrapped.
        checks.0.iter().map(|&i| u128::from(self.checks[i])).sum()
    }

    pub fn duration(&self, checks: &Checks) -> u128 {
        u128::from(self.boot) + self.work(checks)
    }

    pub fn latest(&self, checks: &Checks, deadline: u64) -> Option<u64> {
        // An infeasible union may be larger than u64; never truncate it.
        u128::from(deadline)
            .checked_sub(self.duration(checks))
            .map(|time| time as u64)
    }
}

#[derive(Debug, Clone)]
pub(crate) struct Pending {
    pub token: usize,
    pub deadline: u64,
    pub checks: Checks,
}

pub(crate) struct Arrival {
    pub at: u64,
    pub repo: String,
    pub job: Pending,
}

pub(crate) struct Prepared {
    pub costs: Costs,
    pub labels: Vec<String>,
    pub arrivals: Vec<Arrival>,
}

impl Workload {
    pub(crate) fn prepare(self) -> Result<Prepared, InputError> {
        let indexes: BTreeMap<_, _> = self
            .check_costs_s
            .keys()
            .enumerate()
            .map(|(i, k)| (k, i))
            .collect();
        let costs = Costs {
            boot: self.boot_s,
            checks: self.check_costs_s.values().copied().collect(),
        };
        let mut labels = Vec::with_capacity(self.jobs.len());
        let mut arrivals = Vec::with_capacity(self.jobs.len());
        let mut seen = BTreeSet::new();
        for (token, job) in self.jobs.into_iter().enumerate() {
            if !seen.insert(job.job_id.clone()) {
                return Err(InputError(format!("duplicate job ID: {:?}", job.job_id)));
            }
            let mut checks = BTreeSet::new();
            for check in &job.checks {
                let index = indexes.get(check).ok_or_else(|| {
                    InputError(format!(
                        "job {:?} references unknown check {check:?}",
                        job.job_id
                    ))
                })?;
                checks.insert(*index);
            }
            let checks = Checks(checks.into_iter().collect());
            if costs.duration(&checks) > u128::from(self.sla_s) {
                return Err(InputError(format!(
                    "job {:?} cannot meet its deadline even when launched immediately",
                    job.job_id
                )));
            }
            let deadline = job.arrived_at_s.checked_add(self.sla_s).ok_or_else(|| {
                InputError(format!("deadline overflows u64 for job {:?}", job.job_id))
            })?;
            labels.push(job.job_id);
            arrivals.push(Arrival {
                at: job.arrived_at_s,
                repo: job.repo,
                job: Pending {
                    token,
                    deadline,
                    checks,
                },
            });
        }
        arrivals.sort_by_key(|arrival| arrival.at);
        Ok(Prepared {
            costs,
            labels,
            arrivals,
        })
    }
}
