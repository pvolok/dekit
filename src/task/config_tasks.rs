use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::time::Duration;

use anyhow::bail;

use crate::{
  config::{
    config::Config,
    stop_signal::StopConfig,
    task::{AUTOSTART_TAG, CmdConfig, DYNAMIC_TAG, TaskConfig},
    task_log::{LogMode, TaskLogConfig},
  },
  kernel::{
    kernel_message::{TaskContext, TaskRegistration, TaskSelector},
    task::{RestartMode, STOP_TIMEOUT, TaskId, TaskKind},
    task_key::TaskKey,
    task_path::TaskPath,
  },
  spec::{Base, Source, TaskSpec},
  task::{
    process_task::{
      RunCx, Sig, StopSignal, process_task, process_task_resumed,
    },
    ready::{Probe, ReadyCheck, ReadyConfig, parse_http_url},
  },
  upgrade::snapshot as snap,
};

/// The config's tasks, the autostart ones pinned so they start at once:
/// how `dekit mprocs` starts.
pub async fn register_config_tasks(
  config: &Config,
  pc: &TaskContext,
) -> anyhow::Result<()> {
  register_config(config, pc, true).await
}

/// The config's tasks, none pinned: a dekit runner starts nothing until
/// asked (`up` starts the autostart ones).
pub async fn register_idle_config_tasks(
  config: &Config,
  pc: &TaskContext,
) -> anyhow::Result<()> {
  register_config(config, pc, false).await
}

/// The tasks a runner saved when it quit, as they were saved: their
/// definitions, origins, deps, and order, each idle and unpinned with a
/// fresh id around its saved screen. A task that was started gets a saved
/// pin, so `up` starts it again. Nothing comes from the config: a reload
/// brings in what changed there. Returns what could not be restored, and
/// the base for the reload (`saved_base`) without the entries of those,
/// so that it offers them again.
pub async fn register_saved_tasks(
  config: &Config,
  pc: &TaskContext,
  snapshot: &snap::Snapshot,
) -> anyhow::Result<(Vec<String>, Base)> {
  // As a dep names it: outside the default space, with its space.
  let name = |saved: &snap::Task| match saved.key() {
    Ok(Some(key)) => key.to_string(),
    Ok(None) | Err(_) => saved.path.clone().unwrap_or(saved.id.to_string()),
  };
  let process: Vec<(&snap::Task, &snap::ProcessTask)> = snapshot
    .tasks
    .iter()
    .filter_map(|task| match &task.kind {
      snap::TaskKind::Process(process) => Some((task, process)),
      snap::TaskKind::Console {} => None,
    })
    .collect();
  let new_id: HashMap<usize, TaskId> = process
    .iter()
    .map(|(saved, _)| (saved.id, pc.alloc_id()))
    .collect();
  let mut warnings = Vec::new();
  let mut dropped: HashMap<TaskId, String> = HashMap::new();
  let mut lost: HashSet<Source> = HashSet::new();
  let mut listed = Vec::new();
  let mut registrations = Vec::new();
  for (saved, process) in process {
    let mut deps = Vec::new();
    for dep in &saved.deps {
      match new_id.get(dep) {
        Some(id) => deps.push(TaskSelector::Id(*id)),
        None => warnings.push(format!(
          "saved task {}: dropped a dependency that was not restored",
          name(saved)
        )),
      }
    }
    let task_id = new_id[&saved.id];
    match saved_task_registration(config, task_id, saved, process, deps, None) {
      Ok(registration) => {
        listed.push(saved);
        registrations.push(with_saved_pin(registration, saved));
      }
      Err(err) => {
        warnings
          .push(format!("saved task {} not restored: {err:#}", name(saved)));
        dropped.insert(task_id, name(saved));
        lost.extend(saved.origin.clone());
      }
    }
  }
  // The definitions need no such edit: the kernel keeps them without
  // deps, and gives one the deps its task's edges lead to.
  for registration in &mut registrations {
    registration.def.deps.retain(|dep| {
      let TaskSelector::Id(id) = dep else {
        return true;
      };
      let Some(dep) = dropped.get(id) else {
        return true;
      };
      let task = registration.def.path.as_ref().map_or("", |p| p.as_str());
      warnings.push(format!(
        "task {task}: dropped its dependency on {dep}, which was left out"
      ));
      false
    });
  }
  let mut base = saved_base(snapshot, &registrations);
  let sources: Vec<Option<Source>> =
    registrations.iter().map(|r| r.def.origin.clone()).collect();
  let Ok(results) = pc.register_tasks(registrations).await else {
    bail!("the kernel has stopped");
  };
  for ((saved, registered), source) in
    listed.into_iter().zip(results).zip(sources)
  {
    if let Err(err) = registered {
      warnings.push(format!("saved task {} not restored: {err}", name(saved)));
      lost.extend(source);
    }
  }
  for source in &lost {
    base.remove(source);
  }
  Ok((warnings, base))
}

