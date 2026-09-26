use std::{collections::BTreeMap, fmt};

use serde::{Deserialize, Serialize};
use tokio::sync::oneshot::Receiver;

use crate::{
  config::{
    config::Config,
    task::{CmdConfig, DYNAMIC_TAG, TaskConfig},
  },
  kernel::kernel_message::{
    Ack, KernelCommand, RegisterError, TaskContext, TaskSelector,
  },
  target::Target,
  task::config_tasks::spawn_config_task,
};

/// The serde-stable verbs shared by the CLI, RPC, config hooks, and JS.
/// Task-directed variants carry a `Target`; `quit` addresses the runner.
#[derive(Clone, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(tag = "command", rename_all = "kebab-case")]
pub enum Command {
  Batch {
    commands: Vec<Command>,
  },
  /// Stop the runner. It saves its tasks for its next start unless
  /// `save` is false.
  Quit {
    #[serde(default = "yes", skip_serializing_if = "is_yes")]
    save: bool,
  },
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
  /// Register a process task at an exact path and start it.
  Add {
    target: Target,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    label: Option<String>,
    #[serde(flatten)]
    cmd: CmdConfig,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    cwd: Option<String>,
    /// `null` unsets a variable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    env: Option<BTreeMap<String, Option<String>>>,
    /// Each dep target must match at least one existing task.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    deps: Vec<Target>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    tags: Vec<String>,
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
  Duplicate {
    target: Target,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    name: Option<String>,
  },
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
}

#[derive(Debug)]
pub enum CommandError {
  InvalidTarget(String),
  InvalidCommand(String),
  Register(RegisterError),
  KernelClosed,
}

impl fmt::Display for CommandError {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self {
      CommandError::InvalidTarget(message)
      | CommandError::InvalidCommand(message) => f.write_str(message),
      CommandError::Register(err) => err.fmt(f),
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
    Command::Quit { .. }
    | Command::Start { .. }
    | Command::Stop { .. }
    | Command::Kill { .. }
    | Command::Veto { .. }
    | Command::Restart { .. }
    | Command::ForceRestart { .. }
    | Command::Add { .. }
    | Command::Remove { .. }
    | Command::Rename { .. }
    | Command::Duplicate { .. } => Ok(result),
  }
}

/// Sends the command's kernel messages now, in order, and logs failures
/// as they come back.
pub fn issue(pc: &TaskContext, config: &Config, command: Command) {
  let mut pending = Vec::new();
  if let Err(err) = dispatch(pc, config, &command, &mut pending, false) {
    log::error!("Command failed: {err}");
    return;
  }
  if pending.is_empty() {
    return;
  }
  tokio::spawn(async move {
    for outcome in pending {
      if let Err(err) = outcome.wait().await {
        log::error!("Command failed: {err}");
      }
    }
  });
}

enum Pending {
  Matched(Receiver<usize>),
  Registered(Receiver<Result<(), RegisterError>>),
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
    }
  }
}

