use std::fmt;

use serde::{Deserialize, Serialize};
use tokio::sync::oneshot::Receiver;

use crate::{
  config::{config::Config, task::AUTOSTART_TAG},
  kernel::kernel_message::{
    Ack, DuplicateRequest, KernelCommand, MoveError, MoveTask, Place,
    RegisterError, TaskContext, TaskSelector, Verb,
  },
  spec::{Source, TaskSpec},
  target::Target,
  task::{
    config_tasks::{added_task, project_dir},
    process_task::{RunCx, process_task, process_task_update},
  },
};

/// The serde-stable verbs shared by the CLI, RPC, config hooks, and JS.
/// Task-directed variants carry a `Target`; `up`, `down`, and `quit`
/// address the runner.
#[derive(Clone, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(tag = "command", rename_all = "kebab-case")]
pub enum Command {
  Batch {
    commands: Vec<Command>,
  },
  /// Save the tasks for the next `up`, then stop the runner.
  Down,
  /// Stop the runner without saving.
  Quit,
  /// Start what the last `down` saved as started, and the autostart
  /// tasks; running tasks and done jobs are left alone.
  Up,
  Start {
    target: Target,
  },
  Stop {
    target: Target,
  },
  Kill {
    target: Target,
  },
  Veto {
    target: Target,
  },
  Restart {
    target: Target,
  },
  ForceRestart {
    target: Target,
  },
  /// Register a process task at an exact path and, unless `start` is
  /// false, start it. `spec` is a task entry as dekit.yaml writes one,
  /// with `defaults` under it but for `autorestart`, unless `defaults`
  /// is false: then it is the whole entry, as the console's dialog and a
  /// reload have it. The entry is its own object: a key it gets later
  /// can never collide with a field of the command.
  Add {
    target: Target,
    spec: TaskSpec,
    /// Where it is listed beside the tasks at its path's parent, as for
    /// `move`; last unless it says.
    #[serde(default, skip_serializing_if = "Place::is_keep")]
    place: Place,
    #[serde(default = "yes", skip_serializing_if = "is_yes")]
    start: bool,
    #[serde(default = "yes", skip_serializing_if = "is_yes")]
    defaults: bool,
    /// The config entry it is written in; without one it is the
    /// runner's own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    origin: Option<Source>,
  },
  /// Redefine the task at an exact path in place: `spec` is its whole
  /// entry. It runs the new definition from its next start, or now with
  /// `restart` if it is running.
  Update {
    target: Target,
    spec: TaskSpec,
    /// The definition the new one was made from: refused when the task's
    /// is another by then, or no task is there, so it never undoes a
    /// change it did not see.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    was: Option<TaskSpec>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    restart: bool,
  },
  /// Move the task or group at an exact path to `to`, or within the list
  /// at its own path.
  Move {
    target: Target,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    to: Option<Target>,
    #[serde(default, skip_serializing_if = "Place::is_keep")]
    place: Place,
  },
  /// Remove matching tasks in any state, killing running ones.
  Remove {
    target: Target,
  },
  /// Set the display label.
  Rename {
    target: Target,
    name: String,
  },
  /// Register a copy of each matching task right after it, next to its
  /// path, and start it unless `start` is false. With `path`, the target
  /// names one task and the copy goes there.
  Duplicate {
    target: Target,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    path: Option<Target>,
    #[serde(default = "yes", skip_serializing_if = "is_yes")]
    start: bool,
  },
  /// Set the config entry the matching tasks are written in; without
  /// one they are the runner's own.
  SetOrigin {
    target: Target,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    origin: Option<Source>,
  },
}

impl Command {
  /// The verb a wait can carry out for it, with its target: for `up`,
  /// the autostart tasks (the kernel adds the saved pins).
  pub fn verb(&self) -> Option<(Verb, Target)> {
    let (verb, target) = match self {
      Command::Up => (Verb::Up, Target::tag(AUTOSTART_TAG)),
      Command::Start { target } => (Verb::Start, target.clone()),
      Command::Restart { target } => (Verb::Restart, target.clone()),
      Command::ForceRestart { target } => (Verb::ForceRestart, target.clone()),
      Command::Stop { target } => (Verb::Stop, target.clone()),
      Command::Kill { target } => (Verb::Kill, target.clone()),
      Command::Veto { target } => (Verb::Veto, target.clone()),
      Command::Batch { .. }
      | Command::Down
      | Command::Quit
      | Command::Add { .. }
      | Command::Update { .. }
      | Command::Move { .. }
      | Command::Remove { .. }
      | Command::Rename { .. }
      | Command::Duplicate { .. }
      | Command::SetOrigin { .. } => return None,
    };
    Some((verb, target))
  }

