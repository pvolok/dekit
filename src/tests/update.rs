//! `dekit update` end to end, offline: the real install script, a release
//! served from `file://` URLs, and a runner that records an older version.

#![cfg(unix)]

use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use sha2::{Digest, Sha256};

mod common;
use common::{DEKIT, TestRunner, stderr_of as stderr, wait_until};

const VERSION: &str = env!("CARGO_PKG_VERSION");
const INSTALLER: &str = concat!(
  env!("CARGO_MANIFEST_DIR"),
  "/../packaging/install/install.sh"
);

/// A dekit installed by the script, with its own runner and a release to
/// update from.
struct Installed {
  runner: TestRunner,
  exe: PathBuf,
}

impl Installed {
  fn new(name: &str) -> Self {
    let runner = TestRunner::new(name);
    let bin = runner.work.path.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    std::fs::copy(DEKIT, bin.join("dekit")).unwrap();
    let exe = std::fs::canonicalize(bin.join("dekit")).unwrap();
    Installed { runner, exe }
  }

  fn run(&self, args: &[&str]) -> Output {
    let mut cmd = Command::new(&self.exe);
    cmd
      .arg("-C")
      .arg(&self.runner.work.path)
      .args(args)
      .env("XDG_RUNTIME_DIR", &self.runner.runtime.path)
      .env("XDG_CONFIG_HOME", &self.runner.runtime.path)
      .env("XDG_DATA_HOME", &self.runner.runtime.path)
      .env("DEKIT_INSTALL_URL", format!("file://{INSTALLER}"))
      .env(
        "DEKIT_RELEASES_URL",
        format!("file://{}", self.release().display()),
      );
    cmd.output().unwrap()
  }

  fn release(&self) -> PathBuf {
    self.runner.work.path.join("release")
  }

  /// Publishes this build as the latest release. `sums` replaces the real
  /// checksum.
  fn publish(&self, sums: Option<&str>) {
    let cpu = std::env::consts::ARCH;
    let os = if cfg!(target_os = "macos") {
      "apple-darwin"
    } else {
      "unknown-linux-musl"
    };
    let asset = format!("dekit-{cpu}-{os}.tar.gz");
    let dir = self.release().join("latest/download");
    std::fs::create_dir_all(&dir).unwrap();
    let tar = Command::new("tar")
      .arg("-czf")
      .arg(dir.join(&asset))
      .arg("-C")
      .arg(Path::new(DEKIT).parent().unwrap())
      .arg("dekit")
      .status()
      .unwrap();
    assert!(tar.success());
    let sum = match sums {
      Some(sum) => sum.to_string(),
      None => Sha256::digest(std::fs::read(dir.join(&asset)).unwrap())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect(),
    };
    std::fs::write(dir.join("SHA256SUMS"), format!("{sum}  {asset}\n"))
      .unwrap();
  }

  /// `runner status` as the installed dekit sees it.
  fn record(&self) -> serde_json::Value {
    let out = self.run(&["--json", "runner", "status"]);
    assert!(out.status.success(), "{}", stderr(&out));
    serde_json::from_str(&stdout(&out)).unwrap()
  }

  /// Rewrites the published record to say the runner runs an old dekit.
  fn record_old_version(&self) {
    let dir = self.runner.runtime.path.join("dekit");
    let file = std::fs::read_dir(dir)
      .unwrap()
      .map(|entry| entry.unwrap().path())
      .find(|path| path.extension().is_some_and(|ext| ext == "json"))
      .expect("a runner record");
    let mut record: serde_json::Value =
      serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
    record["version"] = "0.0.1".into();
    std::fs::write(&file, record.to_string()).unwrap();
  }

  fn spawn_sleeper(&self) -> u32 {
    self.runner.ok(&[
      "spawn",
      "sleeper",
      "--",
      "sh",
      "-c",
      "echo pid $$; exec sleep 1000",
    ]);
    let mut pid = None;
    wait_until("the sleeper's pid", || {
      let screen = self.runner.ok(&["screen", "sleeper"]);
      pid = screen
        .lines()
        .find_map(|line| line.trim().strip_prefix("pid ")?.trim().parse().ok());
      pid.is_some()
    });
    pid.unwrap()
  }

  fn inode(&self) -> u64 {
    std::fs::metadata(&self.exe).unwrap().ino()
  }
}

fn stdout(out: &Output) -> String {
  String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn update_replaces_the_binary_and_upgrades_its_runner() {
  let dekit = Installed::new("upd");
  dekit.publish(None);
  let started = dekit.run(&["runner", "start"]);
  assert!(started.status.success(), "{}", stderr(&started));
  let child = dekit.spawn_sleeper();
  let before = dekit.record();
  assert_eq!(before["binary"], dekit.exe.to_str().unwrap());
  let inode = dekit.inode();
  assert_eq!(before["restart_required"], false);
  dekit.record_old_version();
  assert_eq!(dekit.record()["restart_required"], true);

  let out = dekit.run(&["update"]);
  assert!(out.status.success(), "{}", stderr(&out));
  let text = stdout(&out);
  assert!(text.contains(" installed to "), "{text}");
  assert!(
    text.contains(&format!("from dekit 0.0.1 to {VERSION}")),
    "{text}"
  );

  assert_ne!(dekit.inode(), inode, "the binary was not replaced");
  let after = dekit.record();
  assert_eq!(after["version"], VERSION);
  assert_eq!(after["restart_required"], false);
  assert_eq!(after["pid"], before["pid"]);
  assert_eq!(after["binary"], before["binary"]);
  let screen = dekit.runner.ok(&["screen", "sleeper"]);
  assert!(screen.contains(&format!("pid {child}")), "{screen}");
  assert!(dekit.runner.ok(&["ls"]).contains("ready"));

  // Nothing is left to do the second time.
  let again = dekit.run(&["--json", "update"]);
  assert!(again.status.success(), "{}", stderr(&again));
  let results: serde_json::Value =
    serde_json::from_str(&stdout(&again)).unwrap();
  assert_eq!(results[0]["result"], "current", "{results}");
  assert_eq!(results.as_array().unwrap().len(), 1);
}

#[test]
fn update_with_a_bad_checksum_changes_nothing() {
  let dekit = Installed::new("upb");
  dekit.publish(Some(&"0".repeat(64)));
  let started = dekit.run(&["runner", "start"]);
  assert!(started.status.success(), "{}", stderr(&started));
  let before = dekit.record();
  let inode = dekit.inode();
  dekit.record_old_version();

  let out = dekit.run(&["update"]);
  assert!(!out.status.success());
  assert!(
    stderr(&out).contains("checksum mismatch"),
    "{}",
    stderr(&out)
  );

  assert_eq!(dekit.inode(), inode);
  let after = dekit.record();
  assert_eq!(after["version"], "0.0.1");
  assert_eq!(after["pid"], before["pid"]);
}

#[test]
fn upgrade_all_keeps_runners_on_other_binaries() {
  let dekit = Installed::new("upk");
  dekit.runner.start_runner();
  let before = dekit.record();
  assert_eq!(before["binary"], DEKIT);
  dekit.record_old_version();

  let out = dekit.run(&["--json", "runner", "upgrade", "--all"]);
  assert!(out.status.success(), "{}", stderr(&out));
  let results: serde_json::Value = serde_json::from_str(&stdout(&out)).unwrap();
  assert_eq!(results[0]["result"], "kept", "{results}");
  assert_eq!(results[0]["version"], "0.0.1");
  assert_eq!(dekit.record()["version"], "0.0.1");
}
