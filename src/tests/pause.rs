//! `runner pause` writes the tasks and their screens to the data dir and
//! stops the runner; the next start brings them back idle, with the
//! current config's commands, and starts the pinned ones. A task that is
//! not started keeps its saved screen; a start resets the screen as
//! every start does.

#![cfg(unix)]

mod common;
use common::{TestRunner, stderr_of as stderr, task_line, wait_until};

impl TestRunner {
  fn paused_files(&self) -> Vec<String> {
    std::fs::read_dir(self.runtime.path.join("dekit").join("paused"))
      .into_iter()
      .flatten()
      .filter_map(|entry| entry.ok())
      .map(|entry| entry.file_name().to_string_lossy().into_owned())
      .collect()
  }
}

#[test]
fn pause_then_start_resumes_the_tasks() {
  let runner = TestRunner::new("pa");
  runner.yaml(
    "tasks:\n  alpha:\n    cmd: [sh, -c, 'echo before; sleep 60']\n    autostart: true\n",
  );
  runner.start_runner();
  wait_until("alpha ready", || {
    task_line(&runner, "alpha").contains("ready")
  });
  wait_until("alpha output", || {
    runner.ok(&["screen", "alpha"]).contains("before")
  });
  // An ad hoc task, stopped before the pause: it comes back unpinned.
  runner.ok(&["spawn", "adhoc", "--", "sh", "-c", "echo adhoc; sleep 60"]);
  wait_until("adhoc output", || {
    runner.ok(&["screen", "adhoc"]).contains("adhoc")
  });
  runner.ok(&["stop", "adhoc"]);
  wait_until("adhoc stopped", || {
    !task_line(&runner, "adhoc").contains("ready")
  });

  let out = runner.run(&["runner", "pause"]);
  assert!(out.status.success(), "pause: {}", stderr(&out));
  assert!(
    String::from_utf8_lossy(&out.stdout).contains("paused"),
    "{out:?}"
  );
  let status: serde_json::Value =
    serde_json::from_str(&runner.ok(&["--json", "runner", "status"])).unwrap();
  assert_eq!(status["status"], "absent", "{status}");
  assert_eq!(runner.paused_files().len(), 1);

  // Edited while paused: the new command runs.
  runner.yaml(
    "tasks:\n  alpha:\n    cmd: [sh, -c, 'echo after; sleep 60']\n    autostart: true\n",
  );
  runner.start_runner();
  wait_until("alpha ready again", || {
    task_line(&runner, "alpha").contains("ready")
  });
  wait_until("alpha new output", || {
    runner.ok(&["screen", "alpha"]).contains("after")
  });
  let adhoc = task_line(&runner, "adhoc");
  assert!(adhoc.contains("idle"), "{adhoc}");
  assert!(runner.ok(&["screen", "adhoc"]).contains("adhoc"));
  assert!(runner.paused_files().is_empty());

  runner.stop();
}
