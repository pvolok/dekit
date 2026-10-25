use std::collections::HashSet;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use crate::cfg::{CfgCx, CfgDoc, CfgNode, CfgObj};
use crate::config::folder::{FolderConfig, folder_from_cfg, folder_key};
use crate::config::hook::{Hook, hook_from_cfg};
use crate::config::js;
use crate::config::keymap::KeymapConfig;
use crate::config::log::LogConfig;
use crate::config::task::{TaskConfig, parse_task_settings, task_from_cfg};
use crate::config::tui::TuiConfig;
use crate::kernel::task::{RestartMode, TaskKind};
use crate::kernel::task_path::{TaskPath, join};
use crate::runner::{RunnerSpec, user_config_dir};

pub(crate) const ROOT_KEYS: &[&str] = &[
  "kernel", "load", "log", "defaults", "vars", "on_init", "on_idle", "tasks",
  "tui", "keymap",
];
const FRAGMENT_KEYS: &[&str] = &["load", "tasks"];
/// Root keys that moved to the user config.
pub(crate) const PRESENTATION_KEYS: &[&str] = &["tui", "keymap"];
const USER_KEYS: &[&str] = &["tui", "keymap", "agent"];

/// Where a change can change a config: a directory, whether what is
/// deeper under it counts too, and what a changed path there must match.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Watched {
  pub dir: PathBuf,
  pub recursive: bool,
  /// An absolute glob pattern.
  pub pattern: String,
}

impl Watched {
  fn file(path: &Path) -> Watched {
    Watched {
      dir: path.parent().unwrap_or(path).to_path_buf(),
      recursive: false,
      pattern: glob::Pattern::escape(&path.to_string_lossy()),
    }
  }

  /// A `load` pattern from `dir`: under the directory its plain parts
  /// name, and deeper when a part before the last has a wildcard, or the
  /// last is `**`, which matches files at any depth. The directory is as
  /// file events name it: no `.` or `..`, links resolved.
  fn load(dir: &Path, pattern: &str) -> Watched {
    let parts: Vec<&str> = pattern.split('/').collect();
    let (last, dirs) = parts.split_last().expect("a split has a part");
    let plain = dirs
      .iter()
      .take_while(|part| !part.contains(['*', '?', '[']))
      .count();
    let mut base = dir.to_path_buf();
    for part in &dirs[..plain] {
      match *part {
        "" | "." => (),
        ".." => {
          base.pop();
        }
        part => base.push(part),
      }
    }
    let base = dunce::canonicalize(&base).unwrap_or(base);
    let escaped =
      glob::Pattern::escape(&base.to_string_lossy().replace('\\', "/"));
    Watched {
      pattern: format!("{escaped}/{}", parts[plain..].join("/")),
      dir: base,
      recursive: plain < dirs.len() || *last == "**",
    }
  }
}

/// What went wrong in one config file, and which: every error reading
/// one is in one of these, so it shows as `in <file>: ...` and a reader
/// can name the file without parsing the message.
#[derive(Debug)]
pub struct InFile {
  pub file: PathBuf,
  pub error: anyhow::Error,
}

impl std::fmt::Display for InFile {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    write!(f, "in {}", self.file.display())
  }
}

impl std::error::Error for InFile {
  fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
    Some(self.error.as_ref())
  }
}

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
  /// In the config's order; `at` places each among the tasks.
  pub folders: Vec<FolderConfig>,
  pub defaults: TaskConfig,
  pub tui: TuiConfig,
  pub keymap: KeymapConfig,
  pub on_init: Option<Hook>,
  pub on_idle: Option<Hook>,
  /// mprocs.yaml: a line typed to add a task runs through the system shell
  /// (`/bin/sh`, PowerShell on Windows) as mprocs ran it.
  pub system_shell: bool,
  /// Non-fatal problems found while loading; the caller reports them.
  pub warnings: Vec<String>,
  /// Where a change can change this config: dekit.yaml, its fragments,
  /// and where its `load` patterns look.
  pub watched: Vec<Watched>,
  /// The coding agent the console hands work to, as an argv the prompt
  /// is appended to (user config `agent: {cmd: [...]}`).
  pub agent: Option<Vec<String>>,
}

/// What one load carries from file to file.
struct Load {
  /// The project root, which every file must be in.
  root: PathBuf,
  /// The files being loaded, from dekit.yaml down, to catch a cycle.
  stack: Vec<PathBuf>,
  task_paths: HashSet<String>,
  js: js::Scope,
}