  /// Whether it starts a runner where none is up: what it does needs one
  /// running. Stopping or removing does not.
  pub fn starts_runner(&self) -> bool {
    match self {
      Command::Up
      | Command::Start { .. }
      | Command::Restart { .. }
      | Command::ForceRestart { .. }
      | Command::Add { .. } => true,
      Command::Batch { .. }
      | Command::Down
      | Command::Quit
      | Command::Stop { .. }
      | Command::Kill { .. }
      | Command::Veto { .. }
      | Command::Update { .. }
      | Command::Move { .. }
      | Command::Remove { .. }
      | Command::Rename { .. }
      | Command::Duplicate { .. }
      | Command::SetOrigin { .. } => false,
    }
  }
}

fn yes() -> bool {
  true
}

fn is_yes(value: &bool) -> bool {
  *value
}

#[derive(Clone, Debug, PartialEq)]
pub enum CommandResult {
  None,
  Matched(usize),
  /// A quit: the runner exits within this.
  Quitting(std::time::Duration),
}

#[derive(Debug)]
pub enum CommandError {
  InvalidTarget(String),
  InvalidCommand(String),
  Register(RegisterError),
  Move(MoveError),
  KernelClosed,
}

impl fmt::Display for CommandError {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self {
      CommandError::InvalidTarget(message)
      | CommandError::InvalidCommand(message) => f.write_str(message),
      CommandError::Register(err) => err.fmt(f),
      CommandError::Move(err) => err.fmt(f),
      CommandError::KernelClosed => {
        f.write_str("kernel closed the reply channel")
      }
    }
  }
}

impl std::error::Error for CommandError {}

/// Runs the command and reports how many tasks it matched.
pub async fn execute(
  pc: &TaskContext,
  config: &Config,
  command: &Command,
) -> Result<CommandResult, CommandError> {
  let mut pending = Vec::new();
  dispatch(pc, config, command, &mut pending, true)?;
  let mut result = CommandResult::None;
  for outcome in pending {
    result = outcome.wait().await?;
  }
  match command {
    Command::Batch { .. } => Ok(CommandResult::None),
    Command::Down
    | Command::Quit
    | Command::Up
    | Command::Start { .. }
    | Command::Stop { .. }
    | Command::Kill { .. }
    | Command::Veto { .. }
    | Command::Restart { .. }
    | Command::ForceRestart { .. }
    | Command::Add { .. }
    | Command::Update { .. }
    | Command::Move { .. }
    | Command::Remove { .. }
    | Command::Rename { .. }
    | Command::Duplicate { .. }
    | Command::SetOrigin { .. } => Ok(result),
  }
}

/// Sends the command's kernel messages now, in order, and logs failures
/// as they come back.
pub fn issue(pc: &TaskContext, config: &Config, command: Command) {
  issue_then(pc, config, command, |result| {
    if let Err(err) = result {
      log::error!("Command failed: {err}");
    }
  });
}

/// `issue`, calling `done` with how it went once every outcome is in:
/// the first failure, any after it logged.
pub fn issue_then(
  pc: &TaskContext,
  config: &Config,
  command: Command,
  done: impl FnOnce(Result<(), CommandError>) + Send + 'static,
) {
  let mut pending = Vec::new();
  if let Err(err) = dispatch(pc, config, &command, &mut pending, false) {
    done(Err(err));
    return;
  }
  if pending.is_empty() {
    done(Ok(()));
    return;
  }
  tokio::spawn(async move {
    let mut first = None;
    for outcome in pending {
      if let Err(err) = outcome.wait().await {
        match first {
          None => first = Some(err),
          Some(_) => log::error!("Command failed: {err}"),
        }
      }
    }
    done(first.map_or(Ok(()), Err));
  });
}

