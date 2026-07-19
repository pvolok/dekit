use std::{ffi::OsString, path::PathBuf};

use anyhow::{Result, bail};
use indexmap::IndexMap;

use crate::cfg::{CfgCx, CfgNode, CfgObj};
use crate::config::proc_log::ProcLogConfig;
use crate::console::proc::StopSignal;
use crate::parse_shell::split_argv;
use crate::process::process_spec::ProcessSpec;

const DEFAULT_SCROLLBACK_LEN: usize = 1000;
const DEFAULT_MOUSE_SCROLL_SPEED: usize = 5;

/// Tag for procs started on `dekit up` / at launch.
pub const AUTOSTART_TAG: &str = "autostart";
/// Tag for tasks spawned over RPC.
pub const USER_TAG: &str = "user";

#[derive(Clone, Default)]
pub struct ProcConfig {
  pub path: String,
  pub cmd: Option<CmdConfig>,
  pub deps: Vec<String>,
  pub tags: Vec<String>,

  pub cwd: Option<OsString>,
  pub env: Option<IndexMap<String, Option<String>>>,
  pub add_path: Option<Vec<PathBuf>>,
  pub autostart: Option<bool>,
  pub autorestart: Option<bool>,
  /// Readiness probe: the proc is ready once an output line contains this
  /// string; until then dependents wait.
  pub ready_log: Option<String>,
  pub stop: Option<StopSignal>,
  pub log: Option<ProcLogConfig>,
  pub scrollback_len: Option<usize>,
  pub mouse_scroll_speed: Option<usize>,
}

impl ProcConfig {
  pub fn overlay(self, over: ProcConfig) -> ProcConfig {
    ProcConfig {
      path: if over.path.is_empty() {
        self.path
      } else {
        over.path
      },
      cmd: over.cmd.or(self.cmd),
      deps: if over.deps.is_empty() {
        self.deps
      } else {
        over.deps
      },
      tags: if over.tags.is_empty() {
        self.tags
      } else {
        over.tags
      },
      cwd: over.cwd.or(self.cwd),
      env: over.env.or(self.env),
      add_path: over.add_path.or(self.add_path),
      autostart: over.autostart.or(self.autostart),
      autorestart: over.autorestart.or(self.autorestart),
      ready_log: over.ready_log.or(self.ready_log),
      stop: over.stop.or(self.stop),
      log: match (over.log, self.log) {
        (Some(over), Some(base)) => Some(base.merged(&over)),
        (over, base) => over.or(base),
      },
      scrollback_len: over.scrollback_len.or(self.scrollback_len),
      mouse_scroll_speed: over.mouse_scroll_speed.or(self.mouse_scroll_speed),
    }
  }

  pub fn autostart(&self) -> bool {
    self.autostart.unwrap_or(false)
  }
  pub fn autorestart(&self) -> bool {
    self.autorestart.unwrap_or(false)
  }
  pub fn stop(&self) -> StopSignal {
    self.stop.clone().unwrap_or_default()
  }
  pub fn scrollback_len(&self) -> usize {
    self.scrollback_len.unwrap_or(DEFAULT_SCROLLBACK_LEN)
  }
  pub fn mouse_scroll_speed(&self) -> usize {
    self
      .mouse_scroll_speed
      .unwrap_or(DEFAULT_MOUSE_SCROLL_SPEED)
  }
}

pub(crate) fn parse_proc_settings(
  obj: &CfgObj<'_>,
  cx: &CfgCx,
) -> Result<ProcConfig> {
  let mut p = ProcConfig::default();
  if let Some(cwd) = obj.get("cwd") {
    p.cwd = Some(cx.resolve_path(cwd.as_str()?).into_os_string());
  }
  p.env = obj.optional("env", cx)?;
  p.add_path = obj.optional("add_path", cx)?;
  p.autostart = obj.optional("autostart", cx)?;
  p.autorestart = obj.optional("autorestart", cx)?;
  p.ready_log = obj.optional("ready_log", cx)?;
  p.stop = obj.optional("stop", cx)?;
  p.log = obj.optional("log", cx)?;
  p.scrollback_len = obj.optional("scrollback_len", cx)?;
  p.mouse_scroll_speed = obj.optional("mouse_scroll_speed", cx)?;
  Ok(p)
}

