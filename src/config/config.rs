use std::path::{Path, PathBuf};

use anyhow::{Result, bail};

use crate::cfg::{CfgCx, CfgDoc, CfgObj};
use crate::config::hook::{Hook, event_from_cfg};
use crate::config::keymap::KeymapConfig;
use crate::config::log::LogConfig;
use crate::config::proc::{ProcConfig, parse_proc_settings, proc_from_cfg};
use crate::config::tui::TuiConfig;

pub struct Config {
  pub log: LogConfig,
  pub procs: Vec<ProcConfig>,
  pub proc_defaults: ProcConfig,
  pub tui: TuiConfig,
  pub keymap: KeymapConfig,
  pub on_init: Option<Hook>,
  pub on_all_finished: Option<Hook>,
}

impl Config {
  pub fn make_default() -> Self {
    Self {
      log: LogConfig::default(),
      procs: Vec::new(),
      proc_defaults: ProcConfig::default(),
      tui: TuiConfig::builtin(),
      keymap: KeymapConfig::default(),
      on_init: None,
      on_all_finished: None,
    }
  }

  pub fn load_dir(working_dir: &Path) -> Result<Config> {
    let mut config = Config::make_default();

    // GLOBAL
    if let Some(global) = global_config_path() {
      if global.exists() {
        let dir = global.parent().unwrap_or(working_dir).to_path_buf();
        let cx = CfgCx::new(dir);
        let doc = CfgDoc::load(&global, &cx)?;
        let obj = doc.root().as_obj()?;
        if obj.get("procs").is_some() {
          bail!(
            "'procs' is not allowed in the global config ({}); \
             define processes in the workspace dekit.yaml",
            global.display()
          );
        }
        config.apply(&obj, &cx)?;
      }
    }

    // LOCAL
    let ws = working_dir.join("dekit.yaml");
    if ws.exists() {
      let cx = CfgCx::new(working_dir.to_path_buf());
      let doc = CfgDoc::load(&ws, &cx)?;
      let obj = doc.root().as_obj()?;
      config.apply(&obj, &cx)?;
      if let Some(node) = obj.get("procs") {
        config.procs = node
          .as_obj()?
          .iter()
          .map(|(path, proc)| proc_from_cfg(path.to_string(), &proc, &cx))
          .collect::<Result<Vec<_>>>()?;
      }
    }

    Ok(config)
  }

  fn apply(&mut self, obj: &CfgObj<'_>, cx: &CfgCx) -> Result<()> {
    self.log.merge(obj, cx)?;
    if let Some(pd) = obj.get("proc_defaults") {
      let over = parse_proc_settings(&pd.as_obj()?, cx)?;
      self.proc_defaults =
        std::mem::take(&mut self.proc_defaults).overlay(over);
    }
    self.tui.merge(obj, cx)?;
    self.keymap.merge(obj)?;
    if let Some(hook) = event_from_cfg(obj, "on_init")? {
      self.on_init = Some(hook);
    }
    if let Some(hook) = event_from_cfg(obj, "on_all_finished")? {
      self.on_all_finished = Some(hook);
    }
    Ok(())
  }
}

fn global_config_path() -> Option<PathBuf> {
  let mut base = match std::env::var_os("XDG_CONFIG_HOME") {
    Some(dir) => PathBuf::from(dir),
    None => default_config_dir()?,
  };
  base.push("dekit");
  base.push("dekit.yaml");
  Some(base)
}

#[cfg(windows)]
fn default_config_dir() -> Option<PathBuf> {
  Some(PathBuf::from(std::env::var_os("APPDATA")?))
}

#[cfg(not(windows))]
fn default_config_dir() -> Option<PathBuf> {
  let mut path = PathBuf::from(std::env::var_os("HOME")?);
  path.push(".config");
  Some(path)
}
