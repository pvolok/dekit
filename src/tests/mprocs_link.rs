//! A link named `mprocs` runs the mprocs command line, like `dekit mprocs`.

#![cfg(unix)]

use std::process::Command;

mod common;
use common::{DEKIT, TmpDir, stderr_of as stderr};

#[test]
fn mprocs_link_runs_the_mprocs_cli() {
  let dir = TmpDir::new("mplink");
  let link = dir.path.join("mprocs");
  std::os::unix::fs::symlink(DEKIT, &link).unwrap();

  let out = Command::new(&link).arg("--help").output().unwrap();
  assert!(out.status.success(), "{}", stderr(&out));
  let help = String::from_utf8_lossy(&out.stdout);
  assert!(help.contains("Usage: mprocs"), "{help}");
  assert!(help.contains("--ctl"), "{help}");
}
