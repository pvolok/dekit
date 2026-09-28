//! `dekit up` is its own verb: a runner start alone starts nothing, and
//! `up` starts what is not running and leaves running tasks and done jobs
//! alone, so a second `up` changes nothing.

#![cfg(unix)]

use std::time::Duration;

mod common;
use common::{TestRunner, task_line, wait_until};

fn pid_of(runner: &TestRunner, task: &str) -> u32 {
  let screen = runner.ok(&["screen", task]);
  screen
    .lines()
    .find_map(|line| line.trim().strip_prefix("pid ")?.trim().parse().ok())
    .unwrap_or_else(|| panic!("{task} printed no pid: {screen}"))
}

#[test]
fn a_fresh_runner_starts_nothing_until_up() {
  let runner = TestRunner::new("fr");
  runner.yaml(
    "tasks:\n  alpha:\n    cmd: [sleep, '60']\n    autostart: true\n  \
     beta:\n    cmd: [sleep, '60']\n",
  );
  runner.start_runner();
  std::thread::sleep(Duration::from_millis(300));
  let alpha = task_line(&runner, "alpha");
  assert!(alpha.contains("idle"), "{alpha}");

  // A start starts what it names, not the autostart set.
  runner.ok(&["start", "beta"]);
  wait_until("beta ready", || {
    task_line(&runner, "beta").contains("ready")
  });
  let alpha = task_line(&runner, "alpha");
  assert!(alpha.contains("idle"), "{alpha}");

  let out = runner.ok(&["--json", "up"]);
  assert!(out.contains("\"matched\":1"), "{out}");
  wait_until("alpha ready", || {
    task_line(&runner, "alpha").contains("ready")
  });

  runner.stop();
}

/// The review's reproduction: a done autostart job and its dependent.
#[test]
fn up_again_does_not_rerun_a_done_job() {
  let runner = TestRunner::new("uj");
  runner.yaml(
    "tasks:\n  setup:\n    type: job\n    cmd: [sh, -c, 'echo run >> runs.txt']\n    \
     autostart: true\n  \
     db:\n    cmd: [sh, -c, 'echo pid $$; sleep 60']\n    deps: [setup]\n    \
     autostart: true\n",
  );
  runner.ok(&["up"]);
  wait_until("db ready", || task_line(&runner, "db").contains("ready"));
  wait_until("db pid", || runner.ok(&["screen", "db"]).contains("pid "));
  let pid = pid_of(&runner, "db");

  for _ in 0..2 {
    runner.ok(&["up"]);
  }
  std::thread::sleep(Duration::from_millis(300));
  let runs =
    std::fs::read_to_string(runner.work.path.join("runs.txt")).unwrap();
  assert_eq!(runs.lines().count(), 1, "{runs}");
  assert!(task_line(&runner, "setup").contains("done"));
  assert!(task_line(&runner, "db").contains("ready"));
  assert_eq!(pid_of(&runner, "db"), pid);

  runner.stop();
}
