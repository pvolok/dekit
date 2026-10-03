use std::collections::{HashMap, HashSet};

use crate::config::config::Config;
use crate::config::hook::Hook;
use crate::config::keymap::KeymapConfig;
use crate::config::log::LogConfig;
use crate::config::stop_signal::StopConfig;
use crate::config::task::{CmdConfig, TaskConfig};
use crate::config::tui::{SidebarConfig, TipsConfig, TuiConfig};
use crate::kernel::task::{RestartMode, TaskKind};
use crate::kernel::task_path::{path_name, unique};

impl From<crate::mprocs::config::Config> for Config {
  fn from(legacy: crate::mprocs::config::Config) -> Self {
    let defaults = TaskConfig {
      log: legacy.proc_log,
      scrollback_len: Some(legacy.scrollback_len),
      mouse_scroll_speed: Some(legacy.mouse_scroll_speed),
      ..TaskConfig::default()
    };
    // A proc's name is its label, and its path the name made a path.
    let mut taken = HashSet::new();
    let paths: HashMap<String, String> = legacy
      .procs
      .iter()
      .map(|proc| {
        let path = unique(&path_name(&proc.name), |path| taken.contains(path));
        taken.insert(path.clone());
        (proc.name.clone(), path)
      })
      .collect();
    Config {
      runner: None,
      log: LogConfig::default(),
      tasks: legacy
        .procs
        .into_iter()
        .map(|proc| task_config(&paths, proc))
        .collect(),
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
      on_init: legacy.on_init.map(Hook::LegacyAction),
      on_idle: legacy.on_all_finished.map(Hook::LegacyAction),
      system_shell: true,
      warnings: Vec::new(),
    }
  }
}

fn task_config(
  paths: &HashMap<String, String>,
  legacy: crate::mprocs::config::ProcConfig,
) -> TaskConfig {
  TaskConfig {
    path: paths[&legacy.name].clone(),
    cmd: Some(legacy.cmd.into()),
    deps: legacy
      .deps
      .into_iter()
      .map(|dep| paths.get(&dep).cloned().unwrap_or(dep))
      .collect(),
    label: Some(legacy.name),
    tags: Vec::new(),
    cwd: legacy.cwd,
    env: legacy.env,
    add_path: Some(legacy.add_path).filter(|p| !p.is_empty()),
    kind: TaskKind::Service,
    ready: None,
    autostart: Some(legacy.autostart),
    autorestart: Some(if legacy.autorestart {
      RestartMode::OnFailure
    } else {
      RestartMode::Never
    }),
    stop: Some(StopConfig {
      signal: Some(legacy.stop),
      timeout: None,
    }),
    log: legacy.log,
    scrollback_len: Some(legacy.scrollback_len),
    mouse_scroll_speed: Some(legacy.mouse_scroll_speed),
  }
}

impl From<crate::mprocs::config::CmdConfig> for CmdConfig {
  fn from(legacy: crate::mprocs::config::CmdConfig) -> Self {
    match legacy {
      crate::mprocs::config::CmdConfig::Cmd { cmd } => CmdConfig::Cmd { cmd },
      crate::mprocs::config::CmdConfig::Shell { shell } => CmdConfig::Cmd {
        cmd: crate::parse_shell::system_argv(&shell),
      },
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::config::task_log::default_log_filename;
  use crate::mprocs::{config::ConfigContext, settings::Settings};

  #[test]
  fn names_procs_by_label_at_paths_made_from_their_names() {
    let yaml = "procs:\n  npm run dev: 'true'\n  web: 'true'\n  web/dev: 'true'\n  \
                build:watch: {shell: 'true', deps: [npm run dev, web/dev]}\n  \
                npm_run_dev: 'true'\n";
    let legacy = crate::mprocs::config::Config::from_value(
      &serde_yaml::from_str(yaml).unwrap(),
      &ConfigContext {
        path: "mprocs.yaml".into(),
      },
      &Settings::default(),
    )
    .unwrap();
    let config = Config::from(legacy);
    let tasks: Vec<(&str, &str)> = config
      .tasks
      .iter()
      .map(|task| (task.path.as_str(), task.label.as_deref().unwrap()))
      .collect();
    assert_eq!(
      tasks,
      [
        ("npm_run_dev", "npm run dev"),
        ("web", "web"),
        ("web_dev", "web/dev"),
        ("build_watch", "build:watch"),
        ("npm_run_dev-2", "npm_run_dev"),
      ]
    );
    assert_eq!(config.tasks[3].deps, ["npm_run_dev", "web_dev"]);
    // The log files are named as mprocs named them.
    for task in &config.tasks[..4] {
      assert_eq!(
        default_log_filename(&task.path),
        default_log_filename(task.label.as_deref().unwrap())
      );
    }
  }
}
