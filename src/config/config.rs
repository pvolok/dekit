use std::collections::HashSet;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use crate::cfg::{CfgCx, CfgDoc, CfgNode, CfgObj};
use crate::config::hook::{Hook, hook_from_cfg};
use crate::config::keymap::KeymapConfig;
use crate::config::log::LogConfig;
use crate::config::task::{TaskConfig, parse_task_settings, task_from_cfg};
use crate::config::tui::TuiConfig;
use crate::kernel::task_path::TaskPath;
use crate::runner::user_config_dir;

pub(crate) const ROOT_KEYS: &[&str] = &[
  "kernel", "load", "log", "defaults", "on_init", "on_idle", "tasks", "tui",
  "keymap",
];
const FRAGMENT_KEYS: &[&str] = &["load", "tasks"];
pub(crate) const PRESENTATION_KEYS: &[&str] = &["tui", "keymap"];

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum KernelConfig {
  Npm,
  Path(PathBuf),
}

#[derive(Clone)]
pub struct Config {
  pub runner: Option<crate::runner::RunnerSpec>,
  pub log: LogConfig,
  pub tasks: Vec<TaskConfig>,
  pub defaults: TaskConfig,
  pub tui: TuiConfig,
  pub keymap: KeymapConfig,
  pub on_init: Option<Hook>,
  pub on_idle: Option<Hook>,
  /// Non-fatal problems found while loading; the caller reports them.
  pub warnings: Vec<String>,
}

impl Config {
  pub fn make_default() -> Self {
    Self {
      runner: None,
      log: LogConfig::default(),
      tasks: Vec::new(),
      defaults: TaskConfig::default(),
      tui: TuiConfig::builtin(),
      keymap: KeymapConfig::default(),
      on_init: None,
      on_idle: None,
      warnings: Vec::new(),
    }
  }

  pub fn load_dir(root: &Path) -> Result<Config> {
    let mut config = Config::make_default();
    // A broken personal config must not keep a runner from starting.
    if let Err(error) = config.load_presentation() {
      config
        .warnings
        .push(format!("ignoring the user config: {error:#}"));
    }

    let path = root.join("dekit.yaml");
    if !path.exists() {
      return Ok(config);
    }
    let root = dunce::canonicalize(root)?;
    let mut stack = Vec::new();
    let mut task_paths = HashSet::new();
    config.load_file(&root, &path, "", true, &mut stack, &mut task_paths)?;
    Ok(config)
  }

  /// Reads only the root `kernel` declaration. Bootstrap stays compatible with
  /// project configs whose remaining fields require a newer pinned kernel.
  pub fn load_kernel(root: &Path) -> Result<Option<KernelConfig>> {
    let path = root.join("dekit.yaml");
    if !path.exists() {
      return Ok(None);
    }
    let cx = CfgCx::new(root.to_path_buf());
    let source = std::fs::read_to_string(&path)
      .with_context(|| format!("failed to load {}", path.display()))?;
    let value: serde_yaml::Value = serde_yaml::from_str(&source)
      .with_context(|| format!("failed to parse {}", path.display()))?;
    let root = value
      .as_mapping()
      .ok_or_else(|| anyhow::anyhow!("config root must be an object"))?;
    let Some(kernel) = root.get(serde_yaml::Value::from("kernel")) else {
      return Ok(None);
    };
    let mut selected = serde_yaml::Mapping::new();
    selected.insert(serde_yaml::Value::from("kernel"), kernel.clone());
    let doc = CfgDoc::from_value(serde_yaml::Value::Mapping(selected), &cx)?;
    parse_kernel(&doc.root().as_obj()?, &cx)
  }