enum Pending {
  Matched(Receiver<usize>),
  Registered(Receiver<Result<(), RegisterError>>),
  /// How many tasks it acted on, or why it was refused.
  Counted(Receiver<Result<usize, RegisterError>>),
  Moved(Receiver<Result<usize, MoveError>>),
  Quitting(Receiver<std::time::Duration>),
}

impl Pending {
  async fn wait(self) -> Result<CommandResult, CommandError> {
    match self {
      Pending::Matched(rx) => rx
        .await
        .map(CommandResult::Matched)
        .map_err(|_| CommandError::KernelClosed),
      Pending::Registered(rx) => match rx.await {
        Ok(Ok(())) => Ok(CommandResult::Matched(1)),
        Ok(Err(err)) => Err(CommandError::Register(err)),
        Err(_) => Err(CommandError::KernelClosed),
      },
      Pending::Counted(rx) => match rx.await {
        Ok(Ok(n)) => Ok(CommandResult::Matched(n)),
        Ok(Err(err)) => Err(CommandError::Register(err)),
        Err(_) => Err(CommandError::KernelClosed),
      },
      Pending::Moved(rx) => match rx.await {
        Ok(Ok(n)) => Ok(CommandResult::Matched(n)),
        Ok(Err(err)) => Err(CommandError::Move(err)),
        Err(_) => Err(CommandError::KernelClosed),
      },
      Pending::Quitting(rx) => rx
        .await
        .map(CommandResult::Quitting)
        .map_err(|_| CommandError::KernelClosed),
    }
  }
}

/// Every kernel message a command sends leaves here synchronously, so two
/// commands issued in a row reach the kernel in that order. With `acks`
/// off, only outcomes that can fail (registrations) are collected.
fn quit(pc: &TaskContext, save: bool, pending: &mut Vec<Pending>, acks: bool) {
  if acks {
    let (reply, rx) = tokio::sync::oneshot::channel();
    pc.send(KernelCommand::QuitWithin { save, reply });
    pending.push(Pending::Quitting(rx));
  } else if save {
    pc.send(KernelCommand::Quit);
  } else {
    pc.send(KernelCommand::QuitWithoutSave);
  }
}

fn invalid_spec(err: anyhow::Error) -> CommandError {
  CommandError::InvalidCommand(format!("{err:#}"))
}

/// The tasks a definition's deps name: each by its exact key.
fn dep_keys(deps: &[String]) -> Result<Vec<TaskSelector>, CommandError> {
  deps
    .iter()
    .map(|dep| {
      let target = dep.parse::<Target>().map_err(invalid_target)?;
      target.key().map_err(|err| {
        CommandError::InvalidTarget(format!("dep '{dep}': {err}"))
      })?;
      target.selector().map_err(invalid_target)
    })
    .collect()
}

