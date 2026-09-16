use ci_batch_scheduler::{Job, Policy, Schedule, Workload, simulate};
use std::collections::{BTreeMap, BTreeSet};

fn fixture() -> Workload {
    serde_json::from_str(include_str!("../examples/regroup.json")).unwrap()
}

// Independent output validation: use strings and sets, not planner internals.
fn validate(w: &Workload, schedule: &Schedule) -> (usize, u128) {
    let jobs: BTreeMap<_, _> = w.jobs.iter().map(|j| (j.job_id.as_str(), j)).collect();
    let mut seen = BTreeSet::new();
    let mut seconds = 0;
    for batch in &schedule.batches {
        assert!(!batch.job_ids.is_empty());
        let members: Vec<_> = batch.job_ids.iter().map(|id| jobs[id.as_str()]).collect();
        assert!(members.iter().all(|j| j.repo == members[0].repo));
        let checks: BTreeSet<_> = members.iter().flat_map(|j| &j.checks).collect();
        let duration = u128::from(w.boot_s)
            + checks
                .iter()
                .map(|c| u128::from(w.check_costs_s[*c]))
                .sum::<u128>();
        let finish = u128::from(batch.launched_at_s) + duration;
        for job in members {
            assert!(job.arrived_at_s <= batch.launched_at_s);
            assert!(finish <= u128::from(job.arrived_at_s) + u128::from(w.sla_s));
            assert!(seen.insert(job.job_id.as_str()), "duplicate job");
        }
        seconds += duration;
    }
    assert_eq!(seen.len(), jobs.len(), "missing jobs");
    (schedule.batches.len(), seconds)
}

fn canonical(s: &Schedule, cutoff: u64) -> Vec<(u64, Vec<String>)> {
    let mut result: Vec<_> = s
        .batches
        .iter()
        .filter(|b| b.launched_at_s <= cutoff)
        .map(|b| {
            let mut ids = b.job_ids.clone();
            ids.sort();
            (b.launched_at_s, ids)
        })
        .collect();
    result.sort();
    result
}

fn sizes(weights: &[u64]) -> Workload {
    Workload {
        boot_s: 10,
        sla_s: 60,
        check_costs_s: weights
            .iter()
            .enumerate()
            .map(|(i, &w)| (format!("c{i:03}"), w))
            .collect(),
        jobs: weights
            .iter()
            .enumerate()
            .map(|(i, _)| Job {
                job_id: format!("j{i:03}"),
                repo: "r".into(),
                arrived_at_s: 0,
                checks: vec![format!("c{i:03}")],
            })
            .collect(),
    }
}

#[test]
fn regrouping_saves_work_without_adding_a_vm() {
    let w = fixture();
    let (cost, _) = simulate(w.clone(), Policy::Cost).unwrap();
    let (count, _) = simulate(w.clone(), Policy::Count).unwrap();
    assert_eq!(validate(&w, &cost), (2, 60));
    assert_eq!(validate(&w, &count), (2, 70));
    assert_eq!(
        canonical(&cost, u64::MAX),
        vec![(30, vec!["A".into()]), (30, vec!["B".into(), "C".into()])]
    );
}

#[test]
fn all_arrivals_at_the_timer_are_visible_before_launch() {
    let mut w = sizes(&[20]);
    w.jobs.push(Job {
        job_id: "new".into(),
        arrived_at_s: 30,
        ..w.jobs[0].clone()
    });
    let (s, _) = simulate(w.clone(), Policy::Cost).unwrap();
    assert_eq!(validate(&w, &s), (1, 30));
    assert_eq!(s.batches[0].launched_at_s, 30);
}

#[test]
fn end_of_file_does_not_flush_and_idle_time_is_not_simulated_second_by_second() {
    let mut w = sizes(&[20]);
    w.jobs[0].arrived_at_s = 1_000_000_000;
    let (s, _) = simulate(w.clone(), Policy::Cost).unwrap();
    assert_eq!(s.batches[0].launched_at_s, 1_000_000_030);
    validate(&w, &s);
}

#[test]
fn future_input_does_not_change_past_output() {
    let w = random_stream(918, 120, false);
    for policy in [Policy::Cost, Policy::Count] {
        let (full, _) = simulate(w.clone(), policy).unwrap();
        for cutoff in (0..250).step_by(5) {
            let mut prefix = w.clone();
            prefix.jobs.retain(|j| j.arrived_at_s <= cutoff);
            let (partial, _) = simulate(prefix, policy).unwrap();
            assert_eq!(canonical(&full, cutoff), canonical(&partial, cutoff));
        }
    }
}

