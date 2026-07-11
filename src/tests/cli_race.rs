//! Concurrent-CLI races against one daemon: two clients hitting the same
//! verb must see exactly-one-winner (spawn) or only well-defined replies
//! (start under churn), never internal errors.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output};

const DEKIT: &str = env!("CARGO_BIN_EXE_dekit");

/// Unique temp dir, removed on drop.
struct TmpDir {
  path: PathBuf,
}

impl TmpDir {
  /// Keep `name` short: the runtime dir ends up inside a unix socket
  /// path, which must stay under SUN_LEN (~104 bytes).
  fn new(name: &str) -> Self {
    let path = std::env::temp_dir().join(format!(
      "dk-{}-{}",
      name,
      std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&path);
    std::fs::create_dir_all(&path).unwrap();
    TmpDir { path }
  }
}

impl Drop for TmpDir {
  fn drop(&mut self) {
    let _ = std::fs::remove_dir_all(&self.path);
  }
}

/// One isolated daemon: its own working dir and runtime dir.
struct Daemon {
  work: TmpDir,
  runtime: TmpDir,
}

impl Daemon {
  fn start(name: &str) -> Self {
    let daemon = Daemon {
      work: TmpDir::new(&format!("{}w", name)),
      runtime: TmpDir::new(&format!("{}r", name)),
    };
    let out = daemon.run(&["server", "start"]);
    assert!(
      out.status.success(),
      "server start failed: {}",
      String::from_utf8_lossy(&out.stderr)
    );
    daemon
  }

  fn cmd(&self, args: &[&str]) -> Command {
    let mut cmd = Command::new(DEKIT);
    cmd
      .arg("-C")
      .arg(&self.work.path)
      .args(args)
      .env("XDG_RUNTIME_DIR", &self.runtime.path);
    cmd
  }

  fn run(&self, args: &[&str]) -> Output {
    self.cmd(args).output().unwrap()
  }

  fn spawn(&self, args: &[&str]) -> Child {
    self
      .cmd(args)
      .stdout(std::process::Stdio::piped())
      .stderr(std::process::Stdio::piped())
      .spawn()
      .unwrap()
  }

  fn stop(&self) {
    let out = self.run(&["server", "stop"]);
    assert!(
      out.status.success(),
      "server stop failed: {}",
      String::from_utf8_lossy(&out.stderr)
    );
  }
}

fn stderr_of(out: &Output) -> String {
  String::from_utf8_lossy(&out.stderr).into_owned()
}

fn assert_dir_has_no_lock(runtime: &Path) {
  let dekit_dir = runtime.join("dekit");
  if let Ok(entries) = std::fs::read_dir(dekit_dir) {
    let leftover: Vec<_> = entries
      .filter_map(|e| e.ok())
      .map(|e| e.file_name().to_string_lossy().into_owned())
      .collect();
    assert!(
      leftover.is_empty(),
      "daemon files left behind: {:?}",
      leftover
    );
  }
}

#[test]
fn spawn_race_has_exactly_one_winner() {
  let daemon = Daemon::start("sr");

  let a = daemon.spawn(&["spawn", "--path", "/same", "--", "sleep", "30"]);
  let b = daemon.spawn(&["spawn", "--path", "/same", "--", "sleep", "30"]);
  let outs = [
    a.wait_with_output().unwrap(),
    b.wait_with_output().unwrap(),
  ];

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

  daemon.stop();
  assert_dir_has_no_lock(&daemon.runtime.path);
}

#[test]
fn start_races_spawn_and_down_without_internal_errors() {
  let daemon = Daemon::start("ch");

  for i in 0..10 {
    let path = format!("/x/{}", i);
    let spawner =
      daemon.spawn(&["spawn", "--path", &path, "--", "sleep", "30"]);
    let starter = daemon.spawn(&["start", "/x/*"]);

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

    let down_out = daemon.run(&["down", "/x/*"]);
    assert!(
      down_out.status.success(),
      "down failed: {}",
      stderr_of(&down_out)
    );
  }

  // The daemon survived the churn.
  let ls = daemon.run(&["ls"]);
  assert!(ls.status.success(), "ls failed: {}", stderr_of(&ls));

  daemon.stop();
}