/// The base the saved tasks had. An older runner saved none: the entries
/// of its tasks that the config has stand in, each as the task is built
/// from it, so the config's others come in as new, as that runner would
/// have added them.
pub fn saved_base<'a>(
  snapshot: &snap::Snapshot,
  restored: impl IntoIterator<Item = &'a TaskRegistration>,
) -> Base {
  if let Some(base) = &snapshot.config_base {
    return base.clone();
  }
  restored
    .into_iter()
    .filter_map(|registration| {
      let def = &registration.def;
      Some((def.origin.clone()?, def.spec.clone()?))
    })
    .collect()
}

/// A saved process task with its saved definition, around its screen
/// and, when it is still this runner's, its child. An older runner saved
/// no definition: a task of its that the config has is built from the
/// config, as that runner restored it, and any other from what it ran.
pub fn saved_task_registration(
  config: &Config,
  task_id: TaskId,
  saved: &snap::Task,
  process: &snap::ProcessTask,
  deps: Vec<TaskSelector>,
  instance: Option<snap::Instance>,
) -> anyhow::Result<TaskRegistration> {
  let key = saved.key()?;
  let in_config = || {
    let key = key.as_ref().filter(|key| key.space.is_default())?;
    config
      .tasks
      .iter()
      .find(|cfg| cfg.path == key.path.as_str())
  };
  let (spec, origin) = match (&saved.spec, in_config()) {
    (Some(spec), _) => (spec.clone(), saved.origin.clone()),
    (None, Some(cfg)) => (config_spec(config, cfg), cfg.source.clone()),
    (None, None) => (legacy_spec(saved, process)?, None),
  };
  let mut registration = process_task_resumed(
    &RunCx::of(config),
    task_id,
    key,
    spec,
    saved.run_spec.clone(),
    &process.screen,
    instance,
    process.sd_notify.clone(),
  )?;
  registration.def.deps = deps;
  registration.def.pinned = saved.pinned;
  registration.def.origin = origin;
  Ok(registration)
}

