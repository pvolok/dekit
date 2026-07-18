use std::{ffi::OsString, path::PathBuf, str::FromStr};

use anyhow::{Result, bail};
use indexmap::IndexMap;
use serde_yaml::Value;

use crate::console::action::Action;
use crate::console::proc::{Sig, StopSignal};
use crate::mprocs::{
  event::AppEvent,
  proc_log_config::LogConfig,
  settings::Settings,
  yaml_val::{Val, value_to_string},
};
use crate::term::key::KeySpec;

pub struct ConfigContext {
  pub path: PathBuf,
}

fn resolve_config_path(path: &str, ctx: &ConfigContext) -> Result<PathBuf> {
  let mut buf = PathBuf::new();
  if let Some(rest) = path.strip_prefix("<CONFIG_DIR>") {
    if let Some(parent) = dunce::canonicalize(&ctx.path)?.parent() {
      buf.push(parent);
    }
    buf.push(rest.trim_start_matches(['/', '\\']));
  } else {
    buf.push(path);
  }

  Ok(buf)
}

pub struct Config {
  pub procs: Vec<ProcConfig>,
  pub server: Option<ServerConfig>,
  pub hide_keymap_window: bool,
  pub mouse_scroll_speed: usize,
  pub scrollback_len: usize,
  pub proc_list_width: usize,
  pub proc_list_title: String,
  pub on_init: Option<Action>,
  pub on_all_finished: Option<Action>,
  pub proc_log: Option<LogConfig>,
}

impl Config {
  pub fn from_value(
    value: &Value,
    ctx: &ConfigContext,
    settings: &Settings,
  ) -> Result<Config> {
    let config = Val::new(value)?;
    let config = config.as_object()?;

    let procs = if let Some(procs) = config.get(&Value::from("procs")) {
      let procs = procs
        .as_object()?
        .into_iter()
        .map(|(name, proc)| {
          ProcConfig::from_val(
            value_to_string(&name)?,
            settings.mouse_scroll_speed,
            settings.scrollback_len,
            proc,
            ctx,
          )
        })
        .collect::<Result<Vec<_>>>()?
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
      procs
    } else {
      Vec::new()
    };

    let server = if let Some(addr) = config.get(&Value::from("server")) {
      Some(ServerConfig::from_str(addr.as_str()?)?)
    } else {
      None
    };

    let proc_list_title =
      if let Some(title) = config.get(&Value::from("proc_list_title")) {
        title.as_str()?.to_string()
      } else {
        settings.proc_list_title.clone()
      };

    let on_init = if let Some(val) = config.get(&Value::from("on_init")) {
      let event: AppEvent = serde_yaml::from_value(val.raw().clone())?;
      Some(event.to_action())
    } else {
      None
    };

    let on_all_finished =
      if let Some(val) = config.get(&Value::from("on_all_finished")) {
        let event: AppEvent = serde_yaml::from_value(val.raw().clone())?;
        Some(event.to_action())
      } else {
        settings.on_all_finished.clone()
      };

    let proc_log = {
      match config.get(&Value::from("proc_log")) {
        Some(val) => {
          crate::mprocs::proc_log_config::parse_log_config(val, |path| {
            resolve_config_path(path, ctx)
          })?
        }
        None => settings.proc_log.clone(),
      }
    };

    let config = Config {
      procs,
      server,
      hide_keymap_window: settings.hide_keymap_window,
      mouse_scroll_speed: settings.mouse_scroll_speed,
      scrollback_len: settings.scrollback_len,
      proc_list_width: settings.proc_list_width,
      proc_list_title,
      on_init,
      on_all_finished,
      proc_log,
    };

    Ok(config)
  }