#[test]
fn ids_and_check_order_do_not_drive_decisions() {
    let w = random_stream(71, 120, false);
    let mut changed = w.clone();
    let mut reverse = BTreeMap::new();
    for (i, j) in changed.jobs.iter_mut().enumerate() {
        let replacement = format!("renamed-{}", 1000 - i);
        reverse.insert(replacement.clone(), j.job_id.clone());
        j.job_id = replacement;
        j.checks.reverse();
    }
    changed.jobs.reverse();
    let (original, _) = simulate(w, Policy::Cost).unwrap();
    let (mut renamed, _) = simulate(changed, Policy::Cost).unwrap();
    for batch in &mut renamed.batches {
        for id in &mut batch.job_ids {
            *id = reverse[id].clone();
        }
    }
    assert_eq!(
        canonical(&original, u64::MAX),
        canonical(&renamed, u64::MAX)
    );
}

#[test]
fn repositories_do_not_interfere_even_when_timers_coincide() {
    let w = fixture();
    let mut both = w.clone();
    both.jobs.extend(w.jobs.iter().map(|j| Job {
        job_id: format!("copy:{}", j.job_id),
        repo: "other".into(),
        ..j.clone()
    }));
    let (alone, _) = simulate(w, Policy::Cost).unwrap();
    let (combined, _) = simulate(both.clone(), Policy::Cost).unwrap();
    validate(&both, &combined);
    for copy in [false, true] {
        let part = Schedule {
            batches: combined
                .batches
                .iter()
                .filter(|b| b.job_ids[0].starts_with("copy:") == copy)
                .cloned()
                .map(|mut b| {
                    b.job_ids = b
                        .job_ids
                        .into_iter()
                        .map(|id| id.strip_prefix("copy:").unwrap_or(&id).to_owned())
                        .collect();
                    b
                })
                .collect(),
        };
        assert_eq!(canonical(&alone, u64::MAX), canonical(&part, u64::MAX));
    }
}

#[test]
fn identical_pending_work_is_one_pattern() {
    let mut w = sizes(&[20]);
    let job = w.jobs[0].clone();
    w.jobs = (0..10_000)
        .map(|i| Job {
            job_id: i.to_string(),
            ..job.clone()
        })
        .collect();
    let (s, stats) = simulate(w.clone(), Policy::Cost).unwrap();
    assert_eq!(validate(&w, &s), (1, 30));
    assert_eq!(stats.peak_pending_patterns, 1);
}

#[test]
fn larger_search_repairs_a_greedy_mistake() {
    let w = sizes(&[50, 30, 30, 25, 25, 15, 15, 10, 10, 10, 10, 10, 10]);
    let (s, stats) = simulate(w.clone(), Policy::Cost).unwrap();
    // 250 seconds of distinct work / 50 seconds per VM proves five necessary.
    assert_eq!(validate(&w, &s), (5, 300));
    assert!(stats.bounded_searches > 0);
    assert!(stats.search_nodes > 0);
}

#[test]
fn search_limits_preserve_feasibility() {
    let mut weights = vec![50, 30, 30, 25, 25, 15, 15, 10, 10, 10, 10, 10, 10];
    weights.extend([50; 52]);
    let w = sizes(&weights);
    let (s, stats) = simulate(w.clone(), Policy::Cost).unwrap();
    validate(&w, &s);
    assert!(stats.search_budget_exhaustions > 0);
}

#[test]
fn input_errors_are_explicit() {
    let mut w = fixture();
    w.jobs[1].job_id = w.jobs[0].job_id.clone();
    assert!(
        simulate(w, Policy::Cost)
            .unwrap_err()
            .to_string()
            .contains("duplicate")
    );
    let mut w = fixture();
    w.jobs[0].checks.push("absent".into());
    assert!(
        simulate(w, Policy::Cost)
            .unwrap_err()
            .to_string()
            .contains("unknown check")
    );
    let w = sizes(&[51]);
    assert!(
        simulate(w, Policy::Cost)
            .unwrap_err()
            .to_string()
            .contains("immediately")
    );
    let mut w = fixture();
    w.jobs[0].arrived_at_s = u64::MAX;
    assert!(
        simulate(w, Policy::Cost)
            .unwrap_err()
            .to_string()
            .contains("overflows")
    );
}

