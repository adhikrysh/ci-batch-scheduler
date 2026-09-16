use std::io::Write;
use std::process::{Command, Stdio};

fn run(args: &[&str], input: &str) -> std::process::Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_ci-batch-scheduler"))
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.as_bytes())
        .unwrap();
    child.wait_with_output().unwrap()
}

#[test]
fn stdout_is_only_schedule_json_and_stats_are_separate() {
    let output = run(&["--stats", "-"], include_str!("../examples/regroup.json"));
    assert!(output.status.success());
    let schedule: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(schedule.as_object().unwrap().len(), 1);
    assert!(schedule["batches"].is_array());
    let stats: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
    assert!(stats["planning_calls"].as_u64().unwrap() > 0);
}

#[test]
fn bad_input_and_bad_arguments_fail_without_schedule_output() {
    for input in [
        "not json",
        "{}",
        r#"{"boot_s":-1,"sla_s":60,"check_costs_s":{},"jobs":[]}"#,
        r#"{"boot_s":1.5,"sla_s":60,"check_costs_s":{},"jobs":[]}"#,
    ] {
        let output = run(&["-"], input);
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        assert!(!output.stderr.is_empty());
    }
    for args in [vec!["--unknown"], vec!["--policy", "invalid"], vec![]] {
        let output = run(&args, "");
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
    }
}

#[test]
fn example_file_is_runnable() {
    let output = run(
        &[concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/examples/regroup.json"
        )],
        "",
    );
    assert!(output.status.success());
    assert!(output.stderr.is_empty());
    serde_json::from_slice::<ci_batch_scheduler::Schedule>(&output.stdout).unwrap();
}