  pub fn make_default(settings: &Settings) -> anyhow::Result<Self> {
    Ok(Self {
      procs: Vec::new(),
      server: None,
      hide_keymap_window: settings.hide_keymap_window,
      mouse_scroll_speed: settings.mouse_scroll_speed,
      scrollback_len: settings.scrollback_len,
      proc_list_width: settings.proc_list_width,
      proc_list_title: settings.proc_list_title.clone(),
      on_init: None,
      on_all_finished: settings.on_all_finished.clone(),
      proc_log: settings.proc_log.clone(),
    })
  }
}

pub(crate) fn default_stop() -> StopSignal {
  StopSignal::Signal {
    sig: Sig::Term,
    group: false,
  }
}

fn parse_stop(val: &Val) -> Result<StopSignal> {
  match val.raw() {
    Value::String(name) => match name.as_str() {
      "SIGINT" => Ok(StopSignal::Signal {
        sig: Sig::Int,
        group: false,
      }),
      "SIGTERM" => Ok(StopSignal::Signal {
        sig: Sig::Term,
        group: false,
      }),
      "SIGKILL" | "hard-kill" => Ok(StopSignal::Signal {
        sig: Sig::Kill,
        group: false,
      }),
      _ => Err(val.error_at(format!("Unknown stop signal {name:?}"))),
    },
    Value::Mapping(map) => {
      if let Some(keys) = map.get("send-keys") {
        let keys: Vec<KeySpec> = serde_yaml::from_value(keys.clone())?;
        Ok(StopSignal::SendKeys(
          keys.into_iter().map(KeySpec::key).collect(),
        ))
      } else if let Some(cmd) = map.get("cmd") {
        match cmd {
          Value::String(shell) => Ok(StopSignal::Cmd(shell.clone())),
          _ => Err(val.error_at("Expected `cmd` to be a string")),
        }
      } else {
        Err(val.error_at("Expected `send-keys` or `cmd`"))
      }
    }
    _ => Err(val.error_at("Expected a string or mapping")),
  }
}

#[derive(Clone)]
pub struct ProcConfig {
  pub name: String,
  pub cmd: CmdConfig,
  pub cwd: Option<OsString>,
  pub env: Option<IndexMap<String, Option<String>>>,
  pub add_path: Vec<PathBuf>,
  pub autostart: bool,
  pub autorestart: bool,

  pub stop: StopSignal,

  pub deps: Vec<String>,

  pub mouse_scroll_speed: usize,
  pub scrollback_len: usize,
  pub log: Option<LogConfig>,
}