/// The definition an older runner's task ran, from what it saved beside
/// its child: it saved none. It says `autorestart` outright, as
/// `added_task` does.
fn legacy_spec(
  saved: &snap::Task,
  process: &snap::ProcessTask,
) -> anyhow::Result<TaskSpec> {
  let signal = match &process.stop {
    snap::StopSignal::Shutdown {} => StopSignal::default(),
    snap::StopSignal::Kill {} => StopSignal::Signal {
      sig: Sig::Kill,
      group: true,
    },
    snap::StopSignal::Signal { sig, group } => StopSignal::Signal {
      sig: Sig::from_name(sig)
        .ok_or_else(|| anyhow::anyhow!("unknown stop signal {sig}"))?,
      group: *group,
    },
    snap::StopSignal::SendKeys { keys } => StopSignal::Keys(keys.clone()),
    // An older binary's line, meant for the system shell.
    snap::StopSignal::Cmd { cmd } => {
      StopSignal::Cmd(crate::parse_shell::system_argv(cmd))
    }
    snap::StopSignal::Program { argv } => StopSignal::Cmd(argv.clone()),
  };
  let check = match (&process.ready_log, &process.ready_probe) {
    (None, None) => None,
    (Some(text), None) => Some(ReadyCheck::Log(text.clone())),
    (None, Some(probe)) => Some(ReadyCheck::Probe {
      probe: match &probe.check {
        snap::ReadyCheck::Tcp { host, port } => Probe::Tcp {
          host: host.clone(),
          port: *port,
        },
        snap::ReadyCheck::Http { url } => {
          Probe::Http(parse_http_url(url).map_err(anyhow::Error::msg)?)
        }
        snap::ReadyCheck::Cmd { argv } => Probe::Cmd { argv: argv.clone() },
        snap::ReadyCheck::File { path } => Probe::File {
          path: std::path::PathBuf::from(path),
        },
      },
      interval: Duration::from_millis(probe.interval_ms),
    }),
    (Some(_), Some(_)) => bail!("a task has two ready checks"),
  };
  let stop_timeout = saved.stop_timeout_ms.map(Duration::from_millis);
  let stop = StopConfig {
    signal: Some(signal).filter(|s| *s != StopSignal::default()),
    timeout: stop_timeout.filter(|t| *t != STOP_TIMEOUT),
  };
  let mut argv = vec![process.spec.prog.clone()];
  argv.extend(process.spec.args.iter().cloned());
  let defaults = TaskConfig::default();
  let cfg = TaskConfig {
    label: saved.label.clone(),
    cmd: Some(CmdConfig::Cmd { cmd: argv }),
    kind: if saved.job {
      TaskKind::Job
    } else {
      TaskKind::Service
    },
    ready: check.map(|check| ReadyConfig {
      check,
      timeout: saved.ready_timeout_ms.map(Duration::from_millis),
    }),
    cwd: process.spec.cwd.clone().map(Into::into),
    env: (!process.spec.env.is_empty())
      .then(|| process.spec.env.iter().cloned().collect()),
    autostart: saved
      .tags
      .iter()
      .any(|t| t == AUTOSTART_TAG)
      .then_some(true),
    autorestart: Some(saved.restart.into()),
    stop: (stop.signal.is_some() || stop.timeout.is_some()).then_some(stop),
    log: process.log.as_ref().map(|log| TaskLogConfig {
      enabled: log.enabled,
      dir: log.dir.as_ref().map(std::path::PathBuf::from),
      file: log.file.as_ref().map(std::path::PathBuf::from),
      mode: Some(if log.truncate {
        LogMode::Truncate
      } else {
        LogMode::Append
      }),
    }),
    scrollback_len: Some(process.scrollback_len)
      .filter(|n| *n != defaults.scrollback_len()),
    mouse_scroll_speed: Some(process.mouse_scroll_speed)
      .filter(|n| *n != defaults.mouse_scroll_speed()),
    tags: saved
      .tags
      .iter()
      .filter(|tag| *tag != AUTOSTART_TAG && *tag != DYNAMIC_TAG)
      .cloned()
      .collect(),
    ..TaskConfig::default()
  };
  Ok(TaskSpec::from_config(&cfg))
}

/// A task restored at a runner start: unpinned, and with a saved pin if it
/// was started (or still had a saved pin) when it was saved.
fn with_saved_pin(
  mut registration: TaskRegistration,
  saved: &snap::Task,
) -> TaskRegistration {
  registration.def.pinned = false;
  registration.def.saved_pin = saved.pinned || saved.saved_pin;
  registration
}

/// Registers the config tasks, listed as the config has them. They are
/// pinned if they autostart and `pin_autostart`.
async fn register_config(
  config: &Config,
  pc: &TaskContext,
  pin_autostart: bool,
) -> anyhow::Result<()> {
  let registrations = config_registrations(config, pc, pin_autostart)?;
  let Ok(results) = pc.register_tasks(registrations).await else {
    bail!("the kernel has stopped");
  };
  for (cfg, registered) in config.tasks.iter().zip(results) {
    if let Err(err) = registered {
      bail!("Failed to register task '{}': {}", cfg.path, err);
    }
  }
  Ok(())
}

/// A registration for each config task, in the config's order and with a
/// fresh id, pinned if it autostarts and `pin_autostart`.
pub fn config_registrations(
  config: &Config,
  pc: &TaskContext,
  pin_autostart: bool,
) -> anyhow::Result<Vec<TaskRegistration>> {
  let task_ids: Vec<TaskId> =
    config.tasks.iter().map(|_| pc.alloc_id()).collect();
  let deps_by_task = resolve_task_deps(config, &task_ids)?;
  config
    .tasks
    .iter()
    .enumerate()
    .map(|(i, cfg)| {
      let deps = deps_by_task[i]
        .iter()
        .copied()
        .map(TaskSelector::Id)
        .collect();
      let pinned = pin_autostart && cfg.autostart();
      config_task_registration(config, cfg, task_ids[i], deps, pinned)
    })
    .collect()
}

