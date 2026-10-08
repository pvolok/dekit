//! `dekit update`: runs the install script again into the directory of
//! this binary, then the new binary switches the runners
//! (`runner upgrade --all`). Only a dekit the script installed updates
//! itself; the script leaves a receipt with its path.
//!
//! Released binaries keep calling `INSTALL_URL` with `DEKIT_INSTALL_DIR`
//! and `DEKIT_VERSION`, reading the receipt, and running
//! `runner upgrade --all` on what it installs. None of these may change
//! meaning.

use std::path::Path;
use std::process::{Command, Stdio};

use anyhow::{Context, bail};

use crate::runner::user_data_dir;

const INSTALL_URL: &str = if cfg!(windows) {
  "https://dekit.run/install.ps1"
} else {
  "https://dekit.run/install.sh"
};

pub fn update(version: Option<&str>, json: bool) -> anyhow::Result<()> {
  // Read before the install: a replaced binary has no path on Linux.
  let exe = dunce::canonicalize(std::env::current_exe()?)?;
  if !installed_by_script(&exe) {
    bail!(
      "this dekit was not installed by the install script (https://dekit.run); update it the way you installed it, then run `dekit runner upgrade --all`"
    );
  }
  let version = version.unwrap_or("latest");
  // With --json, stdout carries only the runners' results.
  let out = if json {
    Stdio::from(std::io::stderr())
  } else {
    Stdio::inherit()
  };
  run_installer(&exe, version, out)?;

  let mut upgrade = Command::new(&exe);
  upgrade.args(["runner", "upgrade", "--all"]);
  if json {
    upgrade.arg("--json");
  }
  let status = upgrade
    .status()
    .with_context(|| format!("cannot run {}", exe.display()))?;
  if !status.success() {
    std::process::exit(status.code().unwrap_or(1));
  }
  Ok(())
}

/// The receipt holds the path of the binary the script installed last.
fn installed_by_script(exe: &Path) -> bool {
  let Ok(dir) = user_data_dir() else {
    return false;
  };
  let Ok(path) = std::fs::read_to_string(dir.join("install")) else {
    return false;
  };
  dunce::canonicalize(path.trim_end()).is_ok_and(|path| path == exe)
}

fn run_installer(exe: &Path, version: &str, out: Stdio) -> anyhow::Result<()> {
  let dir = exe.parent().context("the dekit binary has no directory")?;
  let url = std::env::var("DEKIT_INSTALL_URL")
    .unwrap_or_else(|_| INSTALL_URL.to_string());

  #[cfg(unix)]
  let mut installer = {
    let mut installer = Command::new("sh");
    installer.arg("-s").stdin(Stdio::piped());
    installer
  };
  #[cfg(windows)]
  let mut installer = {
    let mut installer = Command::new("powershell");
    installer.args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-Command"]);
    installer.arg("irm $env:DEKIT_INSTALL_URL | iex");
    installer.env("DEKIT_INSTALL_URL", &url);
    // PowerShell 7's module path breaks Windows PowerShell's own modules
    // (Get-FileHash, Expand-Archive); without it, powershell uses its default.
    installer.env_remove("PSModulePath");
    installer
  };
  installer
    .env("DEKIT_INSTALL_DIR", dir)
    .env("DEKIT_VERSION", version)
    .stdout(out);

  #[cfg(unix)]
  let status = {
    use std::io::Write;

    let script = download(&url)?;
    let mut child = installer.spawn().context("cannot run sh")?;
    let mut stdin = child.stdin.take().expect("stdin is piped");
    // An installer that stops reading has failed; its status says so.
    let written = stdin.write_all(&script);
    drop(stdin);
    let status = child.wait()?;
    if status.success() {
      written?;
    }
    status
  };
  #[cfg(windows)]
  let status = installer.status().context("cannot run powershell")?;

  if !status.success() {
    bail!("the installer failed; dekit was not updated");
  }
  Ok(())
}

#[cfg(unix)]
fn download(url: &str) -> anyhow::Result<Vec<u8>> {
  for (program, flags) in [("curl", "-fsSL"), ("wget", "-qO-")] {
    let output = Command::new(program)
      .args([flags, url])
      .stderr(Stdio::inherit())
      .output();
    match output {
      Ok(output) if output.status.success() && !output.stdout.is_empty() => {
        return Ok(output.stdout);
      }
      Ok(_) => bail!("could not download {url}"),
      Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
      Err(err) => {
        return Err(err).with_context(|| format!("cannot run {program}"));
      }
    }
  }
  bail!("curl or wget is needed to download {url}")
}
