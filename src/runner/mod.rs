use std::path::{Path, PathBuf};

use anyhow::{Context, bail};
use serde::{Deserialize, Serialize};

use crate::config::config::{Config, KernelConfig};

pub mod kill;
pub mod lockfile;
pub mod socket;
pub mod spawn;

/// Env contract between a runner and the processes it spawns: script
/// tasks read their runner identity back through these.
pub const ENV_RUNNER_ROOT: &str = "DEKIT_RUNNER_ROOT";
pub const ENV_RUNNER_KIND: &str = "DEKIT_RUNNER_KIND";

#[derive(Clone, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum RunnerKind {
  Project,
  Host,
}

impl RunnerKind {
  pub fn as_str(&self) -> &'static str {
    match self {
      RunnerKind::Project => "project",
      RunnerKind::Host => "host",
    }
  }

  pub fn from_name(name: &str) -> Option<RunnerKind> {
    match name {
      "project" => Some(RunnerKind::Project),
      "host" => Some(RunnerKind::Host),
      _ => None,
    }
  }
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct RunnerSpec {
  pub kind: RunnerKind,
  pub root: PathBuf,
}

impl RunnerSpec {
  pub fn exact(kind: RunnerKind, path: &Path) -> anyhow::Result<Self> {
    let root = dunce::canonicalize(path)
      .with_context(|| format!("invalid runner root `{}`", path.display()))?;
    validate_root(&root)?;
    if kind == RunnerKind::Host {
      let host = Self::host()?;
      if root != host.root {
        bail!("host runner root must be {}", host.root.display());
      }
      return Ok(host);
    }
    Ok(Self { kind, root })
  }

  pub fn project(path: &Path) -> anyhow::Result<Self> {
    Self::exact(RunnerKind::Project, path)
  }

  pub fn host() -> anyhow::Result<Self> {
    let root = user_config_dir()?.join("host");
    std::fs::create_dir_all(&root)?;
    Self::host_at(root)
  }

  /// Discovers the runner for a directory: the host runner inside the
  /// host root, else the nearest project. The host runner is never a
  /// fallback — outside a project it must be named explicitly, so broad
  /// verbs cannot silently reach machine-wide tasks.
  pub fn discover(from: &Path) -> anyhow::Result<Self> {
    let from = dunce::canonicalize(from)
      .with_context(|| format!("invalid path `{}`", from.display()))?;
    Self::discover_with_host(&from, Self::existing_host())
  }

  fn discover_with_host(
    from: &Path,
    host: Option<Self>,
  ) -> anyhow::Result<Self> {
    if let Some(host) = host
      && from.starts_with(&host.root)
    {
      return Ok(host);
    }
    match find_project_root(from) {
      Some(root) => Self::project(&root),
      None => bail!(
        "no project found above `{}` (looked for dekit.yaml, a git repo, or package.json); pass --chdir, or use a 'host::' target for the host runner",
        from.display()
      ),
    }
  }

  /// The host runner root, only if it already exists — no directories
  /// are created and a missing HOME is not an error. Used to classify a
  /// directory during discovery.
  fn existing_host() -> Option<Self> {
    Self::host_at(user_config_dir().ok()?.join("host")).ok()
  }

  fn host_at(root: PathBuf) -> anyhow::Result<Self> {
    let root = dunce::canonicalize(root)?;
    validate_root(&root)?;
    Ok(Self {
      kind: RunnerKind::Host,
      root,
    })
  }
}

fn validate_root(root: &Path) -> anyhow::Result<()> {
  if root.to_str().is_none() {
    bail!("runner root is not valid UTF-8: {}", root.display());
  }
  Ok(())
}

/// Walks up from `from` looking for a project root. `dekit.yaml` wins
/// even when a git repo or `package.json` is closer; otherwise the
/// nearest git repo, then the nearest `package.json`.
pub fn find_project_root(from: &Path) -> Option<PathBuf> {
  find_marker(from, |dir| dir.join("dekit.yaml").is_file())
    .or_else(|| find_marker(from, is_git_root))
    .or_else(|| find_marker(from, |dir| dir.join("package.json").is_file()))
}

fn find_marker(from: &Path, pred: impl Fn(&Path) -> bool) -> Option<PathBuf> {
  from
    .ancestors()
    .find(|dir| pred(dir))
    .map(Path::to_path_buf)
}

fn is_git_root(dir: &Path) -> bool {
  let git = dir.join(".git");
  git.is_dir() || git.is_file()
}

pub fn user_config_dir() -> anyhow::Result<PathBuf> {
  user_dir("config", "APPDATA", "XDG_CONFIG_HOME", ".config")
}

fn user_data_dir() -> anyhow::Result<PathBuf> {
  user_dir("data", "LOCALAPPDATA", "XDG_DATA_HOME", ".local/share")
}