fn config_task_registration(
  config: &Config,
  cfg: &TaskConfig,
  task_id: TaskId,
  deps: Vec<TaskSelector>,
  pinned: bool,
) -> anyhow::Result<TaskRegistration> {
  let key = TaskPath::new(&cfg.path).ok().map(TaskKey::default_space);
  let spec = config_spec(config, cfg);
  let mut registration =
    process_task(&RunCx::of(config), task_id, key, spec)
      .map_err(|err| anyhow::anyhow!("task '{}': {err:#}", cfg.path))?;
  registration.def.deps = deps;
  registration.def.pinned = pinned;
  registration.def.origin = cfg.source.clone();
  Ok(registration)
}

/// A config task's spec, `defaults` applied: what the base records for
/// it, and what a reload and a write-back compare with.
fn config_spec(config: &Config, cfg: &TaskConfig) -> TaskSpec {
  TaskSpec::from_config(&config.defaults.clone().overlay(cfg.clone()))
}

/// A config's tasks, each with its spec.
pub fn config_specs(config: &Config) -> Vec<(&TaskConfig, TaskSpec)> {
  config
    .tasks
    .iter()
    .map(|cfg| (cfg, config_spec(config, cfg)))
    .collect()
}

/// The base of a config just taken in: each entry as it says.
pub fn config_base(config: &Config) -> Base {
  config_specs(config)
    .into_iter()
    .filter_map(|(cfg, spec)| Some((cfg.source.clone()?, spec)))
    .collect()
}

/// A task `add` registers: `spec` as given, with `defaults` under it but
/// for `autorestart` (a command run once would be rerun for ever), which
/// it then says outright, so it reads the same written into the config.
/// Relative paths in it are from the project root.
pub fn added_task(
  config: &Config,
  path: &TaskPath,
  spec: &TaskSpec,
) -> anyhow::Result<TaskConfig> {
  let mut cfg = spec.parse(path.as_str(), &project_dir(config))?;
  if config.defaults.autorestart.is_some() {
    cfg.autorestart.get_or_insert(RestartMode::Never);
  }
  let mut merged = add_defaults(config).overlay(cfg);
  merged.path = path.to_string();
  Ok(merged)
}

/// What `added_task` lays under a task.
pub fn add_defaults(config: &Config) -> TaskConfig {
  TaskConfig {
    autorestart: None,
    source: None,
    ..config.defaults.clone()
  }
}

/// Where relative paths typed at runtime are from.
pub fn project_dir(config: &Config) -> std::path::PathBuf {
  match &config.runner {
    Some(runner) => runner.root.clone(),
    None => std::env::current_dir().unwrap_or_else(|_| Path::new("/").into()),
  }
}

pub fn resolve_task_deps(
  config: &Config,
  task_ids: &[TaskId],
) -> anyhow::Result<Vec<Vec<TaskId>>> {
  if config.tasks.len() != task_ids.len() {
    bail!("Internal error: task and task id counts differ.");
  }
  let deps = dep_indexes(config)?;
  Ok(
    deps
      .into_iter()
      .map(|deps| deps.into_iter().map(|i| task_ids[i]).collect())
      .collect(),
  )
}

/// Each config task's deps, by index, as a fresh start takes them: each
/// one a task of the config, and none needing itself.
pub fn dep_indexes(config: &Config) -> anyhow::Result<Vec<Vec<usize>>> {
  let mut name_to_index = HashMap::new();
  for (index, task_config) in config.tasks.iter().enumerate() {
    let name = task_config.path.as_str();
    if name_to_index.insert(name, index).is_some() {
      bail!("Duplicate task name '{}'.", name);
    }
  }
  let mut dep_indexes_by_task = Vec::with_capacity(config.tasks.len());
  for task_config in &config.tasks {
    let mut dep_indexes = Vec::with_capacity(task_config.deps.len());
    for dep_name in &task_config.deps {
      let Some(dep_index) = name_to_index.get(dep_name.as_str()) else {
        bail!(
          "Process '{}' depends on unknown process '{}'.",
          task_config.path,
          dep_name
        );
      };
      dep_indexes.push(*dep_index);
    }
    dep_indexes_by_task.push(dep_indexes);
  }
  validate_task_dep_cycles(config, &dep_indexes_by_task)?;
  Ok(dep_indexes_by_task)
}