fn dispatch(
  pc: &TaskContext,
  config: &Config,
  command: &Command,
  pending: &mut Vec<Pending>,
  acks: bool,
) -> Result<(), CommandError> {
  match command {
    Command::Batch { commands } => {
      for command in commands {
        dispatch(pc, config, command, pending, acks)?;
      }
    }
    Command::Down => quit(pc, true, pending, acks),
    Command::Quit => quit(pc, false, pending, acks),
    Command::Up
    | Command::Start { .. }
    | Command::Stop { .. }
    | Command::Kill { .. }
    | Command::Veto { .. }
    | Command::Restart { .. }
    | Command::ForceRestart { .. } => {
      let (verb, target) = command.verb().expect("a verb");
      act(
        pc,
        &target,
        |selector, ack| KernelCommand::Act(verb, selector, ack),
        pending,
        acks,
      )?
    }
    Command::Remove { target } => {
      act(pc, target, KernelCommand::Remove, pending, acks)?
    }
    Command::Rename { target, name } => {
      let name = Some(name.clone());
      act(
        pc,
        target,
        |selector, ack| KernelCommand::SetLabel(selector, name, ack),
        pending,
        acks,
      )?
    }
    Command::Duplicate {
      target,
      name,
      path,
      start,
    } => {
      let path = path
        .as_ref()
        .map(|path| path.key().map(|key| key.path))
        .transpose()
        .map_err(invalid_target)?;
      // Copies of several tasks can't all go at one path.
      if path.is_some() && !matches!(target, Target::Id(_)) {
        target.key().map_err(invalid_target)?;
      }
      let req = DuplicateRequest {
        label: name.clone(),
        path,
        start: *start,
      };
      let selector = target.selector().map_err(invalid_target)?;
      let (reply, rx) = tokio::sync::oneshot::channel();
      pc.send(KernelCommand::Duplicate(selector, req, reply));
      pending.push(Pending::Counted(rx));
    }
    Command::SetOrigin { target, origin } => {
      let origin = origin.clone();
      act(
        pc,
        target,
        |selector, ack| KernelCommand::SetOrigin(selector, origin, ack),
        pending,
        acks,
      )?
    }
    Command::Add {
      target,
      spec,
      place,
      start,
      defaults,
      origin,
    } => {
      let key = target.key().map_err(invalid_target)?;
      let cfg = match defaults {
        true => added_task(config, &key.path, spec),
        false => spec.parse(key.path.as_str(), &project_dir(config)),
      }
      .map_err(invalid_spec)?;
      let deps = dep_keys(&cfg.deps)?;
      let spec = TaskSpec::from_config(&cfg);
      let mut registration =
        process_task(&RunCx::of(config), pc.alloc_id(), Some(key), spec)
          .map_err(invalid_spec)?;
      registration.def.deps = deps;
      registration.def.place = place.clone();
      registration.def.pinned = *start;
      registration.def.origin = origin.clone();
      pending.push(Pending::Registered(pc.register_task(registration)));
    }
    Command::Update {
      target,
      spec,
      was,
      restart,
    } => {
      // Its whole entry, `defaults` already in it.
      let key = target.key().map_err(invalid_target)?;
      let deps = dep_keys(&spec.deps())?;
      let update = process_task_update(
        &RunCx::of(config),
        &key.path,
        spec.clone(),
        deps,
        was.clone(),
        *restart,
      )
      .map_err(invalid_spec)?;
      let selector = target.selector().map_err(invalid_target)?;
      let (reply, rx) = tokio::sync::oneshot::channel();
      pc.send(KernelCommand::Update(selector, Box::new(update), reply));
      pending.push(Pending::Counted(rx));
    }
    Command::Move { target, to, place } => {
      let from = target.key().map_err(invalid_target)?;
      let to = match to {
        Some(to) => to.key().map_err(invalid_target)?,
        None => from.clone(),
      };
      if to.space != from.space {
        return Err(CommandError::InvalidTarget(
          "a task can't move to another space".to_string(),
        ));
      }
      let (reply, rx) = tokio::sync::oneshot::channel();
      pc.send(KernelCommand::Move(
        MoveTask {
          space: from.space,
          from: from.path,
          to: to.path,
          place: place.clone(),
        },
        reply,
      ));
      pending.push(Pending::Moved(rx));
    }
  }
  Ok(())
}

fn invalid_target(err: impl ToString) -> CommandError {
  CommandError::InvalidTarget(err.to_string())
}

fn act(
  pc: &TaskContext,
  target: &Target,
  make: impl FnOnce(TaskSelector, Ack) -> KernelCommand,
  pending: &mut Vec<Pending>,
  acks: bool,
) -> Result<(), CommandError> {
  let selector = target.selector().map_err(invalid_target)?;
  let ack = if acks {
    let (tx, rx) = tokio::sync::oneshot::channel();
    pending.push(Pending::Matched(rx));
    Some(tx)
  } else {
    None
  };
  pc.send(make(selector, ack));
  Ok(())
}

#[cfg(test)]
mod tests {
  use crate::kernel::{
    kernel::Kernel,
    task::{RestartMode, TargetTask, TaskDef, TaskId},
    task_path::TaskPath,
  };

  use super::*;
  use crate::test_util::spec;

  fn add(target: &str, yaml: &str) -> Command {
    Command::Add {
      target: target.parse().unwrap(),
      spec: spec(yaml),
      place: Default::default(),
      start: true,
      defaults: true,
      origin: None,
    }
  }