fn user_dir(
  what: &str,
  windows_var: &str,
  xdg_var: &str,
  home_rel: &str,
) -> anyhow::Result<PathBuf> {
  let base = if cfg!(windows) {
    std::env::var_os(windows_var).map(PathBuf::from)
  } else {
    std::env::var_os(xdg_var).map(PathBuf::from).or_else(|| {
      std::env::var_os("HOME").map(|home| PathBuf::from(home).join(home_rel))
    })
  };
  let Some(base) = base else {
    bail!("could not determine the user {what} directory")
  };
  Ok(base.join("dekit"))
}

fn default_binary_file() -> anyhow::Result<PathBuf> {
  Ok(user_data_dir()?.join("default-kernel"))
}

pub fn read_default_binary() -> anyhow::Result<Option<PathBuf>> {
  let file = default_binary_file()?;
  let value = match std::fs::read_to_string(&file) {
    Ok(value) => value,
    Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
    Err(err) => return Err(err.into()),
  };
  let path = PathBuf::from(value.trim());
  Ok(Some(path))
}

pub fn set_default_binary(path: &Path) -> anyhow::Result<PathBuf> {
  let path = if path.is_absolute() {
    path.to_path_buf()
  } else {
    std::env::current_dir()?.join(path)
  };
  let path = validate_binary(path)?;
  let file = default_binary_file()?;
  let dir = file.parent().expect("default kernel file has a parent");
  std::fs::create_dir_all(dir)?;
  atomic_write(&file, format!("{}\n", path.display()).as_bytes())?;
  Ok(path)
}

pub fn clear_default_binary() -> anyhow::Result<()> {
  match std::fs::remove_file(default_binary_file()?) {
    Ok(()) => Ok(()),
    Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
    Err(err) => Err(err.into()),
  }
}

pub fn resolve_kernel_binary(runner: &RunnerSpec) -> anyhow::Result<PathBuf> {
  match Config::load_kernel(&runner.root)? {
    Some(KernelConfig::Path(path)) => validate_binary(path),
    Some(KernelConfig::Npm) => resolve_npm_binary(&runner.root),
    None => match read_default_binary()? {
      Some(path) if path.is_file() => validate_binary(path),
      Some(path) => {
        eprintln!(
          "dekit: registered default kernel does not exist: {}; using this binary",
          path.display()
        );
        validate_binary(std::env::current_exe()?)
      }
      None => validate_binary(std::env::current_exe()?),
    },
  }
}

pub(crate) fn validate_binary(path: PathBuf) -> anyhow::Result<PathBuf> {
  if !path.is_file() {
    bail!("dekit binary does not exist: {}", path.display());
  }
  validate_executable(&path)?;
  dunce::canonicalize(&path)
    .with_context(|| format!("invalid dekit binary `{}`", path.display()))
}

fn validate_executable(path: &Path) -> anyhow::Result<()> {
  if path.to_str().is_none() {
    bail!("dekit binary path is not valid UTF-8: {}", path.display());
  }
  #[cfg(unix)]
  {
    use std::os::unix::fs::PermissionsExt;
    if std::fs::metadata(path)?.permissions().mode() & 0o111 == 0 {
      bail!("dekit binary is not executable: {}", path.display());
    }
  }
  Ok(())
}

pub(crate) fn atomic_write(path: &Path, contents: &[u8]) -> anyhow::Result<()> {
  use std::io::Write;

  atomic_write_with(path, |out| Ok(out.write_all(contents)?))
}

/// Like `atomic_write`, with the contents streamed by `write`.
pub(crate) fn atomic_write_with(
  path: &Path,
  write: impl FnOnce(&mut std::io::BufWriter<std::fs::File>) -> anyhow::Result<()>,
) -> anyhow::Result<()> {
  let temp = path.with_extension(format!("tmp-{}", std::process::id()));
  let result = (|| {
    let mut out = std::io::BufWriter::new(std::fs::File::create(&temp)?);
    write(&mut out)?;
    let file = out.into_inner().map_err(|err| err.into_error())?;
    file.sync_data()?;
    std::fs::rename(&temp, path)?;
    anyhow::Ok(())
  })();
  if result.is_err() {
    let _ = std::fs::remove_file(temp);
  }
  result
}

