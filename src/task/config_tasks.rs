use std::collections::{HashMap, HashSet, VecDeque};

use anyhow::bail;
use futures::future::try_join_all;

use crate::{
  config::{
    config::Config,
    task::{AUTOSTART_TAG, TaskConfig},
  },
  kernel::{
    kernel_message::{
      RegisterError, TaskContext, TaskRegistration, TaskSelector,
    },
    task::{RestartMode, TaskId},
    task_key::{TaskKey, TaskSpaceId},
    task_path::TaskPath,
  },
  task::{
    logger::LogSpec,
    process_task::{
      ProcessTaskConfig, process_task_config_from_snapshot,
      process_task_registration, process_task_resumed,
    },
  },
  upgrade::snapshot as snap,
};

pub async fn register_config_tasks(
  config: &Config,
  pc: &TaskContext,
) -> anyhow::Result<()> {
  register_config(config, pc, &HashMap::new()).await?;
  Ok(())
}

/// The tasks a runner saved when it quit, every one idle with a fresh
/// id: the config tasks first, each around its saved screen and with its
/// saved pin when the snapshot has it, then the saved tasks the config
/// lacks. Returns what could not be restored.
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
  let mut new_id = register_config(config, pc, &saved_by_path).await?;

  let mut warnings = Vec::new();
  let mut pending: Vec<&snap::Task> = snapshot
    .tasks
    .iter()
    .filter(|task| !new_id.contains_key(&task.id))
    .filter(|task| match task.kind {
      snap::TaskKind::Process(_) => true,
      snap::TaskKind::Console {} => false,
    })
    .collect();
  // Only these ever get a new id; a dependency on anything else is
  // dropped.
  let restorable: HashSet<usize> = pending
    .iter()
    .map(|task| task.id)
    .chain(new_id.keys().copied())
    .collect();
  // Dependencies first.
  while !pending.is_empty() {
    let (ready, rest): (Vec<_>, Vec<_>) = pending.into_iter().partition(|t| {
      t.deps
        .iter()
        .all(|dep| new_id.contains_key(dep) || !restorable.contains(dep))
    });
    if ready.is_empty() {
      bail!("the saved tasks depend on each other in a cycle");
    }
    pending = rest;
    for saved in ready {
      let snap::TaskKind::Process(process) = &saved.kind else {
        continue;
      };
      let name = saved.path.clone().unwrap_or_else(|| saved.id.to_string());
      let mut deps = Vec::new();
      for dep in &saved.deps {
        match new_id.get(dep) {
          Some(id) => deps.push(TaskSelector::Id(*id)),
          None => warnings.push(format!(
            "saved task {name}: dropped a dependency that was not restored"
          )),
        }
      }
      let id = pc.alloc_id();
      let registration = process_task_config_from_snapshot(
        saved, process, deps,
      )
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
        process_task_resumed(id, key, config, &process.screen, None)
      });
      let registered = match registration {
        Ok(registration) => match pc.register_task(registration).await {
          Ok(registered) => registered.map_err(|err| err.to_string()),
          Err(_) => bail!("the kernel has stopped"),
        },
        Err(err) => Err(format!("{err:#}")),
      };
      match registered {
        Ok(()) => {
          new_id.insert(saved.id, id);
        }
        Err(err) => {
          warnings.push(format!("saved task {name} not restored: {err}"))
        }
      }
    }
  }
  Ok(warnings)
}

/// Registers the config tasks, around their saved screens where
/// `saved_by_path` has them. Returns the new id of every saved task used.
async fn register_config(
  config: &Config,
  pc: &TaskContext,
  saved_by_path: &HashMap<&str, &snap::Task>,
) -> anyhow::Result<HashMap<usize, TaskId>> {
  let task_ids: Vec<TaskId> =
    config.tasks.iter().map(|_| pc.alloc_id()).collect();
  let deps_by_task = resolve_task_deps(&config.tasks, &task_ids)?;
  let order = dep_order(&task_ids, &deps_by_task)?;

  let mut new_id = HashMap::new();
  let mut replies = Vec::with_capacity(order.len());
  for &i in &order {
    let cfg = config.tasks[i].clone();
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
        config_task_resumed(
          config,
          cfg,
          task_ids[i],
          deps,
          saved.pinned,
          &process.screen,
          None,
        )?
      }
      None => {
        let pinned = cfg.autostart();
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
    replies.push(pc.register_task(registration));
  }
  let outcomes = try_join_all(replies).await?;
  for (i, registered) in order.into_iter().zip(outcomes) {
    if let Err(err) = registered {
      bail!(
        "Failed to register task '{}': {}",
        config.tasks[i].path,
        err
      );
    }
  }
  Ok(new_id)
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
  let (key, process) =
    config_task_parts(config, space, cfg, task_id, deps, pinned);
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
  let (key, process) = config_task_parts(
    config,
    TaskSpaceId::default_space(),
    cfg,
    task_id,
    deps,
    pinned,
  );
  process_task_resumed(task_id, key, process, screen, instance)
}

fn config_task_parts(
  config: &Config,
  space: TaskSpaceId,
  cfg: TaskConfig,
  task_id: TaskId,
  deps: Vec<TaskSelector>,
  pinned: bool,
) -> (Option<TaskKey>, ProcessTaskConfig) {
  let merged = config.defaults.clone().overlay(cfg);
  let path = TaskPath::new(&merged.path)
    .or_else(|_| TaskPath::new(task_id.0.to_string()))
    .ok();
  (
    path.map(|path| TaskKey::new(space, path)),
    process_task_config(&merged, config.runner.as_ref(), deps, pinned),
  )
}

