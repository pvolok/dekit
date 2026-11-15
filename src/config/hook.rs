use anyhow::Result;

use crate::cfg::CfgObj;
use crate::command::{Command, issue};
use crate::config::config::Config;
use crate::console::action::Action;
use crate::kernel::kernel_message::{TaskContext, TaskSelector};

/// Where a legacy hook's action goes: the console mprocs runs.
pub type Actions = tokio::sync::mpsc::UnboundedSender<Action>;

#[derive(Clone)]
pub enum Hook {
  Command(Command),
  LegacyAction(Action),
}

impl Hook {
  /// A legacy action goes to `console`; with none, it is dropped.
  pub fn run(
    &self,
    pc: &TaskContext,
    config: &Config,
    console: Option<&Actions>,
  ) {
    match self {
      Hook::Command(command) => issue(pc, config, command.clone()),
      Hook::LegacyAction(action) => match console {
        Some(console) => {
          let _ = console.send(action.clone());
        }
        None => log::warn!("no console for the hook's action {action:?}"),
      },
    }
  }
}

/// Runs `hook` each time the selected set goes from active to idle.
pub fn watch_idle(
  pc: &TaskContext,
  config: &Config,
  selector: TaskSelector,
  hook: Hook,
  console: Option<Actions>,
) {
  let mut watch = pc.watch_set(vec![selector]);
  let pc = pc.clone();
  let config = config.clone();
  tokio::spawn(async move {
    let mut active = false;
    while let Some(set) = watch.recv().await {
      let was = std::mem::replace(&mut active, set.iter().any(|t| t.active));
      if was && !active {
        hook.run(&pc, &config, console.as_ref());
      }
    }
  });
}

pub(crate) fn hook_from_cfg(
  obj: &CfgObj<'_>,
  key: &str,
) -> Result<Option<Hook>> {
  match obj.get(key) {
    Some(node) => {
      let command: Command = serde_yaml::from_value(node.raw().clone())?;
      Ok(Some(Hook::Command(command)))
    }
    None => Ok(None),
  }
}
