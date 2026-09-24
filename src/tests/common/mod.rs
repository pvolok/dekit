//! One isolated runner per test: its own working dir and runtime dir.

#![allow(dead_code)]

use std::path::PathBuf;
use std::process::{Child, Command, Output};
use std::time::{Duration, Instant};

pub const DEKIT: &str = env!("CARGO_BIN_EXE_dekit");

/// Unique temp dir, removed on drop.
pub struct TmpDir {
  pub path: PathBuf,
}

impl TmpDir {
  /// Keep `name` short: the runtime dir ends up inside a unix socket
  /// path, which must stay under SUN_LEN (~104 bytes).
  pub fn new(name: &str) -> Self {
    let path =
      std::env::temp_dir().join(format!("dk-{}-{}", name, std::process::id()));
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

pub struct TestRunner {
  pub work: TmpDir,
  pub runtime: TmpDir,
}

impl TestRunner {
  pub fn new(name: &str) -> Self {
    TestRunner {
      work: TmpDir::new(&format!("{name}w")),
      runtime: TmpDir::new(&format!("{name}r")),
    }
  }

  pub fn start(name: &str) -> Self {
    let runner = TestRunner::new(name);
    runner.start_runner();
    runner
  }

  pub fn start_runner(&self) {
    let out = self.run(&["runner", "start"]);
    assert!(out.status.success(), "runner start: {}", stderr_of(&out));
  }

  pub fn cmd(&self, args: &[&str]) -> Command {
    let mut cmd = Command::new(DEKIT);
    cmd
      .arg("-C")
      .arg(&self.work.path)
      .args(args)
      .env("XDG_RUNTIME_DIR", &self.runtime.path)
      .env("XDG_CONFIG_HOME", &self.runtime.path)
      .env("XDG_DATA_HOME", &self.runtime.path);
    cmd
  }

  pub fn run(&self, args: &[&str]) -> Output {
    self.cmd(args).output().unwrap()
  }

  pub fn spawn(&self, args: &[&str]) -> Child {
    self
      .cmd(args)
      .stdout(std::process::Stdio::piped())
      .stderr(std::process::Stdio::piped())
      .spawn()
      .unwrap()
  }

  pub fn stop(&self) {
    let out = self.run(&["runner", "stop"]);
    assert!(out.status.success(), "runner stop: {}", stderr_of(&out));
  }

  pub fn ok(&self, args: &[&str]) -> String {
    let out = self.run(args);
    assert!(out.status.success(), "{:?}: {}", args, stderr_of(&out));
    String::from_utf8_lossy(&out.stdout).into_owned()
  }

  pub fn yaml(&self, body: &str) {
    std::fs::write(self.work.path.join("dekit.yaml"), body).unwrap();
  }
}

pub fn wait_until(what: &str, mut check: impl FnMut() -> bool) {
  let deadline = Instant::now() + Duration::from_secs(10);
  while !check() {
    assert!(Instant::now() < deadline, "timed out waiting for {what}");
    std::thread::sleep(Duration::from_millis(50));
  }
}

/// The `ls` line of one task, empty when it is not listed.
pub fn task_line(runner: &TestRunner, task: &str) -> String {
  runner
    .ok(&["ls"])
    .lines()
    .find(|line| line.split_whitespace().any(|word| word == task))
    .map(str::to_string)
    .unwrap_or_default()
}

impl Drop for TestRunner {
  // The runner is detached: after a failed assertion it would outlive the
  // test, unreachable once the runtime dir is deleted.
  fn drop(&mut self) {
    let _ = self.run(&["runner", "stop"]);
  }
}

pub fn stderr_of(out: &Output) -> String {
  String::from_utf8_lossy(&out.stderr).into_owned()
}
