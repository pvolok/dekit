use crate::config::config::Config;
use crate::config::hook::Hook;
use crate::config::keymap::KeymapConfig;
use crate::config::log::LogConfig;
use crate::config::task::{CmdConfig, TaskConfig};
use crate::config::tui::{SidebarConfig, TipsConfig, TuiConfig};

impl From<crate::mprocs::config::Config> for Config {
  fn from(legacy: crate::mprocs::config::Config) -> Self {
    let defaults = TaskConfig {
      log: legacy.proc_log,
      scrollback_len: Some(legacy.scrollback_len),
      mouse_scroll_speed: Some(legacy.mouse_scroll_speed),
      ..TaskConfig::default()
    };
    Config {
      log: LogConfig::default(),
      tasks: legacy.procs.into_iter().map(TaskConfig::from).collect(),
      defaults,
      tui: TuiConfig {
        sidebar: SidebarConfig {
          title: legacy.proc_list_title,
          width: legacy.proc_list_width,
        },
        tips: TipsConfig {
          show: !legacy.hide_keymap_window,
        },
        zoom_tip: true,
      },
      keymap: KeymapConfig::default(),
      on_init: legacy.on_init.map(Hook::Action),
      on_all_finished: legacy.on_all_finished.map(Hook::Action),
    }
  }
}

impl From<crate::mprocs::config::ProcConfig> for TaskConfig {
  fn from(legacy: crate::mprocs::config::ProcConfig) -> Self {
    TaskConfig {
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