impl ProcConfig {
  fn from_val(
    name: String,
    mouse_scroll_speed: usize,
    scrollback_len: usize,
    val: Val,
    ctx: &ConfigContext,
  ) -> Result<Option<ProcConfig>> {
    match val.raw() {
      Value::Null => Ok(None),
      Value::Bool(_) | Value::Number(_) => {
        Err(val.error_at(
          "Expected a process definition (string, array, or mapping)",
        ))
      }
      Value::String(shell) => Ok(Some(ProcConfig {
        name,
        cmd: CmdConfig::Shell {
          shell: shell.to_owned(),
        },
        cwd: None,
        env: None,
        add_path: Vec::new(),
        autostart: true,
        autorestart: false,
        stop: default_stop(),
        deps: Vec::new(),

        mouse_scroll_speed,
        scrollback_len,
        log: None,
      })),
      Value::Sequence(_) => {
        let cmd = val.as_array()?;
        let cmd = cmd
          .into_iter()
          .map(|item| item.as_str().map(|s| s.to_owned()))
          .collect::<Result<Vec<_>>>()?;

        Ok(Some(ProcConfig {
          name,
          cmd: CmdConfig::Cmd { cmd },
          cwd: None,
          env: None,
          add_path: Vec::new(),
          autostart: true,
          autorestart: false,
          stop: default_stop(),
          deps: Vec::new(),
          mouse_scroll_speed,
          scrollback_len,
          log: None,
        }))
      }
      Value::Mapping(_) => {
        let map = val.as_object()?;

        let cmd = {
          let shell = map.get(&Value::from("shell"));
          let cmd = map.get(&Value::from("cmd"));

          match (shell, cmd) {
            (None, Some(cmd)) => CmdConfig::Cmd {
              cmd: cmd
                .as_array()?
                .into_iter()
                .map(|v| v.as_str().map(|s| s.to_owned()))
                .collect::<Result<Vec<_>>>()?,
            },
            (Some(shell), None) => CmdConfig::Shell {
              shell: shell.as_str()?.to_owned(),
            },
            (None, None) => {
              return Err(
                val.error_at("Process must define either `cmd` or `shell`"),
              );
            }
            (Some(_), Some(_)) => {
              return Err(
                val.error_at("Process cannot define both `cmd` and `shell`"),
              );
            }
          }
        };

        let cwd = match map.get(&Value::from("cwd")) {
          Some(cwd) => {
            let cwd = cwd.as_str()?;
            let mut buf = OsString::new();
            if let Some(rest) = cwd.strip_prefix("<CONFIG_DIR>") {
              if let Some(parent) = dunce::canonicalize(&ctx.path)?.parent() {
                buf.push(parent);
              }
              buf.push(rest);
            } else {
              buf.push(cwd);
            }
            Some(buf)
          }
          None => None,
        };

        let log = {
          match map.get(&Value::from("log")) {
            Some(val) => {
              crate::mprocs::proc_log_config::parse_log_config(val, |path| {
                resolve_config_path(path, ctx)
              })?
            }
            None => None,
          }
        };

        let env = match map.get(&Value::from("env")) {
          Some(env) => {
            let env = env.as_object()?;
            let env = env
              .into_iter()
              .map(|(k, v)| {
                let v = match v.raw() {
                  Value::Null => Ok(None),
                  Value::String(v) => Ok(Some(v.to_owned())),
                  _ => Err(v.error_at("Expected string or null")),
                };
                Ok((value_to_string(&k)?, v?))
              })
              .collect::<Result<IndexMap<_, _>>>()?;
            Some(env)
          }
          None => None,
        };

        let add_path = match map.get(&Value::from("add_path")) {
          Some(add_path) => {
            let extra_paths = match add_path.raw() {
              Value::String(path) => vec![path.as_str()],
              Value::Sequence(paths) => paths
                .iter()
                .filter_map(|path| path.as_str())
                .collect::<Vec<_>>(),
              _ => {
                bail!(add_path.error_at("Expected string or array"));
              }
            };
            extra_paths
              .into_iter()
              .map(|p| PathBuf::from_str(p).map_err(anyhow::Error::from))
              .collect::<Result<Vec<_>>>()?
          }
          None => Vec::new(),
        };

        let autostart = map
          .get(&Value::from("autostart"))
          .map_or(Ok(true), |v| v.as_bool())?;

        let autorestart = map
          .get(&Value::from("autorestart"))
          .map_or(Ok(false), |v| v.as_bool())?;

        let stop_signal = if let Some(val) = map.get(&Value::from("stop")) {
          parse_stop(val)?
        } else {
          default_stop()
        };

        let deps = if let Some(deps) = map.get(&Value::from("deps")) {
          deps
            .as_array()?
            .iter()
            .map(|d| d.as_str().map(|s| s.to_string()))
            .collect::<Result<Vec<_>>>()?
        } else {
          Vec::new()
        };

        Ok(Some(ProcConfig {
          name,
          cmd,
          cwd,
          env,
          add_path,
          autostart,
          autorestart,
          stop: stop_signal,
          deps,
          mouse_scroll_speed,
          scrollback_len,
          log,
        }))
      }
      Value::Tagged(_) => anyhow::bail!("Yaml tags are not supported"),
    }
  }
}

pub enum ServerConfig {
  Tcp(String),
}

impl ServerConfig {
  pub fn from_str(server_addr: &str) -> Result<Self> {
    Ok(Self::Tcp(server_addr.to_string()))
  }
}

#[derive(Clone)]
pub enum CmdConfig {
  Cmd { cmd: Vec<String> },
  Shell { shell: String },
}