#[test]
fn zero_work_empty_input_and_large_integer_costs() {
    let mut w = sizes(&[]);
    assert!(
        simulate(w.clone(), Policy::Cost)
            .unwrap()
            .0
            .batches
            .is_empty()
    );
    w.jobs.push(Job {
        job_id: "empty".into(),
        repo: "r".into(),
        arrived_at_s: 0,
        checks: vec![],
    });
    let (s, _) = simulate(w.clone(), Policy::Cost).unwrap();
    assert_eq!(validate(&w, &s), (1, 10));
    w.boot_s = 0;
    w.sla_s = 0;
    w.jobs[0].arrived_at_s = u64::MAX;
    let (s, _) = simulate(w.clone(), Policy::Cost).unwrap();
    assert_eq!(s.batches[0].launched_at_s, u64::MAX);
    validate(&w, &s);
    let mut w = sizes(&[u64::MAX - 10, u64::MAX - 10]);
    w.sla_s = u64::MAX;
    let (s, _) = simulate(w.clone(), Policy::Cost).unwrap();
    assert_eq!(validate(&w, &s), (2, 2 * u128::from(u64::MAX)));
}

#[test]
fn duplicate_check_names_are_not_executed_twice() {
    let mut w = sizes(&[20]);
    let check = w.jobs[0].checks[0].clone();
    w.jobs[0].checks.push(check);
    let (s, _) = simulate(w.clone(), Policy::Cost).unwrap();
    assert_eq!(validate(&w, &s), (1, 30));
}

// Fixed generator for repeatable property checks; it is not scheduler code.
struct Rng(u64);
impl Rng {
    fn below(&mut self, n: u64) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0 % n
    }
}

fn random_stream(seed: u64, n: usize, simultaneous: bool) -> Workload {
    let mut rng = Rng(seed + 1);
    let costs: BTreeMap<_, _> = (0..7)
        .map(|i| (format!("c{i}"), 1 + rng.below(12)))
        .collect();
    let mut jobs = Vec::new();
    let mut now = 0;
    for i in 0..n {
        if !simultaneous {
            now += rng.below(9);
        }
        let count = 1 + rng.below(4);
        let checks: BTreeSet<_> = (0..count).map(|_| format!("c{}", rng.below(7))).collect();
        jobs.push(Job {
            job_id: format!("j{i}"),
            repo: if simultaneous {
                "r".into()
            } else {
                format!("r{}", rng.below(3))
            },
            arrived_at_s: now,
            checks: checks.into_iter().collect(),
        });
    }
    Workload {
        boot_s: 10,
        sla_s: 60,
        check_costs_s: costs,
        jobs,
    }
}

#[test]
fn generated_streams_always_finish_every_job_once_and_on_time() {
    for seed in 0..256 {
        let w = random_stream(seed, 120, false);
        for policy in [Policy::Cost, Policy::Count] {
            let (s, _) = simulate(w.clone(), policy).unwrap();
            validate(&w, &s);
        }
    }
}

// Independent exhaustive reference, only for small simultaneous-arrival tests.
// It enumerates job subsets, unlike the planner's compressed check patterns.
fn reference(w: &Workload) -> (usize, u128) {
    let size = 1 << w.jobs.len();
    let mut valid = vec![None; size];
    for (subset, cost) in valid.iter_mut().enumerate().skip(1) {
        let checks: BTreeSet<_> = w
            .jobs
            .iter()
            .enumerate()
            .filter(|(i, _)| subset & (1 << i) != 0)
            .flat_map(|(_, j)| &j.checks)
            .collect();
        let seconds = u128::from(w.boot_s)
            + checks
                .iter()
                .map(|c| u128::from(w.check_costs_s[*c]))
                .sum::<u128>();
        if seconds <= u128::from(w.sla_s) {
            *cost = Some(seconds);
        }
    }
    let mut best = vec![(usize::MAX, u128::MAX); size];
    best[0] = (0, 0);
    for remaining in 1..size {
        for subset in 1..size {
            if subset & remaining == subset
                && let Some(cost) = valid[subset]
            {
                let tail = best[remaining ^ subset];
                if tail.0 != usize::MAX {
                    best[remaining] = best[remaining].min((tail.0 + 1, tail.1 + cost));
                }
            }
        }
    }
    best[size - 1]
}

#[test]
fn small_known_workloads_match_exhaustive_reference() {
    for seed in 0..500 {
        let w = random_stream(seed, 1 + seed as usize % 8, true);
        let (s, _) = simulate(w.clone(), Policy::Cost).unwrap();
        assert_eq!(validate(&w, &s), reference(&w), "seed {seed}");
    }
}