  fn load_presentation(&mut self) -> Result<()> {
    // No user config dir (HOME/XDG unset) means no user config to load.
    let Ok(dir) = user_config_dir() else {
      return Ok(());
    };
    let path = dir.join("config.yaml");
    if !path.exists() {
      return Ok(());
    }
    let cx = CfgCx::new(dir);
    let doc = CfgDoc::load(&path, &cx)
      .with_context(|| format!("failed to load {}", path.display()))?;
    let obj = doc.root().as_obj()?;
    // Apply each section independently: one unknown or malformed key must
    // not drop the valid tui/keymap settings alongside it.
    for (key, _) in obj.iter() {
      if !PRESENTATION_KEYS.contains(&key) {
        self.warnings.push(format!(
          "ignoring unknown key '{key}' in {}",
          path.display()
        ));
      }
    }
    if obj.get("tui").is_some()
      && let Err(err) = self.tui.merge(&obj, &cx)
    {
      self
        .warnings
        .push(format!("ignoring 'tui' in {}: {err:#}", path.display()));
    }
    if obj.get("keymap").is_some()
      && let Err(err) = self.keymap.merge(&obj)
    {
      self
        .warnings
        .push(format!("ignoring 'keymap' in {}: {err:#}", path.display()));
    }
    Ok(())
  }

  fn load_file(
    &mut self,
    project_root: &Path,
    path: &Path,
    mount: &str,
    is_root: bool,
    stack: &mut Vec<PathBuf>,
    task_paths: &mut HashSet<String>,
  ) -> Result<()> {
    let path = dunce::canonicalize(path)
      .with_context(|| format!("failed to load config {}", path.display()))?;
    if !is_root
      && path.file_name().and_then(|name| name.to_str()) == Some("dekit.yaml")
    {
      bail!(
        "{} is a project declaration, not a config fragment; use a name such as dekit.fragment.yaml",
        path.display()
      );
    }
    if !path.starts_with(project_root) {
      bail!(
        "config fragment escapes the project root: {}",
        path.display()
      );
    }
    if let Some(at) = stack.iter().position(|entry| entry == &path) {
      let mut cycle = stack[at..]
        .iter()
        .map(|p| p.display().to_string())
        .collect::<Vec<_>>();
      cycle.push(path.display().to_string());
      bail!("config load cycle: {}", cycle.join(" -> "));
    }
    stack.push(path.clone());

    let dir = path.parent().expect("config path has a parent");
    let cx = CfgCx::new(dir.to_path_buf());
    let doc = CfgDoc::load(&path, &cx)
      .with_context(|| format!("failed to load {}", path.display()))?;
    let obj = doc.root().as_obj()?;
    if is_root {
      // Presentation settings live in the user config now; warn rather
      // than fail so a project file carrying them still loads.
      for key in PRESENTATION_KEYS {
        if obj.get(key).is_some() {
          self.warnings.push(format!(
            "'{key}' in {} is ignored; presentation settings moved to the user config.yaml",
            path.display()
          ));
        }
      }
      obj
        .known_keys(ROOT_KEYS)
        .with_context(|| format!("in {}", path.display()))?;
      let _ = parse_kernel(&obj, &cx)
        .with_context(|| format!("in {}", path.display()))?;
      self
        .apply_root(&obj, &cx)
        .with_context(|| format!("in {}", path.display()))?;
    } else {
      obj
        .known_keys(FRAGMENT_KEYS)
        .with_context(|| format!("in {}", path.display()))?;
    }
    self
      .load_tasks(&obj, &cx, mount, task_paths)
      .with_context(|| format!("in {}", path.display()))?;

    if let Some(load) = obj.get("load") {
      for entry in load.as_arr()?.iter() {
        let (pattern, child_mount) = parse_load_entry(&entry, &cx)?;
        let child_mount = join_task_path(mount, &child_mount);
        validate_mount(&child_mount, &entry)?;
        let base_path = dir.to_string_lossy().replace('\\', "/");
        let base = glob::Pattern::escape(&base_path);
        let pattern = format!("{base}/{}", pattern.replace('\\', "/"));
        let mut matches = glob::glob(&pattern)
          .map_err(|err| entry.error(format!("invalid load pattern: {err}")))?
          .filter_map(|path| match path {
            Ok(path) if path.is_file() => Some(Ok(path)),
            Ok(_) => None,
            Err(err) => Some(Err(err)),
          })
          .collect::<std::result::Result<Vec<_>, _>>()?;
        matches.sort();
        if matches.is_empty() {
          bail!(
            entry.error(format!("load pattern matched no files: {pattern}"))
          );
        }
        // The declaring file can match its own glob; don't reload it.
        // Matching only itself is fine (it loads nothing), not an error.
        for child in matches {
          let child = dunce::canonicalize(&child)?;
          if child != path {
            self.load_file(
              project_root,
              &child,
              &child_mount,
              false,
              stack,
              task_paths,
            )?;
          }
        }
      }
    }

    stack.pop();
    Ok(())
  }

