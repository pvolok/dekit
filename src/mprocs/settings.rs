use std::{fs::File, io::BufReader, path::PathBuf};

use anyhow::Result;
use indexmap::IndexMap;
use serde_yaml::Value;

use crate::config::keymap::KeymapConfig;
use crate::console::action::Action;
use crate::console::keymap::KeymapGroup;
use crate::mprocs::{
  event::AppEvent,
  proc_log_config::LogConfig,
  yaml_val::{Val, value_to_string},
};

#[derive(Debug)]
pub struct Settings {
  pub keymap: KeymapConfig,
  pub hide_keymap_window: bool,
  pub mouse_scroll_speed: usize,
  pub scrollback_len: usize,
  pub proc_list_width: usize,
  pub proc_list_title: String,
  pub on_all_finished: Option<Action>,
  pub proc_log: Option<LogConfig>,
}

impl Default for Settings {
  fn default() -> Self {
    Self {
      keymap: KeymapConfig::classic(),
      hide_keymap_window: false,
      mouse_scroll_speed: 5,
      scrollback_len: 1000,
      proc_list_width: 30,
      proc_list_title: "Processes".to_string(),
      on_all_finished: None,
      proc_log: None,
    }
  }
}

impl Settings {
  pub fn merge_from_xdg(&mut self) -> Result<()> {
    if let Some(path) = self.get_xdg_config_path() {
      match File::open(&path) {
        Ok(file) => {
          let reader = BufReader::new(file);
          let settings_value: Value = serde_yaml::from_reader(reader)?;
          let settings_val = Val::new(&settings_value)?;
          self.merge_value(settings_val)?;
        }
        Err(err) => match err.kind() {
          std::io::ErrorKind::NotFound => (),
          _ => return Err(err.into()),
        },
      }
    }

    Ok(())
  }

  fn get_xdg_config_path(&self) -> Option<std::path::PathBuf> {
    let mut buf = if let Ok(path) = std::env::var("XDG_CONFIG_HOME") {
      PathBuf::from(path)
    } else {
      self.get_xdg_config_dir()?
    };
    buf.push("mprocs/mprocs.yaml");

    Some(buf)
  }

  #[cfg(windows)]
  fn get_xdg_config_dir(&self) -> Option<PathBuf> {
    let path = PathBuf::from(std::env::var_os("APPDATA")?);
    Some(path)
  }

  #[cfg(not(windows))]
  fn get_xdg_config_dir(&self) -> Option<PathBuf> {
    use std::ffi::OsString;

    let mut path = PathBuf::from(
      std::env::var_os("HOME").unwrap_or_else(|| OsString::from("/")),
    );
    path.push(".config");
    Some(path)
  }

  pub fn merge_value(&mut self, val: Val) -> Result<()> {
    let obj = val.as_object()?;

    fn add_keys(
      into: &mut IndexMap<crate::config::keymap::KeySeq, Action>,
      val: Option<&Val>,
    ) -> Result<()> {
      if let Some(keymap) = val {
        let mut keymap = keymap.as_object()?;

        if let Some(reset) = keymap.shift_remove(&Value::from("reset")) {
          if reset.as_bool()? {
            into.clear();
          }
        }

        for (key, event) in keymap {
          let key =
            crate::term::key::Key::parse(value_to_string(&key)?.as_str())?
              .into();
          if event.raw().is_null() {
            into.shift_remove(&key);
          } else {
            let event: AppEvent = serde_yaml::from_value(event.raw().clone())?;
            into.insert(key, event.to_action());
          }
        }
      }
      Ok(())
    }
    add_keys(
      self.keymap.group_mut(KeymapGroup::Tasks),
      obj.get(&Value::from("keymap_procs")),
    )?;
    add_keys(
      self.keymap.group_mut(KeymapGroup::Term),
      obj.get(&Value::from("keymap_term")),
    )?;
    add_keys(
      self.keymap.group_mut(KeymapGroup::Copy),
      obj.get(&Value::from("keymap_copy")),
    )?;

    if let Some(hide_keymap_window) =
      obj.get(&Value::from("hide_keymap_window"))
    {
      self.hide_keymap_window = hide_keymap_window.as_bool()?;
    }

    if let Some(mouse_scroll_speed) =
      obj.get(&Value::from("mouse_scroll_speed"))
    {
      self.mouse_scroll_speed = mouse_scroll_speed.as_usize()?;
    }

    if let Some(scrollback) = obj.get(&Value::from("scrollback")) {
      self.scrollback_len = scrollback.as_usize()?;
    }

    if let Some(proc_list_title) = obj.get(&Value::from("proc_list_title")) {
      self.proc_list_title = proc_list_title.as_str()?.to_string();
    }

    if let Some(proc_list_width) = obj.get(&Value::from("proc_list_width")) {
      self.proc_list_width = proc_list_width.as_usize()?;
    }

    if let Some(on_all_finished) = obj.get(&Value::from("on_all_finished")) {
      let event: AppEvent =
        serde_yaml::from_value(on_all_finished.raw().clone())?;
      self.on_all_finished = Some(event.to_action());
    }

    if let Some(proc_log) = obj.get(&Value::from("proc_log")) {
      self.proc_log =
        crate::mprocs::proc_log_config::parse_log_config(proc_log, |path| {
          Ok(PathBuf::from(path))
        })?;
    }

    Ok(())
  }
}