fn resolve_npm_binary(root: &Path) -> anyhow::Result<PathBuf> {
  const RESOLVE: &str = r#"
const path = require('path');
const { createRequire } = require('module');
const root = process.argv[1];
const dekit = require.resolve('dekit/package.json', { paths: [root] });
const resolveFromDekit = createRequire(dekit);
const packageName = `@dekit/dekit-${process.platform}-${process.arch}`;
const packageJson = resolveFromDekit.resolve(`${packageName}/package.json`);
const binary = process.platform === 'win32' ? 'dekit.exe' : 'dekit';
process.stdout.write(path.join(path.dirname(packageJson), 'bin', binary));
"#;

  let mut command = std::process::Command::new("node");
  let pnp = root.join(".pnp.cjs");
  if pnp.is_file() {
    command.arg("--require").arg(pnp);
  }
  let output = command
    .arg("--eval")
    .arg(RESOLVE)
    .arg(root)
    .output()
    .with_context(
      || "kernel is pinned to npm, but Node.js could not be started",
    )?;
  if !output.status.success() {
    let error = String::from_utf8_lossy(&output.stderr);
    bail!(
      "kernel is pinned to npm, but the project's native dekit package could not be resolved\n{}\nRun the project's package-manager install command.",
      error.trim()
    );
  }
  let path = String::from_utf8(output.stdout)
    .context("npm returned a non-UTF-8 dekit binary path")?;
  validate_binary(PathBuf::from(path))
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn nearest_project_wins() {
    let root = std::env::temp_dir()
      .join(format!("dekit-resolve-{}", std::process::id()));
    let nested = root.join("a/b");
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&nested).unwrap();
    std::fs::write(root.join("dekit.yaml"), "tasks: {}\n").unwrap();
    assert_eq!(find_project_root(&nested), Some(root.clone()));
    let _ = std::fs::remove_dir_all(root);
  }

  #[test]
  fn git_repo_is_a_project() {
    let root = std::env::temp_dir()
      .join(format!("dekit-git-project-{}", std::process::id()));
    let nested = root.join("a/b");
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&nested).unwrap();
    std::fs::create_dir(root.join(".git")).unwrap();
    assert_eq!(find_project_root(&nested), Some(root.clone()));
    let _ = std::fs::remove_dir_all(root);
  }

  #[test]
  fn git_file_is_a_project() {
    let root = std::env::temp_dir()
      .join(format!("dekit-git-file-{}", std::process::id()));
    let nested = root.join("a");
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&nested).unwrap();
    std::fs::write(root.join(".git"), "gitdir: /elsewhere\n").unwrap();
    assert_eq!(find_project_root(&nested), Some(root.clone()));
    let _ = std::fs::remove_dir_all(root);
  }

  #[test]
  fn package_json_is_a_project() {
    let root = std::env::temp_dir()
      .join(format!("dekit-pkg-project-{}", std::process::id()));
    let nested = root.join("src");
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&nested).unwrap();
    std::fs::write(root.join("package.json"), "{}\n").unwrap();
    assert_eq!(find_project_root(&nested), Some(root.clone()));
    let _ = std::fs::remove_dir_all(root);
  }

  #[test]
  fn dekit_yaml_wins_over_closer_markers() {
    let root = std::env::temp_dir()
      .join(format!("dekit-yaml-over-{}", std::process::id()));
    let nested = root.join("pkg/src");
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&nested).unwrap();
    std::fs::write(root.join("dekit.yaml"), "tasks: {}\n").unwrap();
    std::fs::create_dir(root.join("pkg/.git")).unwrap();
    std::fs::write(root.join("pkg/package.json"), "{}\n").unwrap();
    assert_eq!(find_project_root(&nested), Some(root.clone()));
    let _ = std::fs::remove_dir_all(root);
  }

  #[test]
  fn git_repo_wins_over_closer_package_json() {
    let root = std::env::temp_dir()
      .join(format!("dekit-git-over-pkg-{}", std::process::id()));
    let nested = root.join("pkg/src");
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&nested).unwrap();
    std::fs::create_dir(root.join(".git")).unwrap();
    std::fs::write(root.join("pkg/package.json"), "{}\n").unwrap();
    assert_eq!(find_project_root(&nested), Some(root.clone()));
    let _ = std::fs::remove_dir_all(root);
  }

  #[test]
  fn no_project_is_an_error_not_the_host_runner() {
    let dir = std::env::temp_dir()
      .join(format!("dekit-no-project-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let dir = dunce::canonicalize(&dir).unwrap();
    let err = RunnerSpec::discover_with_host(&dir, None).unwrap_err();
    assert!(err.to_string().contains("no project found"), "{err}");
    let _ = std::fs::remove_dir_all(dir);
  }

  #[test]
  fn host_root_is_not_rediscovered_as_a_project() {
    let root = std::env::temp_dir()
      .join(format!("dekit-host-resolve-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("dekit.yaml"), "tasks: {}\n").unwrap();
    let root = dunce::canonicalize(root).unwrap();
    let host = RunnerSpec {
      kind: RunnerKind::Host,
      root: root.clone(),
    };

    assert_eq!(
      RunnerSpec::discover_with_host(&root, Some(host.clone())).unwrap(),
      host
    );
    let _ = std::fs::remove_dir_all(root);
  }

  #[cfg(unix)]
  #[test]
  fn kernel_binary_must_be_executable() {
    use std::os::unix::fs::PermissionsExt;

    let path = std::env::temp_dir()
      .join(format!("dekit-kernel-mode-{}", std::process::id()));
    std::fs::write(&path, "not executable").unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644))
      .unwrap();
    assert!(validate_binary(path.clone()).is_err());
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
      .unwrap();
    assert!(validate_binary(path.clone()).is_ok());
    let _ = std::fs::remove_file(path);
  }
}