  fn load_tasks(
    &mut self,
    obj: &CfgObj<'_>,
    cx: &CfgCx,
    mount: &str,
    task_paths: &mut HashSet<String>,
  ) -> Result<()> {
    let Some(tasks) = obj.get("tasks") else {
      return Ok(());
    };
    let tasks = tasks.as_obj()?;
    for (name, task) in tasks.iter() {
      let path = join_task_path(mount, name);
      if !task_paths.insert(path.clone()) {
        bail!("duplicate task path '{}'", path);
      }
      let mut config = task_from_cfg(path, &task, cx)?;
      for dep in &mut config.deps {
        if let Some(root_dep) = dep.strip_prefix('/') {
          *dep = root_dep.to_string();
        } else if !mount.is_empty() {
          *dep = join_task_path(mount, dep);
        }
      }
      self.tasks.push(config);
    }
    Ok(())
  }

  fn apply_root(&mut self, obj: &CfgObj<'_>, cx: &CfgCx) -> Result<()> {
    self.log.merge(obj, cx)?;
    if let Some(pd) = obj.get("defaults") {
      let over = parse_task_settings(&pd.as_obj()?, cx)?;
      self.defaults = std::mem::take(&mut self.defaults).overlay(over);
    }
    self.on_init = hook_from_cfg(obj, "on_init")?;
    self.on_idle = hook_from_cfg(obj, "on_idle")?;
    Ok(())
  }
}

fn parse_kernel(obj: &CfgObj<'_>, cx: &CfgCx) -> Result<Option<KernelConfig>> {
  let Some(node) = obj.get("kernel") else {
    return Ok(None);
  };
  if node.is_string() {
    return match node.as_str()? {
      "npm" => Ok(Some(KernelConfig::Npm)),
      value => bail!(node.error(format!(
        "unknown kernel '{value}'; expected 'npm' or {{path: ...}}"
      ))),
    };
  }
  let kernel = node.as_obj()?;
  kernel.known_keys(&["path"])?;
  let path: String = kernel.required("path", cx)?;
  Ok(Some(KernelConfig::Path(cx.resolve_path(&path))))
}

fn parse_load_entry(
  node: &CfgNode<'_>,
  cx: &CfgCx,
) -> Result<(String, String)> {
  if node.is_string() {
    return Ok((node.as_str()?.to_string(), String::new()));
  }
  let obj = node.as_obj()?;
  obj.known_keys(&["file", "at"])?;
  Ok((
    obj.required("file", cx)?,
    obj.default("at", String::new(), cx)?,
  ))
}

fn join_task_path(parent: &str, child: &str) -> String {
  match (parent.is_empty(), child.is_empty()) {
    (true, _) => child.to_string(),
    (_, true) => parent.to_string(),
    _ => format!("{parent}/{child}"),
  }
}

fn validate_mount(mount: &str, node: &CfgNode<'_>) -> Result<()> {
  if mount.is_empty() {
    return Ok(());
  }
  TaskPath::new(mount)
    .map(|_| ())
    .map_err(|err| node.error(format!("invalid load mount '{mount}': {err}")))
}

#[cfg(test)]
mod tests {
  use super::*;