#[derive(Clone, Copy, PartialEq)]
enum VisitState {
  Unvisited,
  Visiting,
  Visited,
}

fn validate_task_dep_cycles(
  config: &Config,
  deps_by_task: &[Vec<usize>],
) -> anyhow::Result<()> {
  let mut states = vec![VisitState::Unvisited; config.tasks.len()];
  let mut stack = Vec::new();

  for index in 0..config.tasks.len() {
    visit_task_deps(index, config, deps_by_task, &mut states, &mut stack)?;
  }
  Ok(())
}

fn visit_task_deps(
  index: usize,
  config: &Config,
  deps_by_task: &[Vec<usize>],
  states: &mut [VisitState],
  stack: &mut Vec<usize>,
) -> anyhow::Result<()> {
  match states[index] {
    VisitState::Visited => return Ok(()),
    VisitState::Visiting => {
      let cycle_start = stack.iter().position(|&i| i == index).unwrap_or(0);
      let mut cycle = stack[cycle_start..]
        .iter()
        .map(|&i| config.tasks[i].path.as_str())
        .collect::<Vec<_>>();
      cycle.push(config.tasks[index].path.as_str());
      bail!("Process dependency cycle detected: {}.", cycle.join(" -> "));
    }
    VisitState::Unvisited => {}
  }

  states[index] = VisitState::Visiting;
  stack.push(index);
  for dep_index in &deps_by_task[index] {
    visit_task_deps(*dep_index, config, deps_by_task, states, stack)?;
  }
  stack.pop();
  states[index] = VisitState::Visited;
  Ok(())
}

#[cfg(test)]
mod tests {
  use crate::config::task::CmdConfig;
  use crate::kernel::{
    kernel::Kernel,
    kernel_message::{KernelCommand, SpaceSelector, TaskSelector},
  };

  use super::*;

  fn task_config(name: &str, deps: &[&str]) -> TaskConfig {
    TaskConfig {
      path: name.to_string(),
      cmd: Some(CmdConfig::Cmd {
        cmd: vec!["true".to_string()],
      }),
      deps: deps.iter().map(|dep| dep.to_string()).collect(),
      ..TaskConfig::default()
    }
  }

  fn config(tasks: Vec<TaskConfig>) -> Config {
    Config {
      tasks,
      ..Config::make_default()
    }
  }

  /// The task list shows a task's label, or else its path's name; a
  /// label that was the path would stay behind when the task moves.
  #[test]
  fn a_config_task_has_only_the_label_it_is_given() {
    let config = config(vec![task_config("tui/forge", &[])]);
    let label = |cfg: &TaskConfig| {
      config_task_registration(&config, cfg, TaskId(1), Vec::new(), false)
        .unwrap()
        .def
        .label
    };
    assert_eq!(label(&config.tasks[0]), None);
    let labeled = TaskConfig {
      label: Some("Forge".into()),
      ..task_config("tui/forge", &[])
    };
    assert_eq!(label(&labeled).as_deref(), Some("Forge"));
  }

  #[test]
  fn resolve_deps() {
    let config = config(vec![
      task_config("db", &[]),
      task_config("api", &["db"]),
      task_config("web", &["api", "db"]),
    ]);
    let ids = vec![TaskId(1), TaskId(2), TaskId(3)];
    assert_eq!(
      resolve_task_deps(&config, &ids).unwrap(),
      vec![vec![], vec![TaskId(1)], vec![TaskId(2), TaskId(1)]]
    );
  }

  #[test]
  fn reject_unknown_dep() {
    let config = config(vec![task_config("api", &["db"])]);
    let err = resolve_task_deps(&config, &[TaskId(1)]).unwrap_err();
    assert_eq!(
      err.to_string(),
      "Process 'api' depends on unknown process 'db'."
    );
  }