fn process_task_config(
  cfg: &TaskConfig,
  runner: Option<&crate::runner::RunnerSpec>,
  deps: Vec<TaskSelector>,
  pinned: bool,
) -> ProcessTaskConfig {
  let log = cfg.log.clone().map(|config| LogSpec {
    config,
    name: cfg.path.clone(),
  });
  ProcessTaskConfig {
    spec: crate::config::task::process_spec(cfg, runner),
    stop: cfg.stop(),
    log,
    restart: if cfg.autorestart() {
      RestartMode::OnFailure
    } else {
      RestartMode::Never
    },
    ready_log: cfg.ready_log.clone(),
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
  task_configs: &[TaskConfig],
  task_ids: &[TaskId],
) -> anyhow::Result<Vec<Vec<TaskId>>> {
  if task_configs.len() != task_ids.len() {
    bail!("Internal error: task and task id counts differ.");
  }

  let mut name_to_id = HashMap::new();
  let mut name_to_index = HashMap::new();
  for (index, (task_config, task_id)) in
    task_configs.iter().zip(task_ids.iter()).enumerate()
  {
    if name_to_id
      .insert(task_config.path.as_str(), *task_id)
      .is_some()
    {
      bail!("Duplicate task name '{}'.", task_config.path);
    }
    name_to_index.insert(task_config.path.as_str(), index);
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

  validate_task_dep_cycles(task_configs, &dep_indexes_by_task)?;
  Ok(deps_by_task)
}

fn dep_order(
  task_ids: &[TaskId],
  deps_by_task: &[Vec<TaskId>],
) -> anyhow::Result<Vec<usize>> {
  let index_of: HashMap<TaskId, usize> = task_ids
    .iter()
    .enumerate()
    .map(|(i, id)| (*id, i))
    .collect();
  let n = task_ids.len();
  let mut missing_deps = vec![0usize; n];
  let mut dependents: Vec<Vec<usize>> = vec![Vec::new(); n];
  for (i, deps) in deps_by_task.iter().enumerate() {
    missing_deps[i] = deps.len();
    for dep in deps {
      dependents[index_of[dep]].push(i);
    }
  }
  let mut queue: VecDeque<usize> =
    (0..n).filter(|i| missing_deps[*i] == 0).collect();
  let mut order = Vec::with_capacity(n);
  while let Some(i) = queue.pop_front() {
    order.push(i);
    for &k in &dependents[i] {
      missing_deps[k] -= 1;
      if missing_deps[k] == 0 {
        queue.push_back(k);
      }
    }
  }
  if order.len() != n {
    bail!("Dependency cycle among config tasks.");
  }
  Ok(order)
}

#[derive(Clone, Copy, PartialEq)]
enum VisitState {
  Unvisited,
  Visiting,
  Visited,
}

fn validate_task_dep_cycles(
  task_configs: &[TaskConfig],
  deps_by_task: &[Vec<usize>],
) -> anyhow::Result<()> {
  let mut states = vec![VisitState::Unvisited; task_configs.len()];
  let mut stack = Vec::new();

  for index in 0..task_configs.len() {
    visit_task_deps(
      index,
      task_configs,
      deps_by_task,
      &mut states,
      &mut stack,
    )?;
  }
  Ok(())
}

fn visit_task_deps(
  index: usize,
  task_configs: &[TaskConfig],
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
        .map(|&i| task_configs[i].path.as_str())
        .collect::<Vec<_>>();
      cycle.push(task_configs[index].path.as_str());
      bail!("Process dependency cycle detected: {}.", cycle.join(" -> "));
    }
    VisitState::Unvisited => {}
  }

  states[index] = VisitState::Visiting;
  stack.push(index);
  for dep_index in &deps_by_task[index] {
    visit_task_deps(*dep_index, task_configs, deps_by_task, states, stack)?;
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
      cmd: Some(CmdConfig::Shell {
        shell: "true".to_string(),
      }),
      deps: deps.iter().map(|dep| dep.to_string()).collect(),
      ..TaskConfig::default()
    }
  }

  #[test]
  fn resolve_deps() {
    let configs = vec![
      task_config("db", &[]),
      task_config("api", &["db"]),
      task_config("web", &["api", "db"]),
    ];
    let ids = vec![TaskId(1), TaskId(2), TaskId(3)];
    assert_eq!(
      resolve_task_deps(&configs, &ids).unwrap(),
      vec![vec![], vec![TaskId(1)], vec![TaskId(2), TaskId(1)]]
    );
  }

  #[test]
  fn reject_unknown_dep() {
    let err = resolve_task_deps(&[task_config("api", &["db"])], &[TaskId(1)])
      .unwrap_err();
    assert_eq!(
      err.to_string(),
      "Process 'api' depends on unknown process 'db'."
    );
  }

  #[test]
  fn reject_dep_cycle() {
    let configs = vec![
      task_config("api", &["worker"]),
      task_config("worker", &["db"]),
      task_config("db", &["api"]),
    ];
    let err = resolve_task_deps(&configs, &[TaskId(1), TaskId(2), TaskId(3)])
      .unwrap_err();
    assert_eq!(
      err.to_string(),
      "Process dependency cycle detected: api -> worker -> db -> api."
    );
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
}
