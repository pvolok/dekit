use std::collections::HashMap;

use anyhow::bail;
use indexmap::IndexMap;

use crate::{
  config::{
    config::Config,
    task::{AUTOSTART_TAG, TaskConfig},
  },
  kernel::{
    kernel_message::{
      RegisterError, TaskContext, TaskRegistration, TaskSelector,
    },
    task::{RestartMode, STOP_TIMEOUT, TaskId},
    task_key::{TaskKey, TaskSpaceId},
    task_path::TaskPath,
  },
  task::{
    logger::LogSpec,
    process_task::{
      ProcessTaskConfig, StopSignal, process_task_config_from_snapshot,
      process_task_registration, process_task_resumed,
    },
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

/// The tasks a runner saved when it quit, every one idle and unpinned with
/// a fresh id: the config tasks, each around its saved screen when the
/// snapshot has it, and the saved tasks the config lacks, listed as
/// `list_order` has them. A task that was started when they were saved
/// gets a saved pin, so `up` starts it again. Returns what could not be
/// restored.
pub async fn register_saved_tasks(
  config: &Config,
  pc: &TaskContext,
  snapshot: &snap::Snapshot,
) -> anyhow::Result<Vec<String>> {
  let saved_by_path: HashMap<&str, &snap::Task> = snapshot
    .tasks
    .iter()
    .filter(|task| task.space.is_empty())
    .filter_map(|task| Some((task.path.as_deref()?, task)))
    .collect();
  let (from_config, mut new_id) =
    config_registrations(config, pc, &saved_by_path, false)?;
  let mut from_config: Vec<Option<TaskRegistration>> =
    from_config.into_iter().map(Some).collect();
  let (order, left_out) = list_order(config, &snapshot.tasks);
  let name =
    |saved: &snap::Task| saved.path.clone().unwrap_or(saved.id.to_string());
  let mut warnings = Vec::new();
  // What is left out, by id, so a dependency on it can be dropped.
  let mut dropped: HashMap<TaskId, String> = HashMap::new();
  for (i, j) in left_out {
    let Some(registration) = from_config[i].take() else {
      continue;
    };
    let path = &config.tasks[i].path;
    warnings.push(format!(
      "config task {path} not added: it would be above or under saved task {}",
      name(&snapshot.tasks[j])
    ));
    dropped.insert(registration.task_id, path.clone());
  }

  let pending: Vec<(usize, &snap::Task, &snap::ProcessTask)> = snapshot
    .tasks
    .iter()
    .enumerate()
    .filter(|(_, task)| !new_id.contains_key(&task.id))
    .filter_map(|(j, task)| match &task.kind {
      snap::TaskKind::Process(process) => Some((j, task, process)),
      snap::TaskKind::Console {} => None,
    })
    .collect();
  // Only these and the config's get a new id; a dependency on anything
  // else is dropped.
  for (_, saved, _) in &pending {
    new_id.insert(saved.id, pc.alloc_id());
  }
  let mut from_snapshot: HashMap<usize, TaskRegistration> = HashMap::new();
  for (j, saved, process) in pending {
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
    let registration = process_task_config_from_snapshot(saved, process, deps)
      .and_then(|config| {
        let space = if saved.space.is_empty() {
          TaskSpaceId::default_space()
        } else {
          TaskSpaceId::new(saved.space.clone())
            .map_err(|err| anyhow::anyhow!("{err}"))?
        };
        let key = saved
          .path
          .as_deref()
          .map(TaskPath::new)
          .transpose()?
          .map(|path| TaskKey::new(space, path));
        process_task_resumed(
          new_id[&saved.id],
          key,
          config,
          &process.screen,
          None,
        )
      });
    match registration {
      Ok(registration) => {
        from_snapshot.insert(j, with_saved_pin(registration, saved));
      }
      Err(err) => {
        warnings
          .push(format!("saved task {} not restored: {err:#}", name(saved)));
        dropped.insert(new_id[&saved.id], name(saved));
      }
    }
  }

  let mut listed = Vec::new();
  let mut registrations = Vec::new();
  for entry in order {
    let registration = match entry {
      Listed::Config(i) => from_config[i].take(),
      Listed::Saved(j) => from_snapshot.remove(&j),
    };
    if let Some(mut registration) = registration {
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
      listed.push(entry);
      registrations.push(registration);
    }
  }
  let Ok(results) = pc.register_tasks(registrations).await else {
    bail!("the kernel has stopped");
  };
  for (entry, registered) in listed.into_iter().zip(results) {
    let Err(err) = registered else {
      continue;
    };
    match entry {
      Listed::Config(i) => bail!(
        "Failed to register task '{}': {}",
        config.tasks[i].path,
        err
      ),
      Listed::Saved(j) => warnings.push(format!(
        "saved task {} not restored: {err}",
        name(&snapshot.tasks[j])
      )),
    }
  }
  Ok(warnings)
}

/// One task of a restored list: a config task by its place in the config,
/// or a saved task the config lacks by its place in the snapshot.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Listed {
  Config(usize),
  Saved(usize),
}

/// The order to list the config's tasks and the saved tasks it lacks, the
/// saved ones in the order the snapshot lists them. Among the paths with
/// one parent, the config's come as the config has them, and each of the
/// others right after the one it was listed after, or first if it was
/// the first.
///
/// A saved task wins over a config task above or under it: the config
/// task is left out, and returned with the saved task in its way.
pub fn list_order(
  config: &Config,
  saved: &[snap::Task],
) -> (Vec<Listed>, Vec<(usize, usize)>) {
  #[derive(Default)]
  struct Dir {
    task: Option<Listed>,
    children: IndexMap<String, Dir>,
    /// The child the saved list passed last.
    passed: Option<String>,
  }

  fn walk(dir: &Dir, left_out: &[(usize, usize)], out: &mut Vec<Listed>) {
    match dir.task {
      Some(Listed::Config(i)) if left_out.iter().any(|(l, _)| *l == i) => (),
      Some(task) => out.push(task),
      None => (),
    }
    for child in dir.children.values() {
      walk(child, left_out, out);
    }
  }

  fn config_tasks(dir: &Dir, out: &mut Vec<usize>) {
    if let Some(Listed::Config(i)) = dir.task {
      out.push(i);
    }
    for child in dir.children.values() {
      config_tasks(child, out);
    }
  }

  // The spaces are the root's children.
  let mut root = Dir::default();
  for (i, task) in config.tasks.iter().enumerate() {
    let mut dir = &mut root;
    for part in std::iter::once("").chain(task.path.as_str().split('/')) {
      dir = dir.children.entry(part.to_string()).or_default();
    }
    dir.task = Some(Listed::Config(i));
  }
  let mut rest = Vec::new();
  let mut left_out = Vec::new();
  for (j, task) in saved.iter().enumerate() {
    let Some(path) = &task.path else {
      rest.push(Listed::Saved(j));
      continue;
    };
    let mut dir = &mut root;
    for part in std::iter::once(task.space.as_str()).chain(path.split('/')) {
      if let Some(Listed::Config(i)) = dir.task {
        left_out.push((i, j));
      }
      if !dir.children.contains_key(part) {
        let last = dir.children.len();
        let place = dir
          .passed
          .as_deref()
          .and_then(|passed| dir.children.get_index_of(passed))
          .map_or(0, |index| index + 1);
        dir.children.insert(part.to_string(), Dir::default());
        dir.children.move_index(last, place);
      }
      dir.passed = Some(part.to_string());
      dir = &mut dir.children[part];
    }
    match dir.task {
      None => {
        dir.task = Some(Listed::Saved(j));
        let mut under = Vec::new();
        config_tasks(dir, &mut under);
        left_out.extend(under.into_iter().map(|i| (i, j)));
      }
      Some(Listed::Config(_)) => (),
      // Two at one path: the kernel refuses the second.
      Some(Listed::Saved(_)) => rest.push(Listed::Saved(j)),
    }
  }
  let mut order = Vec::with_capacity(config.tasks.len() + saved.len());
  walk(&root, &left_out, &mut order);
  order.extend(rest);
  (order, left_out)
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
  let (registrations, _) =
    config_registrations(config, pc, &HashMap::new(), pin_autostart)?;
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
/// fresh id, around its saved screen where `saved_by_path` has it. The
/// others are pinned if they autostart and `pin_autostart`. Also returns
/// the new id of every saved task used.
fn config_registrations(
  config: &Config,
  pc: &TaskContext,
  saved_by_path: &HashMap<&str, &snap::Task>,
  pin_autostart: bool,
) -> anyhow::Result<(Vec<TaskRegistration>, HashMap<usize, TaskId>)> {
  let task_ids: Vec<TaskId> =
    config.tasks.iter().map(|_| pc.alloc_id()).collect();
  let deps_by_task = resolve_task_deps(config, &task_ids)?;

  let mut new_id = HashMap::new();
  let mut registrations = Vec::with_capacity(config.tasks.len());
  for (i, cfg) in config.tasks.iter().enumerate() {
    let cfg = cfg.clone();
    let deps = deps_by_task[i]
      .iter()
      .copied()
      .map(TaskSelector::Id)
      .collect();
    let registration = match saved_by_path.get(cfg.path.as_str()) {
      Some(saved) => {
        let snap::TaskKind::Process(process) = &saved.kind else {
          bail!("saved task {} is not a process task", cfg.path);
        };
        new_id.insert(saved.id, task_ids[i]);
        let registration = config_task_resumed(
          config,
          cfg,
          task_ids[i],
          deps,
          false,
          &process.screen,
          None,
        )?;
        with_saved_pin(registration, saved)
      }
      None => {
        let pinned = pin_autostart && cfg.autostart();
        config_task_registration(
          config,
          TaskSpaceId::default_space(),
          cfg,
          task_ids[i],
          deps,
          pinned,
        )
      }
    };
    registrations.push(registration);
  }
  Ok((registrations, new_id))
}

pub fn spawn_config_task(
  config: &Config,
  pc: &TaskContext,
  space: TaskSpaceId,
  cfg: TaskConfig,
  deps: Vec<TaskSelector>,
  pinned: bool,
) -> (
  TaskId,
  tokio::sync::oneshot::Receiver<Result<(), RegisterError>>,
) {
  let task_id = pc.alloc_id();
  let ack = pc.register_task(config_task_registration(
    config, space, cfg, task_id, deps, pinned,
  ));
  (task_id, ack)
}

pub fn config_task_registration(
  config: &Config,
  space: TaskSpaceId,
  cfg: TaskConfig,
  task_id: TaskId,
  deps: Vec<TaskSelector>,
  pinned: bool,
) -> TaskRegistration {
  let (key, process) = config_task_parts(config, space, cfg, deps, pinned);
  process_task_registration(task_id, key, process)
}

/// A config task continued from a snapshot: the config's spec around the
/// saved screen and child. A changed command applies at the next start.
pub fn config_task_resumed(
  config: &Config,
  cfg: TaskConfig,
  task_id: TaskId,
  deps: Vec<TaskSelector>,
  pinned: bool,
  screen: &snap::Screen,
  instance: Option<snap::Instance>,
) -> anyhow::Result<TaskRegistration> {
  let (key, process) =
    config_task_parts(config, TaskSpaceId::default_space(), cfg, deps, pinned);
  process_task_resumed(task_id, key, process, screen, instance)
}

fn config_task_parts(
  config: &Config,
  space: TaskSpaceId,
  cfg: TaskConfig,
  deps: Vec<TaskSelector>,
  pinned: bool,
) -> (Option<TaskKey>, ProcessTaskConfig) {
  let merged = config.defaults.clone().overlay(cfg);
  let path = TaskPath::new(&merged.path).ok();
  (
    path.map(|path| TaskKey::new(space, path)),
    process_task_config(config, &merged, deps, pinned),
  )
}

fn process_task_config(
  config: &Config,
  cfg: &TaskConfig,
  deps: Vec<TaskSelector>,
  pinned: bool,
) -> ProcessTaskConfig {
  let log = cfg.log.clone().map(|log| LogSpec {
    config: log,
    name: cfg.path.clone(),
  });
  let (stop, stop_timeout) = match &cfg.stop {
    Some(stop) => (
      stop.signal.clone().unwrap_or_default(),
      stop.timeout.unwrap_or(STOP_TIMEOUT),
    ),
    None => (StopSignal::default(), STOP_TIMEOUT),
  };
  ProcessTaskConfig {
    spec: crate::config::task::process_spec(cfg, config.runner.as_ref()),
    kind: cfg.kind,
    stop,
    stop_timeout,
    log,
    restart: cfg.autorestart.unwrap_or(RestartMode::Never),
    ready: cfg.ready.clone(),
    scrollback_len: cfg.scrollback_len(),
    mouse_scroll_speed: cfg.mouse_scroll_speed(),
    deps,
    label: Some(cfg.label.clone().unwrap_or_else(|| cfg.path.clone())),
    tags: {
      let mut tags = cfg.tags.clone();
      if cfg.autostart() {
        tags.push(AUTOSTART_TAG.to_string());
      }
      tags
    },
    pinned,
  }
}

pub fn resolve_task_deps(
  config: &Config,
  task_ids: &[TaskId],
) -> anyhow::Result<Vec<Vec<TaskId>>> {
  let task_configs = &config.tasks;
  if task_configs.len() != task_ids.len() {
    bail!("Internal error: task and task id counts differ.");
  }

  let mut name_to_id = HashMap::new();
  let mut name_to_index = HashMap::new();
  for (index, (task_config, task_id)) in
    task_configs.iter().zip(task_ids.iter()).enumerate()
  {
    let name = task_config.path.as_str();
    if name_to_id.insert(name, *task_id).is_some() {
      bail!("Duplicate task name '{}'.", name);
    }
    name_to_index.insert(name, index);
  }

  let mut deps_by_task = Vec::with_capacity(task_configs.len());
  let mut dep_indexes_by_task = Vec::with_capacity(task_configs.len());
  for task_config in task_configs {
    let mut deps = Vec::with_capacity(task_config.deps.len());
    let mut dep_indexes = Vec::with_capacity(task_config.deps.len());
    for dep_name in &task_config.deps {
      let Some(dep_id) = name_to_id.get(dep_name.as_str()) else {
        bail!(
          "Process '{}' depends on unknown process '{}'.",
          task_config.path,
          dep_name
        );
      };
      let Some(dep_index) = name_to_index.get(dep_name.as_str()) else {
        bail!(
          "Process '{}' depends on unknown process '{}'.",
          task_config.path,
          dep_name
        );
      };
      deps.push(*dep_id);
      dep_indexes.push(*dep_index);
    }
    deps_by_task.push(deps);
    dep_indexes_by_task.push(dep_indexes);
  }

  validate_task_dep_cycles(config, &dep_indexes_by_task)?;
  Ok(deps_by_task)
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
    kernel_message::{
      KernelCommand, KernelQuery, KernelQueryResponse, SpaceSelector,
      TaskSelector,
    },
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

    let response = pc
      .query(KernelQuery::ListTasks(TaskSelector::all()))
      .await
      .unwrap();
    let KernelQueryResponse::TaskList(tasks) = response else {
      panic!("unexpected response");
    };
    let names: Vec<String> = tasks.iter().map(|task| task.name()).collect();
    assert_eq!(names, ["web", "api/a", "api/b", "db"]);

    pc.send(KernelCommand::Quit);
    handle.await.unwrap();
  }

  #[test]
  fn saved_tasks_the_config_lacks_keep_their_place() {
    let saved = |path: Option<&str>| snap::Task {
      id: 0,
      space: String::new(),
      path: path.map(str::to_string),
      label: None,
      tags: Vec::new(),
      pinned: false,
      deps: Vec::new(),
      restart: snap::Restart::Never,
      job: false,
      ready_timeout_ms: None,
      stop_timeout_ms: None,
      state: snap::TaskState::Idle {},
      vetoed: false,
      killed: false,
      start_failed: false,
      saved_pin: false,
      attempts: 0,
      last_start_secs_ago: None,
      timer_ms: None,
      kind: snap::TaskKind::Console {},
    };
    let order = |config_paths: &[&str], saved_paths: &[Option<&str>]| {
      let config =
        config(config_paths.iter().map(|p| task_config(p, &[])).collect());
      let tasks: Vec<snap::Task> =
        saved_paths.iter().map(|path| saved(*path)).collect();
      list_order(&config, &tasks)
        .0
        .into_iter()
        .map(|entry| match entry {
          Listed::Config(i) => config_paths[i].to_string(),
          Listed::Saved(j) => match saved_paths[j] {
            Some(path) => format!("+{path}"),
            None => format!("+{j}"),
          },
        })
        .collect::<Vec<String>>()
    };

    // The config is as it was: the list is as it was saved.
    assert_eq!(
      order(
        &["db", "web/api", "cache", "web/ui"],
        &[
          Some("first"),
          Some("db"),
          Some("db_2"),
          Some("web/api"),
          Some("web/api_2"),
          Some("web/ui"),
          None,
          Some("cache"),
          Some("extra/a"),
        ],
      ),
      [
        "+first",
        "db",
        "+db_2",
        "web/api",
        "+web/api_2",
        "web/ui",
        "cache",
        "+extra/a",
        "+6"
      ]
    );

    // `gone` left the config, `new` joined it, and the others changed
    // places since the save.
    assert_eq!(
      order(
        &["new", "api", "db"],
        &[
          Some("db"),
          Some("db_2"),
          Some("gone"),
          Some("api"),
          Some("api_2"),
        ],
      ),
      ["new", "api", "+api_2", "db", "+db_2", "+gone"]
    );
  }

  /// A saved task wins over a config task above or under it.
  #[test]
  fn leaves_out_config_tasks_in_the_way_of_saved_ones() {
    let config = config(
      ["web/api", "db", "web/ui", "api"]
        .iter()
        .map(|p| task_config(p, &[]))
        .collect(),
    );
    let saved = |path: &str| snap::Task {
      path: Some(path.to_string()),
      ..snap::decode(include_bytes!(
        "../upgrade/fixtures/v1-orchestration.json"
      ))
      .unwrap()
      .tasks
      .remove(0)
    };
    let (order, left_out) =
      list_order(&config, &[saved("web"), saved("db"), saved("api/v1")]);
    assert_eq!(
      order,
      [Listed::Saved(0), Listed::Config(1), Listed::Saved(2)]
    );
    assert_eq!(left_out, [(0, 0), (2, 0), (3, 2)]);
  }

  /// A saved task that cannot be restored is dropped from the
  /// dependencies of the others, which are still restored.
  #[tokio::test]
  async fn restores_what_depends_on_a_task_that_cannot_be() {
    let mut snapshot =
      snap::decode(include_bytes!("../upgrade/fixtures/v1-orchestration.json"))
        .unwrap();
    snapshot.tasks[0].path = Some("bad path".to_string());
    let kernel = Kernel::new();
    let pc = kernel.context();
    let handle = tokio::spawn(kernel.run());
    let warnings =
      register_saved_tasks(&Config::make_default(), &pc, &snapshot)
        .await
        .unwrap();
    assert_eq!(warnings.len(), 3, "{warnings:?}");
    assert!(warnings[0].starts_with("saved task bad path not restored"));
    assert_eq!(
      warnings[1..],
      [
        "task migrate: dropped its dependency on bad path, which was left out",
        "task api: dropped its dependency on bad path, which was left out",
      ]
    );
    let names: Vec<String> = explain_all(&pc)
      .await
      .into_iter()
      .map(|(name, ..)| name)
      .collect();
    assert_eq!(names, ["api", "migrate", "web", "worker"]);
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

    let response = pc
      .query(KernelQuery::ListTasks(TaskSelector::all()))
      .await
      .unwrap();
    let KernelQueryResponse::TaskList(tasks) = response else {
      panic!("unexpected response");
    };
    assert_eq!(tasks.len(), 2);
    let response = pc
      .query(KernelQuery::Explain(TaskSelector::Glob(
        SpaceSelector::default_space(),
        "api".to_string(),
      )))
      .await
      .unwrap();
    let KernelQueryResponse::Explain(explains) = response else {
      panic!("unexpected response");
    };
    assert_eq!(explains.len(), 1);
    assert_eq!(explains[0].deps[0].name, "db");

    pc.send(KernelCommand::Quit);
    handle.await.unwrap();
  }

  async fn explain_all(pc: &TaskContext) -> Vec<(String, bool, bool)> {
    let response = pc
      .query(KernelQuery::Explain(TaskSelector::all()))
      .await
      .unwrap();
    let KernelQueryResponse::Explain(explains) = response else {
      panic!("unexpected response");
    };
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
    let warnings =
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