  #[test]
  fn reject_dep_cycle() {
    let config = config(vec![
      task_config("api", &["worker"]),
      task_config("worker", &["db"]),
      task_config("db", &["api"]),
    ]);
    let err = resolve_task_deps(&config, &[TaskId(1), TaskId(2), TaskId(3)])
      .unwrap_err();
    assert_eq!(
      err.to_string(),
      "Process dependency cycle detected: api -> worker -> db -> api."
    );
  }

  /// A task that depends on a later one is registered after it, and still
  /// listed where the config has it.
  #[tokio::test]
  async fn lists_tasks_as_the_config_has_them() {
    let mut config = Config::make_default();
    config.tasks = vec![
      task_config("web", &["db"]),
      task_config("api/a", &["db"]),
      task_config("db", &[]),
      task_config("api/b", &[]),
    ];
    let kernel = Kernel::new();
    let pc = kernel.context();
    let handle = tokio::spawn(kernel.run());

    register_config_tasks(&config, &pc).await.unwrap();

    let tasks = pc.list_tasks(TaskSelector::all()).await;
    let names: Vec<String> = tasks.iter().map(|task| task.name()).collect();
    assert_eq!(names, ["web", "api/a", "api/b", "db"]);

    pc.send(KernelCommand::Quit);
    handle.await.unwrap();
  }

  /// A saved task that cannot be restored is dropped from the
  /// dependencies of the others and their definitions, which are still
  /// restored, and its entry from the base, so a reload offers it again.
  #[tokio::test]
  async fn restores_what_depends_on_a_task_that_cannot_be() {
    let mut snapshot =
      snap::decode(include_bytes!("../upgrade/fixtures/v1-orchestration.json"))
        .unwrap();
    let source = |key: &str| Source {
      file: "dekit.yaml".into(),
      key: key.into(),
      mount: String::new(),
    };
    let spec = |yaml: &str| Some(serde_yaml::from_str(yaml).unwrap());
    // In a space of its own, with a definition that does not parse.
    snapshot.tasks[0].space = "tools".to_string();
    snapshot.tasks[0].spec = spec("{cmd: [db], autorestart: sometimes}");
    snapshot.tasks[0].origin = Some(source("db"));
    let saved =
      [source("db"), source("web")].map(|source| (source, TaskSpec::default()));
    snapshot.config_base = Some(saved.into_iter().collect());
    snapshot.tasks[2].spec = spec("{cmd: [api], deps: ['@tools/db', migrate]}");
    let kernel = Kernel::new();
    let pc = kernel.context();
    let handle = tokio::spawn(kernel.run());
    let (warnings, base) =
      register_saved_tasks(&Config::make_default(), &pc, &snapshot)
        .await
        .unwrap();
    assert_eq!(base.sources().collect::<Vec<_>>(), [&source("web")]);
    assert_eq!(warnings.len(), 3, "{warnings:?}");
    assert!(
      warnings[0].starts_with("saved task @tools/db not restored"),
      "{warnings:?}"
    );
    assert_eq!(
      warnings[1..],
      [
        "task migrate: dropped its dependency on @tools/db, which was left out",
        "task api: dropped its dependency on @tools/db, which was left out",
      ]
    );
    let names: Vec<String> = explain_all(&pc)
      .await
      .into_iter()
      .map(|(name, ..)| name)
      .collect();
    assert_eq!(names, ["api", "migrate", "web", "worker"]);
    let tasks = pc.describe(TaskSelector::all()).await;
    let api = tasks.iter().find(|t| t.info.name() == "api").unwrap();
    assert_eq!(api.spec.as_ref().unwrap().deps(), ["migrate"]);
    pc.send(KernelCommand::Quit);
    handle.await.unwrap();
  }

  #[tokio::test]
  async fn registers_before_returning() {
    let mut config = Config::make_default();
    config.tasks = vec![task_config("db", &[]), task_config("api", &["db"])];
    let kernel = Kernel::new();
    let pc = kernel.context();
    let handle = tokio::spawn(kernel.run());

    register_config_tasks(&config, &pc).await.unwrap();

    assert_eq!(pc.list_tasks(TaskSelector::all()).await.len(), 2);
    let explains = pc
      .explain(TaskSelector::Glob(
        SpaceSelector::default_space(),
        "api".to_string(),
      ))
      .await;
    assert_eq!(explains.len(), 1);
    assert_eq!(explains[0].deps[0].name, "db");

    pc.send(KernelCommand::Quit);
    handle.await.unwrap();
  }

