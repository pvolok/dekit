//! Concurrent-CLI races against one runner: two clients hitting the same
//! verb must see exactly-one-winner (spawn) or only well-defined replies
//! (start under churn), never internal errors.

use std::path::Path;

mod common;
use common::{TestRunner, TmpDir, stderr_of};

fn assert_dir_has_no_lock(runtime: &Path) {
  let dekit_dir = runtime.join("dekit");
  if let Ok(entries) = std::fs::read_dir(dekit_dir) {
    // A quitting runner leaves its saved tasks there on purpose.
    let leftover: Vec<_> = entries
      .filter_map(|e| e.ok())
      .map(|e| e.file_name().to_string_lossy().into_owned())
      .filter(|name| name != "saved")
      .collect();
    assert!(
      leftover.is_empty(),
      "runner files left behind: {:?}",
      leftover
    );
  }
}

#[test]
fn spawn_race_has_exactly_one_winner() {
  let runner = TestRunner::start("sr");

  let a = runner.spawn(&["spawn", "same", "--", "sleep", "30"]);
  let b = runner.spawn(&["spawn", "same", "--", "sleep", "30"]);
  let outs = [a.wait_with_output().unwrap(), b.wait_with_output().unwrap()];

  let winners = outs.iter().filter(|o| o.status.success()).count();
  assert_eq!(
    winners,
    1,
    "expected exactly one spawn winner, got {}: [{}] [{}]",
    winners,
    stderr_of(&outs[0]),
    stderr_of(&outs[1]),
  );
  let loser = outs.iter().find(|o| !o.status.success()).unwrap();
  assert!(
    stderr_of(loser).contains("already exists"),
    "loser failed for the wrong reason: {}",
    stderr_of(loser)
  );

  runner.stop();
  assert_dir_has_no_lock(&runner.runtime.path);
}

#[test]
fn start_races_spawn_and_down_without_internal_errors() {
  let runner = TestRunner::start("ch");

  for i in 0..10 {
    let path = format!("x/{}", i);
    let spawner = runner.spawn(&["spawn", &path, "--", "sleep", "30"]);
    let starter = runner.spawn(&["start", "x/*"]);

    let spawn_out = spawner.wait_with_output().unwrap();
    assert!(
      spawn_out.status.success(),
      "spawn {} failed: {}",
      path,
      stderr_of(&spawn_out)
    );

    // The start raced the spawn: acting on zero or more matches always
    // succeeds. Any failure (internal error, dangling reply) is a bug.
    let start_out = starter.wait_with_output().unwrap();
    assert!(
      start_out.status.success(),
      "start failed unexpectedly: {}",
      stderr_of(&start_out)
    );

    let stop_out = runner.run(&["stop", "x/*"]);
    assert!(
      stop_out.status.success(),
      "stop failed: {}",
      stderr_of(&stop_out)
    );
  }

  // The runner survived the churn.
  let ls = runner.run(&["ls"]);
  assert!(ls.status.success(), "ls failed: {}", stderr_of(&ls));

  runner.stop();
}

#[test]
fn live_runner_ignores_a_broken_kernel_pin() {
  let runner = TestRunner::start("pin");
  std::fs::write(
    runner.work.path.join("dekit.yaml"),
    "kernel: {path: missing-dekit}\n",
  )
  .unwrap();

  let ls = runner.run(&["ls"]);
  assert!(ls.status.success(), "ls failed: {}", stderr_of(&ls));
  let status = runner.run(&["runner", "status"]);
  assert!(
    status.status.success(),
    "status failed: {}",
    stderr_of(&status)
  );
  assert!(
    String::from_utf8_lossy(&status.stdout).contains("Kernel selection error")
  );
  runner.stop();
}

#[test]
fn startup_reports_config_errors() {
  let runner = TestRunner::new("err");
  std::fs::write(
    runner.work.path.join("dekit.yaml"),
    "unknown_setting: true\n",
  )
  .unwrap();

  let output = runner.run(&["runner", "start"]);
  let error = stderr_of(&output);
  assert!(!output.status.success());
  assert!(error.contains("unknown_setting"), "wrong error: {error}");
  assert!(
    !error.contains("did not become ready"),
    "wrong error: {error}"
  );
}

#[test]
fn script_task_controls_its_runner() {
  let runner = TestRunner::new("js");
  let script_cwd = TmpDir::new("jscwd");
  std::fs::write(
    runner.work.path.join("dekit.yaml"),
    format!(
      "tasks:\n  worker:\n    cmd: [sleep, '30']\n  workflow:\n    script: workflow.js\n    cwd: '{}'\n",
      script_cwd.path.display()
    ),
  )
  .unwrap();
  std::fs::write(
    runner.work.path.join("workflow.js"),
    "export async function main() { return await std.dekit.start('worker') }\n",
  )
  .unwrap();
  runner.start_runner();

  let start = runner.run(&["start", "workflow"]);
  assert!(
    start.status.success(),
    "script failed: {}",
    stderr_of(&start)
  );

  let mut running = false;
  for _ in 0..40 {
    let output = runner.run(&["ls", "worker"]);
    assert!(output.status.success(), "ls failed: {}", stderr_of(&output));
    let stdout = String::from_utf8_lossy(&output.stdout);
    if stdout.contains("running") || stdout.contains("ready") {
      running = true;
      break;
    }
    std::thread::sleep(std::time::Duration::from_millis(50));
  }
  let workflow = runner.run(&["screen", "workflow"]);
  assert!(
    running,
    "script did not start worker: {}{}",
    String::from_utf8_lossy(&workflow.stdout),
    stderr_of(&workflow),
  );
  runner.stop();
}
