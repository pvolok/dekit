//! `dekit down` stops the runner, which saves its tasks and screens; the
//! next `dekit up` brings them back idle with the current config's
//! commands, starts the ones that were running, and starts the autostart
//! set. A task that is not started keeps its saved screen; a start resets
//! the screen as every start does.

#![cfg(unix)]

mod common;
use common::{DEKIT, TestRunner, stderr_of as stderr, task_line, wait_until};

impl TestRunner {
  fn saved_files(&self) -> Vec<String> {
    std::fs::read_dir(self.runtime.path.join("dekit").join("saved"))
      .into_iter()
      .flatten()
      .filter_map(|entry| entry.ok())
      .map(|entry| entry.file_name().to_string_lossy().into_owned())
      .collect()
  }
}

#[test]
fn down_then_up_restores_the_tasks() {
  let runner = TestRunner::new("dn");
  runner.yaml(
    "tasks:\n  alpha:\n    cmd: [sh, -c, 'echo before; sleep 60']\n    autostart: true\n  \
     beta:\n    cmd: [sh, -c, 'echo beta; sleep 60']\n",
  );
  runner.ok(&["up"]);
  wait_until("alpha ready", || {
    task_line(&runner, "alpha").contains("ready")
  });
  wait_until("alpha output", || {
    runner.ok(&["screen", "alpha"]).contains("before")
  });
  // Started by hand: it was running, so it comes back running.
  runner.ok(&["start", "beta"]);
  wait_until("beta ready", || {
    task_line(&runner, "beta").contains("ready")
  });
  // An ad hoc task, stopped before the down: it comes back idle with
  // its screen.
  runner.ok(&["spawn", "adhoc", "--", "sh", "-c", "echo adhoc; sleep 60"]);
  wait_until("adhoc output", || {
    runner.ok(&["screen", "adhoc"]).contains("adhoc")
  });
  runner.ok(&["stop", "adhoc"]);
  wait_until("adhoc stopped", || {
    !task_line(&runner, "adhoc").contains("ready")
  });

  let out = runner.run(&["down"]);
  assert!(out.status.success(), "down: {}", stderr(&out));
  assert!(
    String::from_utf8_lossy(&out.stdout).contains("Runner stopped"),
    "{out:?}"
  );
  let status: serde_json::Value =
    serde_json::from_str(&runner.ok(&["--json", "runner", "status"])).unwrap();
  assert_eq!(status["status"], "absent", "{status}");
  assert_eq!(runner.saved_files().len(), 1);
  // Already down: not an error.
  let again: serde_json::Value =
    serde_json::from_str(&runner.ok(&["--json", "down"])).unwrap();
  assert_eq!(again["stopped"], false, "{again}");

  // Edited while down: the new command runs.
  runner.yaml(
    "tasks:\n  alpha:\n    cmd: [sh, -c, 'echo after; sleep 60']\n    autostart: true\n  \
     beta:\n    cmd: [sh, -c, 'echo beta; sleep 60']\n",
  );
  runner.ok(&["up"]);
  wait_until("alpha ready again", || {
    task_line(&runner, "alpha").contains("ready")
  });
  wait_until("alpha new output", || {
    runner.ok(&["screen", "alpha"]).contains("after")
  });
  wait_until("beta ready again", || {
    task_line(&runner, "beta").contains("ready")
  });
  let adhoc = task_line(&runner, "adhoc");
  assert!(adhoc.contains("idle"), "{adhoc}");
  assert!(runner.ok(&["screen", "adhoc"]).contains("adhoc"));
  assert!(runner.saved_files().is_empty());

  runner.stop();
}

const YAML: &str = "tasks:\n  alpha:\n    cmd: [sh, -c, 'echo alpha; sleep 60']\n    autostart: true\n  \
   beta:\n    cmd: [sh, -c, 'echo beta; sleep 60']\n";

#[test]
fn runner_stop_does_not_save() {
  let runner = TestRunner::new("rs");
  runner.yaml(YAML);
  runner.ok(&["up"]);
  runner.ok(&["start", "beta"]);
  runner.ok(&["spawn", "adhoc", "--", "sh", "-c", "sleep 60"]);
  wait_until("beta ready", || {
    task_line(&runner, "beta").contains("ready")
  });

  let out = runner.ok(&["runner", "stop"]);
  assert!(out.contains("not saved"), "{out}");
  assert!(runner.saved_files().is_empty());

  // Fresh from dekit.yaml: no ad hoc task, and beta is not started.
  runner.ok(&["up"]);
  wait_until("alpha ready", || {
    task_line(&runner, "alpha").contains("ready")
  });
  assert_eq!(task_line(&runner, "adhoc"), "");
  let beta = task_line(&runner, "beta");
  assert!(beta.contains("idle"), "{beta}");

  runner.stop();
}

#[test]
fn runner_stop_removes_the_save_of_a_stopped_runner() {
  let runner = TestRunner::new("rr");
  runner.yaml(YAML);
  runner.ok(&["up"]);
  runner.ok(&["spawn", "adhoc", "--", "sh", "-c", "sleep 60"]);

  // `down` takes the runner as an argument too.
  let out = std::process::Command::new(DEKIT)
    .args(["down", runner.work.path.to_str().unwrap()])
    .current_dir(&runner.runtime.path)
    .env("XDG_RUNTIME_DIR", &runner.runtime.path)
    .env("XDG_CONFIG_HOME", &runner.runtime.path)
    .env("XDG_DATA_HOME", &runner.runtime.path)
    .output()
    .unwrap();
  assert!(out.status.success(), "down: {}", stderr(&out));
  assert_eq!(runner.saved_files().len(), 1);

  let removed: serde_json::Value =
    serde_json::from_str(&runner.ok(&["--json", "runner", "stop"])).unwrap();
  assert_eq!(removed["stopped"], false, "{removed}");
  assert_eq!(removed["removed_saved"], true, "{removed}");
  assert!(runner.saved_files().is_empty());
  // Nothing left to stop or remove.
  assert!(!runner.run(&["runner", "stop"]).status.success());

  runner.ok(&["up"]);
  wait_until("alpha ready", || {
    task_line(&runner, "alpha").contains("ready")
  });
  assert_eq!(task_line(&runner, "adhoc"), "");

  runner.stop();
}
