//! `--wait` and `dekit wait`: exit once the tasks are ready, or down, or
//! have exited, and say with the status how it went.

#![cfg(unix)]

mod common;
use common::{TestRunner, stderr_of, task_line};

const STACK: &str = "tasks:\n  \
  db:\n    cmd: [sh, -c, 'sleep 0.3; echo up; sleep 60']\n    ready: {log: up}\n  \
  api:\n    cmd: [sh, -c, 'sleep 0.3; echo listening; sleep 60']\n    \
  ready: {log: listening}\n    deps: [db]\n    autostart: true\n  \
  broken:\n    cmd: [sh, -c, 'echo cannot find value x; exit 3']\n    \
  ready: {log: listening}\n  \
  app:\n    cmd: [sleep, '60']\n    deps: [broken]\n  \
  slow:\n    cmd: [sleep, '60']\n    ready: {log: never}\n";

#[test]
fn up_wait_returns_once_the_stack_is_ready() {
  let runner = TestRunner::new("wu");
  runner.yaml(STACK);
  let out = runner.run(&["up", "--wait"]);
  assert!(out.status.success(), "{}", stderr_of(&out));
  // No polling: it is ready when the command returns.
  assert!(task_line(&runner, "api").contains("ready"));
  assert!(task_line(&runner, "db").contains("ready"));

  // Already ready: a wait is met at once, and a probe says so.
  runner.ok(&["wait", "api"]);
  runner.ok(&["wait", "**", "--timeout", "0"]);
  runner.stop();
}

#[test]
fn a_failure_names_the_dep_at_its_root() {
  let runner = TestRunner::new("wf");
  runner.yaml(STACK);
  runner.start_runner();
  let out = runner.run(&["start", "app", "--wait"]);
  assert_eq!(out.status.code(), Some(1));
  let err = stderr_of(&out);
  assert!(err.contains("because of broken: exited (code 3)"), "{err}");
  assert!(err.contains("cannot find value x"), "{err}");

  // Waiting for it to exit judges how it ended.
  let out = runner.run(&["wait", "broken", "--for", "exit"]);
  assert_eq!(out.status.code(), Some(1));
  runner.stop();
}

#[test]
fn a_wait_that_runs_out_of_time_exits_124() {
  let runner = TestRunner::new("wt");
  runner.yaml(STACK);
  runner.start_runner();
  let out = runner.run(&["start", "slow", "--wait", "--wait-timeout", "1s"]);
  assert_eq!(out.status.code(), Some(124));
  let err = stderr_of(&out);
  assert!(err.contains("Timed out"), "{err}");
  assert!(err.contains("slow: running"), "{err}");
  // The task is left as it is.
  assert!(task_line(&runner, "slow").contains("running"));

  let out = runner.run(&["wait", "slow", "--timeout", "0"]);
  assert_eq!(out.status.code(), Some(124));
  runner.stop();
}

#[test]
fn waiting_on_nothing_is_an_error() {
  let runner = TestRunner::new("wn");
  runner.yaml(STACK);
  runner.start_runner();
  let out = runner.run(&["wait", "nope"]);
  assert_eq!(out.status.code(), Some(1));
  assert!(stderr_of(&out).contains("no task matches 'nope'"));
  let out = runner.run(&["start", "nope", "--wait"]);
  assert_eq!(out.status.code(), Some(1));

  // A task nobody started won't get ready.
  let out = runner.run(&["wait", "db"]);
  assert_eq!(out.status.code(), Some(1));
  assert!(
    stderr_of(&out).contains("not started"),
    "{}",
    stderr_of(&out)
  );
  runner.stop();
}

#[test]
fn stop_wait_returns_once_down_or_says_who_needs_it() {
  let runner = TestRunner::new("ws");
  runner.yaml(STACK);
  runner.ok(&["up", "--wait"]);

  // api still needs db: it would start right back up.
  let out = runner.run(&["stop", "db", "--wait"]);
  assert_eq!(out.status.code(), Some(1));
  let err = stderr_of(&out);
  assert!(err.contains("api still needs it"), "{err}");

  let out = runner.run(&["veto", "db", "--wait"]);
  assert!(out.status.success(), "{}", stderr_of(&out));
  assert!(task_line(&runner, "db").contains("idle"));
  assert!(task_line(&runner, "api").contains("idle"));
  runner.stop();
}

#[test]
fn json_is_the_result() {
  let runner = TestRunner::new("wj");
  runner.yaml(STACK);
  let out = runner.ok(&["--json", "start", "db", "--wait"]);
  let result: serde_json::Value = serde_json::from_str(&out).unwrap();
  assert_eq!(result["wait"], "met");
  assert_eq!(result["tasks"][0]["path"], "db");
  assert_eq!(result["tasks"][0]["kind"], "service");
  assert_eq!(result["tasks"][0]["state"], "ready");
  runner.stop();
}