  fn temp_project(name: &str) -> PathBuf {
    let path = std::env::temp_dir()
      .join(format!("dekit-config-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&path);
    std::fs::create_dir_all(&path).unwrap();
    path
  }

  #[test]
  fn loads_and_mounts_fragments() {
    let root = temp_project("mount");
    std::fs::create_dir(root.join("cfg")).unwrap();
    std::fs::write(
      root.join("dekit.yaml"),
      "load:\n  - file: cfg/web.yaml\n    at: packages/web\n",
    )
    .unwrap();
    std::fs::write(
      root.join("cfg/web.yaml"),
      "tasks:\n  db:\n    cmd: [echo, db]\n  dev:\n    cmd: [echo, dev]\n    deps: [db]\n",
    ).unwrap();
    let config = Config::load_dir(&root).unwrap();
    assert_eq!(config.tasks[0].path, "packages/web/db");
    assert_eq!(config.tasks[1].deps, ["packages/web/db"]);
    let _ = std::fs::remove_dir_all(root);
  }

  #[test]
  fn mounted_deps_can_explicitly_target_root() {
    let root = temp_project("root-dep");
    std::fs::write(
      root.join("dekit.yaml"),
      "load:\n  - file: web.yaml\n    at: web\ntasks:\n  db: {cmd: ['true']}\n",
    )
    .unwrap();
    std::fs::write(
      root.join("web.yaml"),
      "tasks:\n  db: {cmd: ['true']}\n  app:\n    cmd: ['true']\n    deps: [/db]\n",
    )
    .unwrap();
    let config = Config::load_dir(&root).unwrap();
    let app = config
      .tasks
      .iter()
      .find(|task| task.path == "web/app")
      .unwrap();
    assert_eq!(app.deps, ["db"]);
    let _ = std::fs::remove_dir_all(root);
  }

  #[test]
  fn mounted_deps_resolve_across_fragments_at_the_same_mount() {
    let root = temp_project("cross-fragment-dep");
    std::fs::write(
      root.join("dekit.yaml"),
      "load:\n  - {file: app.yaml, at: web}\n  - {file: db.yaml, at: web}\n",
    )
    .unwrap();
    std::fs::write(
      root.join("app.yaml"),
      "tasks:\n  app:\n    cmd: ['true']\n    deps: [db]\n",
    )
    .unwrap();
    std::fs::write(root.join("db.yaml"), "tasks:\n  db: {cmd: ['true']}\n")
      .unwrap();
    let config = Config::load_dir(&root).unwrap();
    let app = config
      .tasks
      .iter()
      .find(|task| task.path == "web/app")
      .unwrap();
    assert_eq!(app.deps, ["web/db"]);
    let _ = std::fs::remove_dir_all(root);
  }

  #[test]
  fn fragment_paths_are_relative_to_the_declaring_file() {
    let root = temp_project("relative-paths");
    let fragment_dir = root.join("packages/web");
    std::fs::create_dir_all(fragment_dir.join("bin")).unwrap();
    std::fs::write(fragment_dir.join("dev.js"), "export function main() {}\n")
      .unwrap();
    std::fs::write(
      root.join("dekit.yaml"),
      "load: [packages/web/tasks.yaml]\n",
    )
    .unwrap();
    std::fs::write(
      fragment_dir.join("tasks.yaml"),
      "tasks:\n  dev:\n    script: dev.js\n    cwd: .\n    add_path: [bin]\n",
    )
    .unwrap();
    let config = Config::load_dir(&root).unwrap();
    let task = &config.tasks[0];
    assert_eq!(
      dunce::canonicalize(task.cwd.as_ref().unwrap()).unwrap(),
      dunce::canonicalize(&fragment_dir).unwrap()
    );
    assert_eq!(
      dunce::canonicalize(&task.add_path.as_ref().unwrap()[0]).unwrap(),
      dunce::canonicalize(fragment_dir.join("bin")).unwrap()
    );
    let Some(crate::config::task::CmdConfig::Script { script, .. }) = &task.cmd
    else {
      panic!("expected script command")
    };
    assert_eq!(
      dunce::canonicalize(script).unwrap(),
      dunce::canonicalize(fragment_dir.join("dev.js")).unwrap()
    );
    let _ = std::fs::remove_dir_all(root);
  }

  #[test]
  fn load_globs_escape_the_project_path_and_ignore_directories() {
    let root = temp_project("[glob]");
    std::fs::create_dir_all(root.join("cfg/subdir.yaml")).unwrap();
    std::fs::write(root.join("dekit.yaml"), "load: [cfg/*.yaml]\n").unwrap();
    std::fs::write(
      root.join("cfg/tasks.yaml"),
      "tasks:\n  ok: {cmd: ['true']}\n",
    )
    .unwrap();
    let config = Config::load_dir(&root).unwrap();
    assert_eq!(config.tasks[0].path, "ok");
    let _ = std::fs::remove_dir_all(root);
  }

  #[test]
  fn kernel_bootstrap_ignores_directives_outside_kernel() {
    let root = temp_project("kernel-only");
    std::fs::write(
      root.join("dekit.yaml"),
      "kernel: {path: bin/dekit}\ntasks:\n  future:\n    cmd: {$js: 'future'}\n",
    )
    .unwrap();
    assert_eq!(
      Config::load_kernel(&root).unwrap(),
      Some(KernelConfig::Path(root.join("bin/dekit")))
    );
    let _ = std::fs::remove_dir_all(root);
  }

  #[test]
  fn glob_load_skips_the_declaring_file() {
    let root = temp_project("self-glob");
    std::fs::write(
      root.join("dekit.yaml"),
      "load: ['*.yaml']\ntasks:\n  a: {cmd: ['true']}\n",
    )
    .unwrap();
    std::fs::write(root.join("extra.yaml"), "tasks:\n  b: {cmd: ['true']}\n")
      .unwrap();
    let config = Config::load_dir(&root).unwrap();
    let mut paths: Vec<_> =
      config.tasks.iter().map(|t| t.path.as_str()).collect();
    paths.sort();
    assert_eq!(paths, ["a", "b"]);
    let _ = std::fs::remove_dir_all(root);
  }

  #[test]
  fn glob_matching_only_the_declaring_file_loads_nothing() {
    let root = temp_project("only-self-glob");
    std::fs::write(
      root.join("dekit.yaml"),
      "load: ['*.yaml']\ntasks:\n  a: {cmd: ['true']}\n",
    )
    .unwrap();
    // The glob matches only dekit.yaml itself: not an error, loads no
    // fragments, and the declared task still registers.
    let config = Config::load_dir(&root).unwrap();
    assert_eq!(config.tasks.len(), 1);
    assert_eq!(config.tasks[0].path, "a");
    let _ = std::fs::remove_dir_all(root);
  }

  #[test]
  fn project_presentation_keys_warn_and_load_continues() {
    let root = temp_project("presentation");
    std::fs::write(
      root.join("dekit.yaml"),
      "tui: {}\ntasks:\n  a: {cmd: ['true']}\n",
    )
    .unwrap();
    let config = Config::load_dir(&root).unwrap();
    assert_eq!(config.tasks[0].path, "a");
    assert!(
      config.warnings.iter().any(|w| w.contains("'tui'")),
      "{:?}",
      config.warnings
    );
    let _ = std::fs::remove_dir_all(root);
  }

  #[test]
  fn rejects_duplicate_mounted_paths() {
    let root = temp_project("duplicate");
    std::fs::write(root.join("dekit.yaml"), "load: [a.yaml, b.yaml]\n")
      .unwrap();
    std::fs::write(root.join("a.yaml"), "tasks:\n  x: {cmd: ['true']}\n")
      .unwrap();
    std::fs::write(root.join("b.yaml"), "tasks:\n  x: {cmd: ['true']}\n")
      .unwrap();
    let err = match Config::load_dir(&root) {
      Ok(_) => panic!("expected duplicate task error"),
      Err(err) => format!("{err:#}"),
    };
    assert!(err.contains("duplicate task path 'x'"), "{err}");
    let _ = std::fs::remove_dir_all(root);
  }

  #[test]
  fn rejects_fragment_load_cycles() {
    let root = temp_project("cycle");
    std::fs::write(root.join("dekit.yaml"), "load: [a.yaml]\n").unwrap();
    std::fs::write(root.join("a.yaml"), "load: [b.yaml]\n").unwrap();
    std::fs::write(root.join("b.yaml"), "load: [a.yaml]\n").unwrap();
    let err = match Config::load_dir(&root) {
      Ok(_) => panic!("expected load cycle error"),
      Err(err) => err.to_string(),
    };
    assert!(err.contains("config load cycle"), "{err}");
    let _ = std::fs::remove_dir_all(root);
  }
}
