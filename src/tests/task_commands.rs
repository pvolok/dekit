//! Ready and stop commands that cannot run. std's spawn waits for a child
//! whose exec failed, and lost that wait to the runner's reaper often
//! enough to panic under a fast-polling check.

#![cfg(unix)]

use std::time::{Duration, Instant};

mod common;
use common::{TestRunner, stderr_of as stderr, task_line, wait_until};

#[test]
fn commands_that_cannot_run_are_reported() {
  let runner = TestRunner::new("tc");
  let mut yaml = String::from("tasks:\n");
  for i in 0..6 {
    yaml += &format!(
      "  t{i}:\n    cmd: [sleep, '60']\n    autostart: true\n    \
       ready: {{cmd: ./missing.sh, interval: 5ms}}\n    \
       stop: {{cmd: ./missing-stop.sh, timeout: 1s}}\n"
    );
  }
  // A changed PATH and a bare program: std's fork path on every Unix.
  yaml += "  p:\n    cmd: [sleep, '60']\n    autostart: true\n    \
           add_path: [bin]\n    ready: {cmd: missing-probe, interval: 5ms}\n";
  runner.yaml(&yaml);
  runner.ok(&["up"]);
  let log = runner.work.path.join("dekit.log");
  let read_log = || std::fs::read_to_string(&log).unwrap_or_default();
  wait_until("a warning for each ready command", || {
    read_log().matches("Ready command").count() == 7
  });
  // Thousands of failed attempts.
  std::thread::sleep(Duration::from_secs(3));
  for task in ["t0", "t5", "p"] {
    let line = task_line(&runner, task);
    assert!(line.contains("running"), "{line}");
  }

  let started = Instant::now();
  let out = runner.run(&["down"]);
  assert!(out.status.success(), "down: {}", stderr(&out));
  assert!(started.elapsed() < Duration::from_secs(10));
  let log = read_log();
  assert!(!log.contains("panic"), "{log}");
  assert_eq!(log.matches("Stop command").count(), 6, "{log}");
}