  #[test]
  fn serde_round_trip() {
    let command = Command::Batch {
      commands: vec![
        Command::Start {
          target: Target::tag("dev"),
        },
        Command::Stop {
          target: Target::Id(TaskId(7)),
        },
        Command::Add {
          target: Target::glob("web"),
          spec: spec("{label: web server, cmd: [npm, start], deps: [db]}"),
          place: Place::After(TaskPath::new("db").unwrap()),
          start: false,
          defaults: true,
          origin: None,
        },
        Command::Move {
          target: Target::glob("web"),
          to: Some(Target::glob("apps/web")),
          place: Place::After(TaskPath::new("apps/api").unwrap()),
        },
        Command::Update {
          target: Target::glob("web"),
          spec: spec("{cmd: [npm, dev]}"),
          was: Some(spec("{cmd: [npm, start]}")),
          restart: true,
        },
      ],
    };
    // As the RPC sends it; dekit.yaml's hooks only read commands.
    let json = serde_json::to_string(&command).unwrap();
    assert_eq!(serde_json::from_str::<Command>(&json).unwrap(), command);
  }

  #[test]
  fn stable_json_shape() {
    assert_eq!(
      serde_json::to_string(&Command::Start {
        target: Target::tag("dev"),
      })
      .unwrap(),
      r#"{"command":"start","target":"+dev"}"#
    );
    assert_eq!(
      serde_json::to_string(&Command::Up).unwrap(),
      r#"{"command":"up"}"#
    );
    // A field a later version might add is ignored, as for other verbs.
    assert_eq!(
      serde_yaml::from_str::<Command>("{command: up, later: 1}").unwrap(),
      Command::Up
    );
    assert_eq!(
      serde_json::to_string(&Command::ForceRestart {
        target: Target::Id(TaskId(7)),
      })
      .unwrap(),
      r#"{"command":"force-restart","target":{"id":7}}"#
    );
    assert_eq!(
      serde_json::to_string(&add("web", "{cmd: [npm, start], cwd: /repo}"))
        .unwrap(),
      r#"{"command":"add","target":"web","spec":{"cmd":["npm","start"],"cwd":"/repo"}}"#
    );
    assert_eq!(
      serde_json::to_string(&Command::Move {
        target: Target::glob("web"),
        to: None,
        place: Place::First,
      })
      .unwrap(),
      r#"{"command":"move","target":"web","place":"first"}"#
    );
    // The entry is read as dekit.yaml reads one: a string is split into
    // the argv.
    let Ok(Command::Add { spec, .. }) = serde_yaml::from_str::<Command>(
      "{command: add, target: web, spec: {cmd: \"npm run 'my script'\"}}",
    ) else {
      unreachable!()
    };
    let cfg = added_task(
      &Config::make_default(),
      &TaskPath::new("web").unwrap(),
      &spec,
    )
    .unwrap();
    let spec = TaskSpec::from_config(&cfg);
    assert_eq!(spec.argv().unwrap(), ["npm", "run", "my script"]);
  }

  #[test]
  fn bad_target_fails_at_parse_time() {
    assert!(
      serde_yaml::from_str::<Command>("command: start\ntarget: '*::x'")
        .is_err()
    );
  }

