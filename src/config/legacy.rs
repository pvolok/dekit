use crate::config::config::Config;
use crate::config::hook::Hook;
use crate::config::keymap::KeymapConfig;
use crate::config::log::LogConfig;
use crate::config::proc::{CmdConfig, ProcConfig};
use crate::config::tui::{ProcListConfig, TipsConfig, TuiConfig};

impl From<crate::mprocs::config::Config> for Config {
  fn from(legacy: crate::mprocs::config::Config) -> Self {
    let proc_defaults = ProcConfig {
      log: legacy.proc_log,
      scrollback_len: Some(legacy.scrollback_len),
      mouse_scroll_speed: Some(legacy.mouse_scroll_speed),
      ..ProcConfig::default()
    };
    Config {
      log: LogConfig::default(),
      procs: legacy.procs.into_iter().map(ProcConfig::from).collect(),
      proc_defaults,
      tui: TuiConfig {
        procs: ProcListConfig {
          title: legacy.proc_list_title,
          width: legacy.proc_list_width,
        },
        tips: TipsConfig {
          show: !legacy.hide_keymap_window,
        },
        hide_zen_message: legacy.hide_zen_message,
        hide_proc_frame: legacy.hide_proc_frame,
      },
      keymap: KeymapConfig::default(),
      on_init: legacy.on_init.map(Hook::Action),
      on_all_finished: legacy.on_all_finished.map(Hook::Action),
    }
  }
}

impl From<crate::mprocs::config::ProcConfig> for ProcConfig {
  fn from(legacy: crate::mprocs::config::ProcConfig) -> Self {
    ProcConfig {
      path: legacy.name,
      cmd: Some(legacy.cmd.into()),
      deps: legacy.deps,
      tags: Vec::new(),
      cwd: legacy.cwd,
      env: legacy.env,
      add_path: Some(legacy.add_path).filter(|p| !p.is_empty()),
      autostart: Some(legacy.autostart),
      autorestart: Some(legacy.autorestart),
      ready_log: None,
      stop: Some(legacy.stop),
      log: legacy.log,
      scrollback_len: Some(legacy.scrollback_len),
      mouse_scroll_speed: Some(legacy.mouse_scroll_speed),
    }
  }
}

impl From<crate::mprocs::config::CmdConfig> for CmdConfig {
  fn from(legacy: crate::mprocs::config::CmdConfig) -> Self {
    match legacy {
      crate::mprocs::config::CmdConfig::Cmd { cmd } => CmdConfig::Cmd { cmd },
      crate::mprocs::config::CmdConfig::Shell { shell } => {
        CmdConfig::Shell { shell }
      }
    }
  }
}