pub(crate) fn proc_from_cfg(
  path: String,
  node: &CfgNode<'_>,
  cx: &CfgCx,
) -> Result<ProcConfig> {
  let obj = node.as_obj()?;
  let mut p = parse_proc_settings(&obj, cx)?;
  if let Err(err) = crate::kernel::task_path::TaskPath::new(path.as_str()) {
    bail!("proc '{}': {}", path, err);
  }
  p.path = path;
  p.cmd = Some(cmd_from_cfg(node)?);
  p.deps = obj.default("deps", Vec::new(), cx)?;
  p.tags = obj.default("tags", Vec::new(), cx)?;
  Ok(p)
}

fn cmd_from_cfg(node: &CfgNode<'_>) -> Result<CmdConfig> {
  let obj = node.as_obj()?;
  match (obj.get("shell"), obj.get("cmd")) {
    (Some(shell), None) => Ok(CmdConfig::Shell {
      shell: shell.as_str()?.to_owned(),
    }),
    (None, Some(cmd)) => {
      let argv = if cmd.is_string() {
        split_argv(cmd.as_str()?).map_err(|err| cmd.error(err))?
      } else {
        cmd
          .as_arr()?
          .iter()
          .map(|item| Ok(item.as_str()?.to_owned()))
          .collect::<Result<Vec<_>>>()?
      };
      Ok(CmdConfig::Cmd { cmd: argv })
    }
    (None, None) => bail!(obj.error("process must define 'cmd' or 'shell'")),
    (Some(_), Some(_)) => {
      bail!(obj.error("process must define only one of 'cmd' or 'shell'"))
    }
  }
}

#[derive(Clone)]
pub enum CmdConfig {
  Cmd { cmd: Vec<String> },
  Shell { shell: String },
}

impl From<&ProcConfig> for ProcessSpec {
  fn from(cfg: &ProcConfig) -> Self {
    let mut cmd = match &cfg.cmd {
      Some(CmdConfig::Cmd { cmd }) => ProcessSpec::from_argv(cmd.clone()),
      Some(CmdConfig::Shell { shell }) => cmd_from_shell(shell),
      None => ProcessSpec::from_argv(Vec::new()),
    };

    if let Some(env) = &cfg.env {
      for (k, v) in env {
        if let Some(v) = v {
          cmd.env(k, v);
        } else {
          cmd.env_remove(k);
        }
      }
    }

    if let Some(add_path) = cfg.add_path.as_ref().filter(|p| !p.is_empty()) {
      // Base PATH is the proc's own `env` override if it sets one, otherwise
      // the ambient PATH resolved at spawn time.
      let base = cfg
        .env
        .as_ref()
        .and_then(|env| env.get("PATH").cloned().flatten())
        .or_else(|| std::env::var("PATH").ok());
      let mut paths: Vec<PathBuf> = add_path.clone();
      if let Some(base) = base {
        paths.extend(std::env::split_paths(&base));
      }
      if let Ok(joined) = std::env::join_paths(&paths) {
        cmd.env("PATH", joined.to_string_lossy().into_owned());
      }
    }

    if let Some(cwd) = &cfg.cwd {
      cmd.cwd(cwd.to_string_lossy());
    } else if let Ok(cwd) = std::env::current_dir() {
      cmd.cwd(cwd.to_string_lossy());
    }

    cmd
  }
}

#[cfg(windows)]
pub fn cmd_from_shell(shell: &str) -> ProcessSpec {
  ProcessSpec::from_argv(vec![
    "pwsh.exe".into(),
    "-Command".into(),
    shell.into(),
  ])
}

#[cfg(not(windows))]
pub fn cmd_from_shell(shell: &str) -> ProcessSpec {
  ProcessSpec::from_argv(vec!["/bin/sh".into(), "-c".into(), shell.into()])
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn add_path_takes_priority_over_base_path() {
    let mut env = IndexMap::new();
    env.insert("PATH".to_string(), Some("/base/bin".to_string()));

    let cfg = ProcConfig {
      cmd: Some(CmdConfig::Cmd {
        cmd: vec!["true".to_string()],
      }),
      env: Some(env),
      add_path: Some(vec![PathBuf::from("/custom/bin")]),
      ..Default::default()
    };

    let spec = ProcessSpec::from(&cfg);
    let path = spec.env.get("PATH").cloned().flatten().unwrap();

    let expected = std::env::join_paths([
      PathBuf::from("/custom/bin"),
      PathBuf::from("/base/bin"),
    ])
    .unwrap()
    .to_string_lossy()
    .into_owned();

    assert_eq!(path, expected);
  }
}