  /// What dekit.yaml refuses, `add` refuses, with the same message.
  #[test]
  fn add_reads_the_entry_as_the_config_does() {
    let config = Config::make_default();
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    let pc = TaskContext::new(
      std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(1)),
      crate::kernel::task::INIT_TASK_ID,
      tx,
    );
    for (yaml, expected) in [
      ("{cmd: 'a && b'}", "`&` needs quotes"),
      ("{}", "task must define 'cmd' or 'script'"),
      ("{cmd: []}", "cmd is empty"),
      ("{cmd: x, script: x.js}", "only one of 'cmd' or 'script'"),
      ("{cmd: x, shell: x}", "'shell' is not supported"),
      ("{cmd: x, later: 1}", "unknown field 'later'"),
      (
        "{cmd: x, type: job, ready: {log: a}}",
        "a job takes no 'ready'",
      ),
    ] {
      let err =
        dispatch(&pc, &config, &add("web", yaml), &mut Vec::new(), true)
          .unwrap_err()
          .to_string();
      assert!(err.contains(expected), "{yaml}: {err}");
    }
  }

  /// A copy's path is for one task: copies of several would collide.
  #[test]
  fn a_copy_at_a_path_is_of_one_task() {
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    let pc = TaskContext::new(
      std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(1)),
      crate::kernel::task::INIT_TASK_ID,
      tx,
    );
    let duplicate = |target: Target| Command::Duplicate {
      target,
      name: None,
      path: Some(Target::glob("copy")),
      start: false,
    };
    let config = Config::make_default();
    let mut pending = Vec::new();
    for target in [Target::glob("web/*"), Target::tag("t")] {
      let command = duplicate(target);
      let result = dispatch(&pc, &config, &command, &mut pending, false);
      assert!(matches!(result, Err(CommandError::InvalidTarget(_))));
    }
    for target in [Target::glob("web/api"), Target::Id(TaskId(3))] {
      let command = duplicate(target);
      assert!(dispatch(&pc, &config, &command, &mut pending, false).is_ok());
    }
  }

  #[tokio::test]
  async fn add_reports_registration_errors_by_kind() {
    let kernel = Kernel::new();
    let pc = kernel.context();
    let handle = tokio::spawn(kernel.run());
    let config = Config::make_default();

    assert!(matches!(
      execute(&pc, &config, &add("x", "{cmd: []}")).await,
      Err(CommandError::InvalidCommand(_))
    ));
    let script = add("script", "{script: /tmp/script.js}");
    assert!(matches!(
      execute(&pc, &config, &script).await,
      Err(CommandError::InvalidCommand(_))
    ));
    let reserved = execute(&pc, &config, &add("@dekit/x", "{cmd: ['true']}"))
      .await
      .unwrap_err();
    assert!(
      matches!(
        reserved,
        CommandError::Register(RegisterError::ReservedSpace(_))
      ),
      "{reserved}"
    );
    assert!(matches!(
      execute(&pc, &config, &add("x", "{cmd: ['true']}")).await,
      Ok(CommandResult::Matched(1))
    ));
    assert!(matches!(
      execute(&pc, &config, &add("x", "{cmd: ['true']}")).await,
      Err(CommandError::Register(RegisterError::PathTaken(_)))
    ));

    // No SIGCHLD waiter in unit tests: remove the task so quit can finish.
    pc.send(KernelCommand::Remove(TaskSelector::all(), None));
    pc.send(KernelCommand::Quit);
    handle.await.unwrap();
  }

  /// `defaults.autorestart` would rerun a command run once for ever.
  #[test]
  fn added_task_never_takes_autorestart_from_defaults() {
    let mut config = Config::make_default();
    config.defaults.autorestart = Some(RestartMode::Always);
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let pc = TaskContext::new(
      std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(1)),
      crate::kernel::task::INIT_TASK_ID,
      tx,
    );
    dispatch(
      &pc,
      &config,
      &add("build", "{cmd: make}"),
      &mut Vec::new(),
      true,
    )
    .unwrap();
    match rx.try_recv().map(|msg| msg.command) {
      Ok(KernelCommand::RegisterTask(registration, _)) => {
        assert_eq!(registration.def.restart, RestartMode::Never);
        assert!(registration.def.tags.is_empty());
        let spec = registration.def.spec.unwrap();
        assert_eq!(spec.argv().unwrap(), ["make"]);
        // Said outright: written into the config, `defaults` would give
        // it theirs.
        assert_eq!(spec.get("autorestart"), Some(&"never".into()));
      }
      _ => panic!("expected a registration"),
    }
  }

  #[tokio::test]
  async fn executes_selector_command() {
    let kernel = Kernel::new();
    let pc = kernel.context();
    pc.register(
      TaskDef {
        path: Some(TaskPath::new("api").unwrap()),
        ..TaskDef::default()
      },
      Box::new(|_| Box::new(TargetTask)),
    );
    let handle = tokio::spawn(kernel.run());
    let config = Config::make_default();

    let result = execute(
      &pc,
      &config,
      &Command::Start {
        target: Target::glob("api"),
      },
    )
    .await
    .unwrap();
    assert_eq!(result, CommandResult::Matched(1));

    pc.send(KernelCommand::Quit);
    handle.await.unwrap();
  }

  /// `defaults` lays the project's under what `add` is given; without,
  /// the entry is whole, and what it leaves out stays out.
  #[tokio::test]
  async fn add_takes_the_defaults_unless_the_entry_is_whole() {
    let kernel = Kernel::new();
    let pc = kernel.context();
    let handle = tokio::spawn(kernel.run());
    let mut config = Config::make_default();
    config.defaults.label = Some("from defaults".into());
    for (path, defaults, label) in [
      ("partial", true, Some("from defaults")),
      ("whole", false, None),
    ] {
      let add = Command::Add {
        target: Target::glob(path),
        spec: spec("{cmd: [x]}"),
        place: Default::default(),
        start: false,
        defaults,
        origin: None,
      };
      execute(&pc, &config, &add).await.unwrap();
      let tasks = crate::reload::describe(&pc).await;
      let task = tasks.iter().find(|t| t.info.name() == path).unwrap();
      assert_eq!(task.info.label.as_deref(), label, "{path}");
    }

    pc.send(KernelCommand::Remove(TaskSelector::all(), None));
    pc.send(KernelCommand::Quit);
    handle.await.unwrap();
  }

  /// A place is a word, or the path to go after; a path is read as a
  /// task's path is checked.
  #[test]
  fn a_place_reads_as_a_word_or_a_path() {
    let after = Place::After(TaskPath::new("web/api").unwrap());
    assert_eq!(
      serde_json::to_string(&after).unwrap(),
      r#"{"after":"web/api"}"#
    );
    for (text, place) in [
      (r#""first""#, Place::First),
      (r#"{"after":"web/api"}"#, after.clone()),
    ] {
      assert_eq!(serde_json::from_str::<Place>(text).unwrap(), place);
    }
    // As a hook in dekit.yaml says it.
    for (yaml, place) in
      [("first", Place::First), ("{after: web/api}", after.clone())]
    {
      let command = format!("{{command: move, target: web/ui, place: {yaml}}}");
      let value: serde_yaml::Value = serde_yaml::from_str(&command).unwrap();
      match serde_yaml::from_value::<Command>(value) {
        Ok(Command::Move { place: read, .. }) => assert_eq!(read, place),
        other => panic!("{yaml}: {other:?}"),
      }
    }
    for bad in [r#""middle""#, r#"{"after":"/web"}"#, r#"{"after":"a//b"}"#] {
      assert!(serde_json::from_str::<Place>(bad).is_err(), "{bad}");
    }
  }

  /// An update is kept in the one form a definition has, and a `was` in
  /// another form is the same definition still.
  #[tokio::test]
  async fn an_update_is_kept_in_one_form() {
    let kernel = Kernel::new();
    let pc = kernel.context();
    let handle = tokio::spawn(kernel.run());
    let config = Config::make_default();
    let add = Command::Add {
      target: Target::glob("api"),
      spec: spec("{cmd: [api]}"),
      place: Default::default(),
      start: false,
      defaults: false,
      origin: None,
    };
    execute(&pc, &config, &add).await.unwrap();
    let update = Command::Update {
      target: Target::glob("api"),
      spec: spec("{cmd: api --v2}"),
      was: Some(spec("{cmd: api}")),
      restart: false,
    };
    execute(&pc, &config, &update).await.unwrap();
    let tasks = crate::reload::describe(&pc).await;
    let kept = tasks[0].spec.as_ref().unwrap();
    assert_eq!(*kept, spec("{cmd: [api, --v2]}"));

    pc.send(KernelCommand::Remove(TaskSelector::all(), None));
    pc.send(KernelCommand::Quit);
    handle.await.unwrap();
  }

  /// A dep names a task exactly: a pattern or a tag is refused.
  #[tokio::test]
  async fn a_dep_names_a_task_exactly() {
    let kernel = Kernel::new();
    let pc = kernel.context();
    let handle = tokio::spawn(kernel.run());
    let config = Config::make_default();
    let add = |path: &str, deps: &str| Command::Add {
      target: Target::glob(path),
      spec: spec(&format!("{{cmd: [x], deps: {deps}}}")),
      place: Default::default(),
      start: false,
      defaults: true,
      origin: None,
    };
    execute(&pc, &config, &add("db", "[]")).await.unwrap();
    execute(&pc, &config, &add("api", "[db]")).await.unwrap();
    for deps in ["['d*']", "['+web']"] {
      let err = execute(&pc, &config, &add("web", deps)).await.unwrap_err();
      assert!(
        matches!(err, CommandError::InvalidTarget(_)),
        "{deps}: {err}"
      );
    }

    pc.send(KernelCommand::Remove(TaskSelector::all(), None));
    pc.send(KernelCommand::Quit);
    handle.await.unwrap();
  }
}