  /// An older runner saved no base: the entries of its tasks that the
  /// config has stand in, so a config task it did not have is new.
  #[tokio::test]
  async fn an_older_runners_base_is_its_tasks_entries() {
    let snapshot =
      snap::decode(include_bytes!("../upgrade/fixtures/v1-orchestration.json"))
        .unwrap();
    assert!(snapshot.config_base.is_none());
    let entry = |name: &str| TaskConfig {
      source: Some(Source {
        file: "dekit.yaml".into(),
        key: name.into(),
        mount: String::new(),
      }),
      ..task_config(name, &[])
    };
    let config = config(vec![entry("db"), entry("new")]);
    let kernel = Kernel::new();
    let pc = kernel.context();
    let handle = tokio::spawn(kernel.run());
    let (_, base) =
      register_saved_tasks(&config, &pc, &snapshot).await.unwrap();
    let db = config.tasks[0].source.as_ref().unwrap();
    assert_eq!(base.sources().collect::<Vec<_>>(), [db]);
    // As the task is built from it.
    assert!(base.get(db).is_some_and(|spec| spec.argv().is_some()));
    pc.send(KernelCommand::Quit);
    handle.await.unwrap();
  }

  async fn explain_all(pc: &TaskContext) -> Vec<(String, bool, bool)> {
    let explains = pc.explain(TaskSelector::all()).await;
    let mut pins: Vec<(String, bool, bool)> = explains
      .into_iter()
      .map(|e| (e.name, e.pinned, e.saved_pin))
      .collect();
    pins.sort();
    pins
  }

  /// A dekit runner starts nothing until asked; `dekit mprocs` still
  /// starts its autostart tasks at once.
  #[tokio::test]
  async fn only_mprocs_pins_autostart_tasks_at_registration() {
    for mprocs in [false, true] {
      let mut config = Config::make_default();
      config.tasks = vec![TaskConfig {
        autostart: Some(true),
        cmd: Some(CmdConfig::Cmd {
          cmd: vec!["sleep".to_string(), "60".to_string()],
        }),
        ..task_config("web", &[])
      }];
      let kernel = Kernel::new();
      let pc = kernel.context();
      let handle = tokio::spawn(kernel.run());
      if mprocs {
        register_config_tasks(&config, &pc).await.unwrap();
      } else {
        register_idle_config_tasks(&config, &pc).await.unwrap();
      }
      assert_eq!(explain_all(&pc).await, [("web".to_string(), mprocs, false)]);
      // No SIGCHLD waiter in unit tests: remove the task so quit can end.
      pc.send(KernelCommand::Remove(TaskSelector::all(), None));
      pc.send(KernelCommand::Quit);
      handle.await.unwrap();
    }
  }

  /// At a runner start the saved tasks come back unpinned; the ones that
  /// were started, or still had a saved pin, get a saved pin for `up`.
  #[tokio::test]
  async fn saved_tasks_come_back_unpinned_with_saved_pins() {
    let snapshot =
      snap::decode(include_bytes!("../upgrade/fixtures/v1-orchestration.json"))
        .unwrap();
    let kernel = Kernel::new();
    let pc = kernel.context();
    let handle = tokio::spawn(kernel.run());
    let (warnings, _) =
      register_saved_tasks(&Config::make_default(), &pc, &snapshot)
        .await
        .unwrap();
    assert!(warnings.is_empty(), "{warnings:?}");
    let pins: Vec<(String, bool, bool)> = [
      ("api", true),
      ("db", true),
      ("migrate", false),
      ("web", true),
      ("worker", true),
    ]
    .into_iter()
    .map(|(name, saved_pin)| (name.to_string(), false, saved_pin))
    .collect();
    assert_eq!(explain_all(&pc).await, pins);
    pc.send(KernelCommand::Quit);
    handle.await.unwrap();
  }
}
