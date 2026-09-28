//! `dekit update`: installs the new dekit the way this one was installed,
//! then the new binary switches the runners (`runner upgrade --all`).
//!
//! Released binaries keep calling `INSTALL_URL` with `DEKIT_INSTALL_DIR`
//! and `DEKIT_VERSION`, and `runner upgrade --all` on what it installs.
//! Neither may change meaning.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, bail};

const INSTALL_URL: &str = if cfg!(windows) {
  "https://dekit.run/install.ps1"
} else {
  "https://dekit.run/install.sh"
};

enum Install {
  Script,
  /// The npm prefix whose global `node_modules` holds this binary.
  NpmGlobal(PathBuf),
  /// The `--root` that `cargo install` tracked this binary in.
  Cargo(PathBuf),
  /// In a `node_modules` that is not npm's global one: the project or
  /// the package manager that owns it decides the version.
  Package(PathBuf),
}

pub fn update(version: Option<&str>, json: bool) -> anyhow::Result<()> {
  // Read before the install: a replaced binary has no path on Linux.
  let exe = dunce::canonicalize(std::env::current_exe()?)?;
  let version = version.unwrap_or("latest");
  // With --json, stdout carries only the runners' results.
  let out = || {
    if json {
      Stdio::from(std::io::stderr())
    } else {
      Stdio::inherit()
    }
  };

  match install_of(&exe) {
    Install::Script => run_installer(&exe, version, out())?,
    Install::NpmGlobal(prefix) => {
      let package = format!("dekit@{}", registry_version(version)?);
      let prefix = prefix.to_string_lossy();
      let args = ["install", "-g", "--prefix", &prefix, &package];
      run_package_manager("npm", &args, out())?
    }
    Install::Cargo(root) => {
      let root = root.to_string_lossy();
      let mut args = vec!["install", "dekit", "--locked", "--root", &root];
      let version = registry_version(version)?;
      if version != "latest" {
        args.extend(["--version", version]);
      }
      run_package_manager("cargo", &args, out())?
    }
    Install::Package(root) => {
      let modules = root.join("node_modules");
      if cfg!(windows) {
        bail!(
          "this dekit is installed in {}; stop the runners that use it (`dekit down`), then update it with the package manager that put it there",
          modules.display()
        );
      }
      bail!(
        "this dekit is installed in {}; update it with the package manager that put it there, then run `dekit runner upgrade --all`",
        modules.display()
      );
    }
  }

  if !exe.is_file() {
    bail!(
      "dekit was updated, but it is no longer at {}; run `dekit runner upgrade --all` with the new one",
      exe.display()
    );
  }
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

fn install_of(exe: &Path) -> Install {
  let modules = exe
    .ancestors()
    .filter(|dir| dir.file_name().is_some_and(|name| name == "node_modules"))
    .last();
  if let Some(modules) = modules {
    // The prefix the global `node_modules` would belong to, so any npm
    // on PATH can answer, whichever Node installed this one.
    let prefix = if cfg!(windows) {
      modules.parent()
    } else {
      modules.parent().and_then(Path::parent)
    };
    let project = modules.with_file_name("package.json").exists();
    return match prefix {
      Some(prefix)
        if !project && npm_global_root(prefix).as_deref() == Some(modules) =>
      {
        Install::NpmGlobal(prefix.to_path_buf())
      }
      Some(_) | None => {
        Install::Package(modules.parent().unwrap_or(modules).to_path_buf())
      }
    };
  }
  // `cargo install` records what it installed next to the `bin` dir.
  let root = exe.parent().and_then(Path::parent);
  if let Some(root) = root
    && cargo_installed(&root.join(".crates2.json"))
  {
    return Install::Cargo(root.to_path_buf());
  }
  Install::Script
}

fn cargo_installed(crates: &Path) -> bool {
  let Ok(text) = std::fs::read_to_string(crates) else {
    return false;
  };
  let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) else {
    return false;
  };
  let bin = if cfg!(windows) { "dekit.exe" } else { "dekit" };
  json["installs"].as_object().is_some_and(|installs| {
    installs.iter().any(|(package, install)| {
      package.starts_with("dekit ")
        && install["bins"]
          .as_array()
          .is_some_and(|bins| bins.iter().any(|name| name == bin))
    })
  })
}

fn npm_global_root(prefix: &Path) -> Option<PathBuf> {
  let output = Command::new("npm")
    .args(["root", "-g", "--prefix"])
    .arg(prefix)
    .output()
    .ok()?;
  if !output.status.success() {
    return None;
  }
  dunce::canonicalize(String::from_utf8(output.stdout).ok()?.trim()).ok()
}

/// A version as npm and crates.io name it.
fn registry_version(version: &str) -> anyhow::Result<&str> {
  if version == "canary" {
    bail!(
      "canary builds come from the install script only (https://dekit.run)"
    );
  }
  Ok(version.strip_prefix('v').unwrap_or(version))
}

fn run_package_manager(
  program: &str,
  args: &[&str],
  out: Stdio,
) -> anyhow::Result<()> {
  let command = format!("{program} {}", args.join(" "));
  // A running dekit.exe cannot be replaced, and this one is running.
  if cfg!(windows) {
    bail!(
      "stop the runners that use this dekit (`dekit down`), then run: {command}"
    );
  }
  let status = Command::new(program)
    .args(args)
    .stdout(out)
    .status()
    .with_context(|| format!("cannot run `{command}`"))?;
  if !status.success() {
    bail!("`{command}` failed");
  }
  Ok(())
}

fn run_installer(exe: &Path, version: &str, out: Stdio) -> anyhow::Result<()> {
  // The installer writes `dekit` into the directory it is given.
  let name = if cfg!(windows) { "dekit.exe" } else { "dekit" };
  if exe.file_name().is_none_or(|file| file != name) {
    bail!(
      "this binary is {}, and the installer replaces only `{name}`; install dekit again from https://dekit.run",
      exe.display()
    );
  }
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
