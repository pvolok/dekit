use std::{
  ffi::OsString,
  path::{Path, PathBuf},
};

use anyhow::{Result, bail};
use indexmap::IndexMap;
use serde::{Deserialize, Serialize};

use crate::cfg::{CfgCx, CfgNode, CfgObj};
use crate::config::task_log::TaskLogConfig;
use crate::parse_shell::split_argv;
use crate::process::process_spec::ProcessSpec;
use crate::task::process_task::StopSignal;

const DEFAULT_SCROLLBACK_LEN: usize = 1000;
const DEFAULT_MOUSE_SCROLL_SPEED: usize = 5;

/// Keys allowed under `defaults:` (shared process settings, no cmd/deps).
pub(crate) const TASK_SETTING_KEYS: &[&str] = &[
  "cwd",
  "env",
  "add_path",
  "autostart",
  "autorestart",
  "ready_log",
  "stop",
  "log",
  "scrollback_len",
  "mouse_scroll_speed",
];

/// Keys allowed on a full task entry (settings + cmd form + graph fields).
pub(crate) const TASK_KEYS: &[&str] = &[
  "label",
  "cmd",
  "shell",
  "script",
  "deps",
  "tags",
  "cwd",
  "env",
  "add_path",
  "autostart",
  "autorestart",
  "ready_log",
  "stop",
  "log",
  "scrollback_len",
  "mouse_scroll_speed",
];

/// Tag for tasks started on `dekit up` / at launch.
pub const AUTOSTART_TAG: &str = "autostart";

pub fn is_script(path: &Path) -> bool {
  match path.extension().and_then(|ext| ext.to_str()) {
    Some("js" | "mjs") => true,
    _ => false,
  }
}
/// Tag for tasks added at runtime rather than from config.
pub const DYNAMIC_TAG: &str = "dynamic";

#[derive(Clone, Default)]
pub struct TaskConfig {
  pub path: String,
  /// Display name; defaults to the path.
  pub label: Option<String>,
  pub cmd: Option<CmdConfig>,
  pub deps: Vec<String>,
  pub tags: Vec<String>,

  pub cwd: Option<OsString>,
  pub env: Option<IndexMap<String, Option<String>>>,
  pub add_path: Option<Vec<PathBuf>>,
  pub autostart: Option<bool>,
  pub autorestart: Option<bool>,
  /// Readiness probe: the task is ready once an output line contains this
  /// string; until then dependents wait.
  pub ready_log: Option<String>,
  pub stop: Option<StopSignal>,
  pub log: Option<TaskLogConfig>,
  pub scrollback_len: Option<usize>,
  pub mouse_scroll_speed: Option<usize>,
}