/// Every kernel message a command sends leaves here synchronously, so two
/// commands issued in a row reach the kernel in that order. With `acks`
/// off, only outcomes that can fail (registrations) are collected.
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
    Command::Quit { save: true } => pc.send(KernelCommand::Quit),
    Command::Quit { save: false } => pc.send(KernelCommand::QuitWithoutSave),
    Command::Start { target } => {
      act(pc, target, KernelCommand::Start, pending, acks)?
    }
    Command::Stop { target } => {
      act(pc, target, KernelCommand::Stop, pending, acks)?
    }
    Command::Kill { target } => {
      act(pc, target, KernelCommand::Kill, pending, acks)?
    }
    Command::Veto { target } => {
      act(pc, target, KernelCommand::Veto, pending, acks)?
    }
    Command::Restart { target } => {
      act(pc, target, KernelCommand::Restart, pending, acks)?
    }
    Command::ForceRestart { target } => {
      act(pc, target, KernelCommand::ForceRestart, pending, acks)?
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
    Command::Duplicate { target, name } => {
      let name = name.clone();
      act(
        pc,
        target,
        |selector, ack| KernelCommand::Duplicate(selector, name, ack),
        pending,
        acks,
      )?
    }
    Command::Add {
      target,
      label,
      cmd,
      cwd,
      env,
      deps,
      tags,
    } => {
      let key = target.key().map_err(invalid_target)?;
      match cmd {
        CmdConfig::Cmd { cmd } if cmd.is_empty() => {
          return Err(CommandError::InvalidCommand(
            "cmd must not be empty".to_string(),
          ));
        }
        CmdConfig::Script { .. } if config.runner.is_none() => {
          return Err(CommandError::InvalidCommand(
            "script command has no runner identity".to_string(),
          ));
        }
        CmdConfig::Cmd { .. }
        | CmdConfig::Shell { .. }
        | CmdConfig::Script { .. } => {}
      }
      let deps = deps
        .iter()
        .map(|dep| dep.selector().map_err(invalid_target))
        .collect::<Result<Vec<TaskSelector>, _>>()?;
      let task = TaskConfig {
        path: key.path.to_string(),
        label: label.clone(),
        cmd: Some(cmd.clone()),
        cwd: cwd.clone().map(Into::into),
        env: env.as_ref().map(|env| env.clone().into_iter().collect()),
        tags: std::iter::once(DYNAMIC_TAG.to_string())
          .chain(tags.iter().cloned())
          .collect(),
        ..TaskConfig::default()
      };
      let (_, ack) = spawn_config_task(config, pc, key.space, task, deps, true);
      pending.push(Pending::Registered(ack));
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
    task::{TargetTask, TaskDef, TaskId},
    task_path::TaskPath,
  };

  use super::*;

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
          label: Some("web server".to_string()),
          cmd: CmdConfig::Shell {
            shell: "npm start".to_string(),
          },
          cwd: None,
          env: None,
          deps: vec![Target::glob("db")],
          tags: vec![],
        },
      ],
    };
    let yaml = serde_yaml::to_string(&command).unwrap();
    assert_eq!(serde_yaml::from_str::<Command>(&yaml).unwrap(), command);
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
      serde_json::to_string(&Command::ForceRestart {
        target: Target::Id(TaskId(7)),
      })
      .unwrap(),
      r#"{"command":"force-restart","target":{"id":7}}"#
    );
    assert_eq!(
      serde_json::to_string(&Command::Add {
        target: Target::glob("web"),
        label: None,
        cmd: CmdConfig::Cmd {
          cmd: vec!["npm".to_string(), "start".to_string()],
        },
        cwd: Some("/repo".to_string()),
        env: None,
        deps: vec![],
        tags: vec![],
      })
      .unwrap(),
      r#"{"command":"add","target":"web","cmd":["npm","start"],"cwd":"/repo"}"#
    );
  }

  #[test]
  fn bad_target_fails_at_parse_time() {
    assert!(
      serde_yaml::from_str::<Command>("command: start\ntarget: '*::x'")
        .is_err()
    );
  }

  #[tokio::test]
  async fn add_reports_registration_errors_by_kind() {
    let kernel = Kernel::new();
    let pc = kernel.context();
    let handle = tokio::spawn(kernel.run());
    let config = Config::make_default();
    let add = |target: &str, cmd: Vec<&str>| Command::Add {
      target: target.parse().unwrap(),
      label: None,
      cmd: CmdConfig::Cmd {
        cmd: cmd.into_iter().map(String::from).collect(),
      },
      cwd: None,
      env: None,
      deps: vec![],
      tags: vec![],
    };

    assert!(matches!(
      execute(&pc, &config, &add("x", vec![])).await,
      Err(CommandError::InvalidCommand(_))
    ));
    let script = Command::Add {
      target: "script".parse().unwrap(),
      label: None,
      cmd: CmdConfig::Script {
        script: "/tmp/script.js".into(),
      },
      cwd: None,
      env: None,
      deps: vec![],
      tags: vec![],
    };
    assert!(matches!(
      execute(&pc, &config, &script).await,
      Err(CommandError::InvalidCommand(_))
    ));
    assert!(matches!(
      execute(&pc, &config, &add("@dekit/x", vec!["true"])).await,
      Err(CommandError::Register(RegisterError::ReservedSpace(_)))
    ));
    assert!(matches!(
      execute(&pc, &config, &add("x", vec!["true"])).await,
      Ok(CommandResult::Matched(1))
    ));
    assert!(matches!(
      execute(&pc, &config, &add("x", vec!["true"])).await,
      Err(CommandError::Register(RegisterError::PathTaken(_)))
    ));

    // No SIGCHLD waiter in unit tests: remove the task so quit can finish.
    pc.send(KernelCommand::Remove(TaskSelector::all(), None));
    pc.send(KernelCommand::Quit);
    handle.await.unwrap();
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
}