/// An entry under `tasks:`.
#[derive(Clone, Copy)]
pub enum Entry<'a> {
  Task(&'a TaskConfig),
  Folder(&'a FolderConfig),
}

impl Entry<'_> {
  pub fn source(&self) -> Option<&crate::spec::Source> {
    match self {
      Entry::Task(task) => task.source.as_ref(),
      Entry::Folder(folder) => folder.source.as_ref(),
    }
  }

  pub fn path(&self) -> &str {
    match self {
      Entry::Task(task) => &task.path,
      Entry::Folder(folder) => &folder.path,
    }
  }
}

impl Config {
  /// The task and folder entries, in the config's order.
  pub fn entries(&self) -> Vec<Entry<'_>> {
    let mut folders = self.folders.iter().peekable();
    let mut entries = Vec::with_capacity(self.tasks.len() + self.folders.len());
    for (i, task) in self.tasks.iter().enumerate() {
      while let Some(folder) = folders.next_if(|folder| folder.at <= i) {
        entries.push(Entry::Folder(folder));
      }
      entries.push(Entry::Task(task));
    }
    entries.extend(folders.map(Entry::Folder));
    entries
  }

  /// Where the task and folder entries are, in the config's order.
  pub fn sources(&self) -> Vec<crate::spec::Source> {
    self
      .entries()
      .iter()
      .filter_map(|entry| entry.source().cloned())
      .collect()
  }

  pub fn make_default() -> Self {
    Self {
      runner: None,
      log: LogConfig::default(),
      tasks: Vec::new(),
      folders: Vec::new(),
      defaults: TaskConfig::default(),
      tui: TuiConfig::builtin(),
      keymap: KeymapConfig::default(),
      on_init: None,
      on_idle: None,
      system_shell: false,
      warnings: Vec::new(),
      watched: Vec::new(),
      agent: None,
    }
  }

  /// The config of `runner`'s project, as that runner reads it.
  pub fn load(runner: &RunnerSpec) -> Result<Config> {
    Config::try_load(runner).map_err(|(err, _)| err)
  }

  /// `load`, but a config that does not load still says what it was read
  /// from up to the error, where a fix would be made.
  pub fn try_load(
    runner: &RunnerSpec,
  ) -> std::result::Result<Config, (anyhow::Error, Vec<Watched>)> {
    let mut config = Config::make_default();
    match config.read_dir(runner) {
      Ok(()) => {
        config.runner = Some(runner.clone());
        Ok(config)
      }
      Err(err) => Err((err, config.watched)),
    }
  }

  /// `load` off the async workers, since the `!js` values run as it
  /// reads.
  pub async fn load_blocking(runner: &RunnerSpec) -> Result<Config> {
    let runner = runner.clone();
    tokio::task::spawn_blocking(move || Config::load(&runner)).await?
  }

  /// `load` for a project runner at `root`.
  #[cfg(test)]
  pub fn load_dir(root: &Path) -> Result<Config> {
    Config::load(&RunnerSpec {
      kind: crate::runner::RunnerKind::Project,
      root: dunce::canonicalize(root).unwrap_or_else(|_| root.into()),
    })
  }

  fn read_dir(&mut self, runner: &RunnerSpec) -> Result<()> {
    // A broken personal config must not keep a runner from starting.
    if let Err(error) = self.load_presentation() {
      self
        .warnings
        .push(format!("ignoring the user config: {error:#}"));
    }

    let root = &runner.root;
    let root = dunce::canonicalize(root).unwrap_or_else(|_| root.into());
    let path = root.join("dekit.yaml");
    // One made later is the config too.
    self.watch_file(&path);
    if !path.exists() {
      return Ok(());
    }
    let mut load = Load {
      root: root.clone(),
      stack: Vec::new(),
      task_paths: HashSet::new(),
      js: js::Scope::new(runner),
    };
    self.load_file(&mut load, &path, "", true)?;
    let task_paths = load.task_paths;
    for task in &self.tasks {
      let path = task.path.as_str();
      if let Some(above) = path
        .match_indices('/')
        .map(|(i, _)| &path[..i])
        .find(|prefix| task_paths.contains(*prefix))
      {
        bail!("a task can't have tasks under it: '{path}' is under '{above}'");
      }
    }
    let mut folders = HashSet::new();
    for folder in &self.folders {
      let path = folder.path.as_str();
      if !folders.insert(path) {
        bail!("duplicate folder '{path}/'");
      }
      if task_paths.contains(path) {
        bail!("'{path}' is a task, so it can't be a folder too");
      }
      if let Some(above) = path
        .match_indices('/')
        .map(|(i, _)| &path[..i])
        .find(|prefix| task_paths.contains(*prefix))
      {
        bail!(
          "a task can't have tasks under it: folder '{path}/' is under \
           '{above}'"
        );
      }
    }
    if self.defaults.autorestart == Some(RestartMode::Always)
      && let Some(job) = self.tasks.iter().find(|task| match task.kind {
        TaskKind::Service => false,
        TaskKind::Job => task.autorestart.is_none(),
      })
    {
      bail!(
        "task '{}' is a job and can't use 'autorestart: always' from defaults",
        job.path
      );
    }
    Ok(())
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
    let doc = CfgDoc::from_value(serde_yaml::Value::Mapping(selected));
    parse_kernel(&doc.root().as_obj()?, &cx)
  }

  fn load_presentation(&mut self) -> Result<()> {
    // No user config dir (HOME/XDG unset) means no user config to load.
    let Ok(dir) = user_config_dir() else {
      return Ok(());
    };
    let dir = dunce::canonicalize(&dir).unwrap_or(dir);
    let path = dir.join("config.yaml");
    // Watched like dekit.yaml: an edit to it is a change to the config,
    // and one made later is the config too.
    self.watch_file(&path);
    if !path.exists() {
      return Ok(());
    }
    let cx = CfgCx::new(dir);
    let doc = CfgDoc::load(&path)
      .with_context(|| format!("failed to load {}", path.display()))?;
    let obj = doc.root().as_obj()?;
    // Apply each section independently: one unknown or malformed key must
    // not drop the valid tui/keymap settings alongside it.
    for (key, _) in obj.iter() {
      if !USER_KEYS.contains(&key) {
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
    if let Some(agent) = obj.get("agent") {
      let cmd = agent.as_obj().and_then(|agent| {
        agent.known_keys(&["cmd"])?;
        match agent.get("cmd") {
          Some(cmd) => crate::config::task::argv_from_cfg(&cmd).map(Some),
          None => Ok(None),
        }
      });
      match cmd {
        Ok(cmd) => self.agent = cmd,
        Err(err) => self
          .warnings
          .push(format!("ignoring 'agent' in {}: {err:#}", path.display())),
      }
    }
    Ok(())
  }

  fn load_file(
    &mut self,
    load: &mut Load,
    path: &Path,
    mount: &str,
    is_root: bool,
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
    if !path.starts_with(&load.root) {
      bail!(
        "config fragment escapes the project root: {}",
        path.display()
      );
    }
    if let Some(at) = load.stack.iter().position(|entry| entry == &path) {
      let mut cycle = load.stack[at..]
        .iter()
        .map(|p| p.display().to_string())
        .collect::<Vec<_>>();
      cycle.push(path.display().to_string());
      bail!("config load cycle: {}", cycle.join(" -> "));
    }
    load.stack.push(path.clone());
    self.watch_file(&path);
    let children =
      self
        .read_file(load, &path, mount, is_root)
        .map_err(|error| {
          anyhow::Error::new(InFile {
            file: path.clone(),
            error,
          })
        })?;
    for (child, child_mount) in children {
      self.load_file(load, &child, &child_mount, false)?;
    }
    load.stack.pop();
    Ok(())
  }

  /// One file's settings and tasks; returns the files its `load` names,
  /// with the path each is mounted at.
  fn read_file(
    &mut self,
    load: &mut Load,
    path: &Path,
    mount: &str,
    is_root: bool,
  ) -> Result<Vec<(PathBuf, String)>> {
    let dir = path.parent().expect("config path has a parent");
    let cx = CfgCx::new(dir.to_path_buf());
    let mut doc = CfgDoc::load(path)?;
    // The keys first, so a typo is named as one and not by what a `!js`
    // under it does.
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
      obj.known_keys(ROOT_KEYS)?;
      load.js.read_vars(doc.root().raw(), dir)?;
    } else {
      if let Some(vars) = obj.get("vars") {
        bail!(vars.error(
          "'vars' are set in dekit.yaml, and a fragment reads them as ctx.vars"
        ));
      }
      obj.known_keys(FRAGMENT_KEYS)?;
    }
    // `kernel` is the launcher's, which refuses a tag there.
    load.js.resolve_keys(doc.value_mut(), dir, |key| {
      key != "kernel" && key != "vars" && !PRESENTATION_KEYS.contains(&key)
    })?;
    let obj = doc.root().as_obj()?;
    if is_root {
      let _ = parse_kernel(&obj, &cx)?;
      self.apply_root(&obj, &cx)?;
    }
    let file = path
      .strip_prefix(&load.root)
      .unwrap_or(path)
      .to_string_lossy()
      .replace('\\', "/");
    self.load_tasks(&obj, &cx, &file, mount, &mut load.task_paths)?;

    let mut children = Vec::new();
    if let Some(load) = obj.get("load") {
      for entry in load.as_arr()?.iter() {
        let (pattern, child_mount) = parse_load_entry(&entry, &cx)?;
        let child_mount = join(mount, &child_mount);
        validate_mount(&child_mount, &entry)?;
        let watched = Watched::load(dir, &pattern.replace('\\', "/"));
        let pattern = watched.pattern.clone();
        self.watch(watched);
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
            children.push((child, child_mount.clone()));
          }
        }
      }
    }
    Ok(children)
  }

  fn watch_file(&mut self, path: &Path) {
    self.watch(Watched::file(path));
  }

  fn watch(&mut self, watched: Watched) {
    if !self.watched.contains(&watched) {
      self.watched.push(watched);
    }
  }

  fn load_tasks(
    &mut self,
    obj: &CfgObj<'_>,
    cx: &CfgCx,
    file: &str,
    mount: &str,
    task_paths: &mut HashSet<String>,
  ) -> Result<()> {
    let Some(tasks) = obj.get("tasks") else {
      return Ok(());
    };
    let tasks = tasks.as_obj()?;
    for (name, task) in tasks.iter() {
      if let Some(folder) = folder_key(name) {
        if folder.is_empty() {
          bail!(task.error("a folder needs a name before its '/'"));
        }
        let mut config = folder_from_cfg(join(mount, folder), &task, cx)?;
        config.source = Some(crate::spec::Source {
          file: file.to_string(),
          key: name.to_string(),
          mount: mount.to_string(),
        });
        config.at = self.tasks.len();
        self.folders.push(config);
        continue;
      }
      let path = join(mount, name);
      if !task_paths.insert(path.clone()) {
        bail!("duplicate task path '{}'", path);
      }
      let mut config = task_from_cfg(path, &task, cx)?;
      config.source = Some(crate::spec::Source {
        file: file.to_string(),
        key: name.to_string(),
        mount: mount.to_string(),
      });
      for dep in &mut config.deps {
        if let Some(root_dep) = dep.strip_prefix('/') {
          *dep = root_dep.to_string();
        } else if !mount.is_empty() {
          *dep = join(mount, dep);
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
  if js::has_tag(node.raw()) {
    bail!(
      node.error("'kernel' can't use !js: it picks the dekit that runs it")
    );
  }
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
  Ok(Some(KernelConfig::Path(kernel.required("path", cx)?)))
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

  /// A `load` pattern is watched where it can match: under its plain
  /// directories, and deeper when a wildcard is in a directory part.
  #[test]
  fn load_patterns_are_watched_where_they_can_match() {
    let dir = Path::new("/p");
    let watched = |pattern| {
      let w = Watched::load(dir, pattern);
      (w.dir, w.recursive, w.pattern)
    };
    assert_eq!(watched("a.yaml"), ("/p".into(), false, "/p/a.yaml".into()));
    assert_eq!(
      watched("cfg/*.yaml"),
      ("/p/cfg".into(), false, "/p/cfg/*.yaml".into())
    );
    assert_eq!(watched("apps/*/tasks.yaml").0, Path::new("/p/apps"));
    assert!(watched("apps/*/tasks.yaml").1);
    assert_eq!(watched("**/tasks.yaml").0, Path::new("/p"));
    assert!(watched("**/tasks.yaml").1);
    // A last part `**` matches files at any depth under the directory.
    assert_eq!(
      watched("cfg/**"),
      ("/p/cfg".into(), true, "/p/cfg/**".into())
    );
    assert!(!watched("cfg/*").1);
    assert_eq!(
      watched("./a/../../q/*.yaml"),
      ("/q".into(), false, "/q/*.yaml".into())
    );
  }

  /// A config that does not load still says what it was read from, so a
  /// fix to the fragment that broke it is seen.
  #[test]
  fn a_config_that_does_not_load_says_what_it_read() {
    let root = temp_project("broken-fragment");
    std::fs::write(root.join("dekit.yaml"), "load: [b.yaml]\n").unwrap();
    std::fs::write(root.join("b.yaml"), "tasks: [\n").unwrap();
    let runner = RunnerSpec {
      kind: crate::runner::RunnerKind::Project,
      root: root.clone(),
    };
    let Err((_, watched)) = Config::try_load(&runner) else {
      panic!("b.yaml is broken");
    };
    let b = dunce::canonicalize(root.join("b.yaml")).unwrap();
    assert!(
      watched
        .iter()
        .any(|w| glob::Pattern::new(&w.pattern).unwrap().matches_path(&b)),
      "{watched:?}"
    );
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
    // Never a project key: unknown there, not moved.
    std::fs::write(root.join("dekit.yaml"), "agent: {cmd: [claude]}\n")
      .unwrap();
    let err = Config::load_dir(&root).err().map(|err| format!("{err:#}"));
    assert!(err.as_ref().is_some_and(|e| e.contains("agent")), "{err:?}");
    let _ = std::fs::remove_dir_all(root);
  }

  #[test]
  fn job_refuses_autorestart_always_from_defaults() {
    let root = temp_project("job-restart");
    let load_err = |yaml: &str| {
      std::fs::write(root.join("dekit.yaml"), yaml).unwrap();
      match Config::load_dir(&root) {
        Ok(_) => panic!("expected an error for {yaml}"),
        Err(err) => format!("{err:#}"),
      }
    };
    let err = load_err(
      "defaults: {autorestart: always}\ntasks:\n  migrate: {cmd: ['true'], type: job}\n",
    );
    assert!(
      err.contains(
        "task 'migrate' is a job and can't use 'autorestart: always'"
      ),
      "{err}"
    );
    let err = load_err("defaults: {restart: on-failure}\n");
    assert!(err.contains("use 'autorestart: on-failure'"), "{err}");
    // The job's own autorestart wins over the default.
    std::fs::write(
      root.join("dekit.yaml"),
      "defaults: {autorestart: always}\ntasks:\n  migrate: {cmd: ['true'], type: job, autorestart: never}\n",
    )
    .unwrap();
    Config::load_dir(&root).unwrap();
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
  fn rejects_a_task_under_another() {
    let root = temp_project("nested");
    std::fs::write(
      root.join("dekit.yaml"),
      "tasks:\n  web: {cmd: ['true']}\nload:\n  - file: web.yaml\n    at: web\n",
    )
    .unwrap();
    std::fs::write(root.join("web.yaml"), "tasks:\n  dev: {cmd: ['true']}\n")
      .unwrap();
    let err = match Config::load_dir(&root) {
      Ok(_) => panic!("expected a nested task error"),
      Err(err) => format!("{err:#}"),
    };
    assert_eq!(
      err,
      "a task can't have tasks under it: 'web/dev' is under 'web'"
    );
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

  /// A key that ends in `/` is a folder, in its place among the tasks and
  /// under the file's mount.
  #[test]
  fn folder_entries_load_in_place() {
    let root = temp_project("folders");
    std::fs::write(
      root.join("dekit.yaml"),
      "load: [{file: web.yaml, at: web}]\ntasks:\n  db: {cmd: ['true']}\n  apps/:\n    collapsed: true\n  apps/api: {cmd: ['true']}\n  tools/:\n",
    )
    .unwrap();
    std::fs::write(
      root.join("web.yaml"),
      "tasks:\n  ui/: {}\n  ui/dev: {cmd: ['true']}\n",
    )
    .unwrap();
    let config = Config::load_dir(&root).unwrap();
    let entries: Vec<String> = config
      .entries()
      .iter()
      .map(|entry| match entry {
        Entry::Task(task) => task.path.clone(),
        Entry::Folder(folder) => format!("{}/", folder.path),
      })
      .collect();
    assert_eq!(
      entries,
      ["db", "apps/", "apps/api", "tools/", "web/ui/", "web/ui/dev"]
    );
    assert!(config.folders[0].collapsed);
    assert!(!config.folders[1].collapsed);
    let ui = config.folders[2].source.clone().unwrap();
    assert_eq!(
      (ui.file.as_str(), ui.key.as_str(), ui.mount.as_str()),
      ("web.yaml", "ui/", "web")
    );
    assert_eq!(config.sources().len(), 6);

    let load_err = |yaml: &str| {
      std::fs::write(root.join("dekit.yaml"), yaml).unwrap();
      match Config::load_dir(&root) {
        Ok(_) => panic!("expected an error for {yaml}"),
        Err(err) => format!("{err:#}"),
      }
    };
    for (yaml, expected) in [
      (
        "tasks:\n  db: {cmd: x}\n  db/: {}\n",
        "'db' is a task, so it can't be a folder too",
      ),
      (
        "tasks:\n  db: {cmd: x}\n  db/x/: {}\n",
        "folder 'db/x/' is under 'db'",
      ),
      ("tasks:\n  /: {}\n", "a folder needs a name before its '/'"),
      ("tasks:\n  a b/: {}\n", "folder 'a b/': invalid task path"),
      ("tasks:\n  apps/: {cmd: x}\n", "unknown field 'cmd'"),
      (
        "load: [{file: web.yaml, at: apps}]\ntasks:\n  apps/ui/: {}\n",
        "duplicate folder 'apps/ui/'",
      ),
    ] {
      let err = load_err(yaml);
      assert!(err.contains(expected), "{yaml}: {err}");
    }
    let _ = std::fs::remove_dir_all(root);
  }

  fn env<'a>(task: &'a TaskConfig, name: &str) -> &'a str {
    task.env.as_ref().unwrap()[name].as_deref().unwrap()
  }

  /// A `!js` value is computed where it is written, from `ctx`: the
  /// file's directory, the runner, and the vars, read in order.
  #[test]
  fn js_values_load_in_place() {
    let root = temp_project("js");
    std::fs::create_dir(root.join("web")).unwrap();
    std::fs::write(
      root.join("dekit.yaml"),
      r#"vars:
  data: !js std.path.join(ctx.runner.root, ".data")
  db: !js ctx.vars.data + "/db"
  ports: {api: !js 3000 + 1, web: !js 3000}
load: [{file: web/tasks.yaml, at: web}]
defaults:
  env: {DATA: !js ctx.vars.data}
tasks:
  api:
    cmd: !js '["serve", "--port", String(ctx.vars.ports.api)]'
    env:
      FOO_DIR: !js std.path.join(ctx.dir, "foo/bar")
    scrollback_len: !js 500 * 2
    autostart: !js false
"#,
    )
    .unwrap();
    std::fs::write(
      root.join("web/tasks.yaml"),
      r#"tasks:
  dev:
    cmd: npm run dev
    env:
      HERE: !js ctx.dir
      ROOT: !js ctx.runner.root
      KIND: !js ctx.runner.kind
      DB: !js ctx.vars.db
      PORT: !js String(ctx.vars.ports.web)
"#,
    )
    .unwrap();
    let config = Config::load_dir(&root).unwrap();
    let root = dunce::canonicalize(&root).unwrap();
    let data = root.join(".data").to_string_lossy().into_owned();
    assert_eq!(
      config.defaults.env.as_ref().unwrap()["DATA"],
      Some(data.clone())
    );
    let api = &config.tasks[0];
    assert!(matches!(
      &api.cmd,
      Some(crate::config::task::CmdConfig::Cmd { cmd }) if cmd == &["serve", "--port", "3001"]
    ));
    assert_eq!(env(api, "FOO_DIR"), root.join("foo/bar").to_string_lossy());
    assert_eq!(api.scrollback_len, Some(1000));
    assert_eq!(api.autostart, Some(false));
    let dev = &config.tasks[1];
    assert_eq!(dev.path, "web/dev");
    assert_eq!(env(dev, "HERE"), root.join("web").to_string_lossy());
    assert_eq!(env(dev, "ROOT"), root.to_string_lossy());
    assert_eq!(env(dev, "KIND"), "project");
    assert_eq!(env(dev, "DB"), format!("{data}/db"));
    assert_eq!(env(dev, "PORT"), "3000");
    let _ = std::fs::remove_dir_all(root);
  }

  #[test]
  fn js_errors_name_the_value() {
    let root = temp_project("js-errors");
    std::fs::write(root.join("web.yaml"), "vars: {a: 1}\n").unwrap();
    let load_err = |yaml: &str| {
      std::fs::write(root.join("dekit.yaml"), yaml).unwrap();
      match Config::load_dir(&root) {
        Ok(_) => panic!("expected an error for {yaml}"),
        Err(err) => format!("{err:#}"),
      }
    };
    for (yaml, expected) in [
      (
        "tasks:\n  api: {cmd: !js dri}\n",
        "!js: ReferenceError: dri is not defined at <config>.tasks.api.cmd",
      ),
      (
        "tasks:\n  api: {cmd: x, autostart: !js \"'yes'\"}\n",
        "expected bool at <config>.tasks.api.autostart",
      ),
      // A var sees only the ones before it.
      (
        "vars:\n  a: !js ctx.vars.b\n  b: 1\n",
        "!js: returned undefined, which is not data at <config>.vars.a",
      ),
      ("vars: [1]\n", "expected object at <config>.vars"),
      ("vars: !js '[1]'\n", "expected object at <config>.vars"),
      (
        "load: [web.yaml]\n",
        "'vars' are set in dekit.yaml, and a fragment reads them as ctx.vars",
      ),
      ("kernel: !js \"'npm'\"\n", "'kernel' can't use !js"),
      ("kernel: {path: !js ctx.dir}\n", "'kernel' can't use !js"),
      (
        "tasks:\n  api: {cmd: !shell x}\n",
        "unknown tag !shell; the one tag dekit reads is !js at <config>.tasks.api.cmd",
      ),
      (
        "tasks:\n  api: {cmd: !js [x]}\n",
        "!js takes an expression, written as a string, at <config>.tasks.api.cmd",
      ),
      (
        "tasks:\n  api:\n    cmd: !js\n",
        "!js has no expression at <config>.tasks.api.cmd",
      ),
      (
        "tasks:\n  ? !js \"'api'\"\n  : {cmd: x}\n",
        "a key can't have a tag at <config>.tasks",
      ),
      // A typo is named as one, whatever is under it.
      (
        "tsks:\n  api: {cmd: !js dri}\n",
        "unknown field 'tsks' at <config>; did you mean 'tasks'?",
      ),
      (
        "tasks:\n  api: {cmd: x, cwd: <CONFIG_DIR>/api}\n",
        "'<CONFIG_DIR>' is not supported: a relative path is from the file's \
         directory already at <config>.tasks.api.cwd",
      ),
    ] {
      let err = load_err(yaml);
      assert!(err.contains(expected), "{yaml}: {err}");
    }
    // The launcher reads `kernel` alone, and refuses it the same way.
    std::fs::write(root.join("dekit.yaml"), "kernel: !js \"'npm'\"\n").unwrap();
    let err = format!("{:#}", Config::load_kernel(&root).unwrap_err());
    assert!(err.contains("'kernel' can't use !js"), "{err}");
    let _ = std::fs::remove_dir_all(root);
  }

  /// `load`, the hooks, and `vars` as one expression take `!js` too; the
  /// presentation keys dekit.yaml ignores are not evaluated; `ctx.runner`
  /// is the runner that loads.
  #[test]
  fn js_values_under_load_hooks_and_a_host_runner() {
    let root = temp_project("js-keys");
    std::fs::write(
      root.join("dekit.yaml"),
      r#"vars: !js '{kind: ctx.runner.kind}'
load: [!js '"web" + ".yaml"']
on_idle: {command: start, target: !js '"+" + ctx.vars.kind'}
tui: {theme: !js dri}
"#,
    )
    .unwrap();
    std::fs::write(
      root.join("web.yaml"),
      "tasks:\n  web: {cmd: x, env: {KIND: !js ctx.runner.kind}}\n",
    )
    .unwrap();
    let runner = RunnerSpec {
      kind: crate::runner::RunnerKind::Host,
      root: dunce::canonicalize(&root).unwrap(),
    };
    let config = Config::load(&runner).unwrap();
    assert_eq!(config.runner.as_ref(), Some(&runner));
    assert_eq!(env(&config.tasks[0], "KIND"), "host");
    match &config.on_idle {
      Some(Hook::Command(crate::command::Command::Start { target })) => {
        assert_eq!(target.to_string(), "+host")
      }
      _ => panic!("on_idle is not a start"),
    }
    assert!(
      config.warnings.iter().any(|w| w.contains("'tui'")),
      "{:?}",
      config.warnings
    );
    let _ = std::fs::remove_dir_all(root);
  }
}