impl TaskConfig {
  pub fn overlay(self, over: TaskConfig) -> TaskConfig {
    TaskConfig {
      path: if over.path.is_empty() {
        self.path
      } else {
        over.path
      },
      label: over.label.or(self.label),
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

pub(crate) fn parse_task_settings(
  obj: &CfgObj<'_>,
  cx: &CfgCx,
) -> Result<TaskConfig> {
  // Callers that allow extra keys (full task entries) must validate first.
  // `defaults:` uses this directly and only permits setting keys.
  obj.known_keys(TASK_SETTING_KEYS)?;
  parse_task_settings_unchecked(obj, cx)
}

fn parse_task_settings_unchecked(
  obj: &CfgObj<'_>,
  cx: &CfgCx,
) -> Result<TaskConfig> {
  let mut p = TaskConfig::default();
  if let Some(cwd) = obj.get("cwd") {
    p.cwd = Some(cx.resolve_path(cwd.as_str()?).into_os_string());
  }
  p.env = obj.optional("env", cx)?;
  if let Some(env) = &mut p.env {
    for value in env.values_mut().flatten() {
      if value.starts_with("<CONFIG_DIR>") {
        *value = cx.resolve_path(value).to_string_lossy().into_owned();
      }
    }
  }
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

pub(crate) fn task_from_cfg(
  path: String,
  node: &CfgNode<'_>,
  cx: &CfgCx,
) -> Result<TaskConfig> {
  let obj = node.as_obj()?;
  obj.known_keys(TASK_KEYS)?;
  let mut p = parse_task_settings_unchecked(&obj, cx)?;
  if let Err(err) = crate::kernel::task_path::TaskPath::new(path.as_str()) {
    bail!("task '{}': {}", path, err);
  }
  p.path = path;
  p.label = obj.optional("label", cx)?;
  p.cmd = Some(cmd_from_cfg(node, cx)?);
  p.deps = obj.default("deps", Vec::new(), cx)?;
  p.tags = obj.default("tags", Vec::new(), cx)?;
  Ok(p)
}

fn cmd_from_cfg(node: &CfgNode<'_>, cx: &CfgCx) -> Result<CmdConfig> {
  let obj = node.as_obj()?;
  match (obj.get("shell"), obj.get("cmd"), obj.get("script")) {
    (Some(shell), None, None) => Ok(CmdConfig::Shell {
      shell: shell.as_str()?.to_owned(),
    }),
    (None, Some(cmd), None) => {
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
    (None, None, Some(script)) => {
      let path = cx.resolve_path(script.as_str()?);
      if !is_script(&path) {
        bail!(script.error("script must be a .js or .mjs file"));
      }
      if !path.is_file() {
        bail!(
          script.error(format!("script does not exist: {}", path.display()))
        );
      }
      Ok(CmdConfig::Script { script: path })
    }
    (None, None, None) => {
      bail!(obj.error("task must define 'cmd', 'shell', or 'script'"))
    }
    _ => {
      bail!(
        obj.error("task must define only one of 'cmd', 'shell', or 'script'")
      )
    }
  }
}

#[derive(Clone, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(untagged)]
pub enum CmdConfig {
  Cmd { cmd: Vec<String> },
  Shell { shell: String },
  Script { script: PathBuf },
}

/// The spec a task runs as. `runner` is the identity script tasks
/// inherit; registration paths validate it exists before a script task
/// can reach here.
pub fn process_spec(
  cfg: &TaskConfig,
  runner: Option<&crate::runner::RunnerSpec>,
) -> ProcessSpec {
  let mut cmd = match &cfg.cmd {
    Some(CmdConfig::Cmd { cmd }) => ProcessSpec::from_argv(cmd.clone()),
    Some(CmdConfig::Shell { shell }) => cmd_from_shell(shell),
    Some(CmdConfig::Script { script }) => {
      let runner = runner.expect("script tasks require a runner identity");
      // Runner identity travels in env, not argv, so the script sees
      // the same `[exe, script]` argv as `dekit script.js` on the CLI.
      let mut spec = ProcessSpec::from_argv(vec![
        std::env::current_exe()
          .expect("current executable is available")
          .to_string_lossy()
          .into_owned(),
        script.to_string_lossy().into_owned(),
      ]);
      spec.env(
        crate::runner::ENV_RUNNER_ROOT,
        runner.root.to_str().expect("validated runner root"),
      );
      spec.env(crate::runner::ENV_RUNNER_KIND, runner.kind.as_str());
      spec
    }
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
    // Base PATH is the task's own `env` override if it sets one, otherwise
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

#[cfg(windows)]
pub fn cmd_from_shell(shell: &str) -> ProcessSpec {
  // Prefer PowerShell 7, but fall back to Windows PowerShell if not installed.
  let shell_exe = if which::which("pwsh.exe").is_ok() {
    "pwsh.exe"
  } else {
    "powershell.exe"
  };
  ProcessSpec::from_argv(vec![
    shell_exe.into(),
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
  use crate::cfg::{CfgCx, CfgDoc};
  use std::path::PathBuf;

  #[test]
  fn task_rejects_unknown_keys_with_suggestion() {
    let yaml = r#"
cmd: ["echo", "hi"]
auto_restart: true
"#;
    let value: serde_yaml::Value = serde_yaml::from_str(yaml).unwrap();
    let cx = CfgCx::new(PathBuf::from("."));
    let doc = CfgDoc::from_value(value, &cx).unwrap();
    let err = match task_from_cfg("web".into(), &doc.root(), &cx) {
      Ok(_) => panic!("expected unknown key error"),
      Err(e) => e.to_string(),
    };
    assert!(err.contains("unknown field 'auto_restart'"), "err={err}");
    assert!(err.contains("did you mean 'autorestart'?"), "err={err}");
  }

  #[test]
  fn defaults_reject_cmd_key() {
    let yaml = r#"
cmd: ["echo", "nope"]
cwd: /tmp
"#;
    let value: serde_yaml::Value = serde_yaml::from_str(yaml).unwrap();
    let cx = CfgCx::new(PathBuf::from("."));
    let doc = CfgDoc::from_value(value, &cx).unwrap();
    let err = match parse_task_settings(&doc.root().as_obj().unwrap(), &cx) {
      Ok(_) => panic!("expected unknown key error"),
      Err(e) => e.to_string(),
    };
    assert!(err.contains("unknown field 'cmd'"), "err={err}");
  }

  #[test]
  fn env_resolves_config_dir_prefix() {
    let yaml = r#"
env:
  FOO_DIR: <CONFIG_DIR>/foo/bar
  ROOT: <CONFIG_DIR>
  PLAIN: ./relative
  NOT_PREFIX: prefix/<CONFIG_DIR>
  EMPTY: ''
  REMOVE: null
"#;
    let value = serde_yaml::from_str(yaml).unwrap();
    let cx = CfgCx::new(PathBuf::from("project"));
    let doc = CfgDoc::from_value(value, &cx).unwrap();
    let config =
      parse_task_settings(&doc.root().as_obj().unwrap(), &cx).unwrap();
    let env = config.env.unwrap();

    assert_eq!(
      env["FOO_DIR"],
      Some(cx.config_dir.join("foo/bar").to_string_lossy().into_owned())
    );
    assert_eq!(PathBuf::from(env["ROOT"].as_ref().unwrap()), cx.config_dir);
    assert_eq!(env["PLAIN"].as_deref(), Some("./relative"));
    assert_eq!(env["NOT_PREFIX"].as_deref(), Some("prefix/<CONFIG_DIR>"));
    assert_eq!(env["EMPTY"].as_deref(), Some(""));
    assert_eq!(env["REMOVE"], None);
  }

  #[test]
  fn add_path_takes_priority_over_base_path() {
    let mut env = IndexMap::new();
    env.insert("PATH".to_string(), Some("/base/bin".to_string()));

    let cfg = TaskConfig {
      cmd: Some(CmdConfig::Cmd {
        cmd: vec!["true".to_string()],
      }),
      env: Some(env),
      add_path: Some(vec![PathBuf::from("/custom/bin")]),
      ..Default::default()
    };

    let spec = process_spec(&cfg, None);
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
