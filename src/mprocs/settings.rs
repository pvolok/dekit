use std::{fs::File, io::BufReader, path::PathBuf};

use anyhow::Result;
use indexmap::IndexMap;
use serde_yaml::Value;

use crate::console::action::{Action, CopyMove, ScrollUnit};
use crate::console::keymap::Keymap;
use crate::mprocs::{
  event::AppEvent,
  proc_log_config::LogConfig,
  yaml_val::{Val, value_to_string},
};
use crate::term::key::{Key, KeyCode, KeyMods};

#[derive(Debug)]
pub struct Settings {
  keymap_procs: IndexMap<Key, Action>,
  keymap_term: IndexMap<Key, Action>,
  keymap_copy: IndexMap<Key, Action>,
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
    let mut settings = Self {
      keymap_procs: Default::default(),
      keymap_term: Default::default(),
      keymap_copy: Default::default(),
      hide_keymap_window: false,
      mouse_scroll_speed: 5,
      scrollback_len: 1000,
      proc_list_width: 30,
      proc_list_title: "Processes".to_string(),
      on_all_finished: None,
      proc_log: None,
    };
    settings.add_defaults();
    settings
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
      into: &mut IndexMap<Key, Action>,
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
            crate::term::key::KeySpec::parse(value_to_string(&key)?.as_str())?
              .key();
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
      &mut self.keymap_procs,
      obj.get(&Value::from("keymap_procs")),
    )?;
    add_keys(&mut self.keymap_term, obj.get(&Value::from("keymap_term")))?;
    add_keys(&mut self.keymap_copy, obj.get(&Value::from("keymap_copy")))?;

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

  pub fn add_defaults(&mut self) {
    let s = self;

    s.keymap_add_p(
      Key::new(KeyCode::Char('a'), KeyMods::CONTROL),
      Action::ToggleFocus,
    );
    s.keymap_add_t(
      Key::new(KeyCode::Char('a'), KeyMods::CONTROL),
      Action::ToggleFocus,
    );
    s.keymap_add_c(
      Key::new(KeyCode::Char('a'), KeyMods::CONTROL),
      Action::ToggleFocus,
    );

    s.keymap_add_p(KeyCode::Char('q').into(), Action::Quit);
    s.keymap_add_p(KeyCode::Char('Q').into(), Action::ForceQuit);
    s.keymap_add_p(KeyCode::Char('p').into(), Action::ShowCommandsMenu);
    s.keymap_add_p(Key::new(KeyCode::Down, KeyMods::NONE), Action::NextTask);
    s.keymap_add_p(
      Key::new(KeyCode::Char('j'), KeyMods::NONE),
      Action::NextTask,
    );
    s.keymap_add_p(Key::new(KeyCode::Up, KeyMods::NONE), Action::PrevTask);
    s.keymap_add_p(
      Key::new(KeyCode::Char('k'), KeyMods::NONE),
      Action::PrevTask,
    );
    s.keymap_add_p(
      Key::new(KeyCode::Char('s'), KeyMods::NONE),
      Action::StartTask,
    );
    s.keymap_add_p(
      Key::new(KeyCode::Char('x'), KeyMods::NONE),
      Action::StopTask,
    );
    s.keymap_add_p(
      Key::new(KeyCode::Char('X'), KeyMods::NONE),
      Action::KillTask,
    );
    s.keymap_add_p(
      Key::new(KeyCode::Char('r'), KeyMods::NONE),
      Action::RestartTask,
    );
    s.keymap_add_p(
      Key::new(KeyCode::Char('R'), KeyMods::NONE),
      Action::ForceRestartTask,
    );
    s.keymap_add_p(
      Key::new(KeyCode::Char('e'), KeyMods::NONE),
      Action::ShowRenameTask,
    );
    let ctrlc = Key::new(KeyCode::Char('c'), KeyMods::CONTROL);
    s.keymap_add_p(ctrlc, Action::SendKey { key: ctrlc });
    s.keymap_add_p(
      Key::new(KeyCode::Char('a'), KeyMods::NONE),
      Action::ShowAddTask,
    );
    s.keymap_add_p(
      Key::new(KeyCode::Char('C'), KeyMods::NONE),
      Action::DuplicateTask,
    );
    s.keymap_add_p(
      Key::new(KeyCode::Char('d'), KeyMods::NONE),
      Action::ShowRemoveTask,
    );

    // Scrolling in TERM and COPY modes
    for map in [&mut s.keymap_procs, &mut s.keymap_copy] {
      map.insert(
        Key::new(KeyCode::Char('y'), KeyMods::CONTROL),
        Action::ScrollUp {
          n: 3,
          unit: ScrollUnit::Line,
        },
      );
      map.insert(
        Key::new(KeyCode::Char('e'), KeyMods::CONTROL),
        Action::ScrollDown {
          n: 3,
          unit: ScrollUnit::Line,
        },
      );
      let half_up = Action::ScrollUp {
        n: 1,
        unit: ScrollUnit::HalfScreen,
      };
      let half_down = Action::ScrollDown {
        n: 1,
        unit: ScrollUnit::HalfScreen,
      };
      map.insert(
        Key::new(KeyCode::Char('u'), KeyMods::CONTROL),
        half_up.clone(),
      );
      map.insert(Key::new(KeyCode::PageUp, KeyMods::NONE), half_up.clone());
      map.insert(
        Key::new(KeyCode::Char('d'), KeyMods::CONTROL),
        half_down.clone(),
      );
      map.insert(
        Key::new(KeyCode::PageDown, KeyMods::NONE),
        half_down.clone(),
      );
    }

    s.keymap_add_p(Key::new(KeyCode::Char('z'), KeyMods::NONE), Action::Zoom);

    s.keymap_add_p(
      Key::new(KeyCode::Char('?'), KeyMods::NONE),
      Action::ToggleKeymapWindow,
    );

    s.keymap_add_p(
      Key::new(KeyCode::Char('v'), KeyMods::NONE),
      Action::CopyModeEnter,
    );

    for i in 0..8 {
      let char = char::from_digit(i + 1, 10).unwrap();
      s.keymap_add_p(
        Key::new(KeyCode::Char(char), KeyMods::ALT),
        Action::SelectTask { index: i as usize },
      );
    }

    s.keymap_add_c(KeyCode::Esc.into(), Action::CopyModeLeave);
    s.keymap_add_c(KeyCode::Char('v').into(), Action::CopyModeEnd);
    s.keymap_add_c(KeyCode::Char('c').into(), Action::CopyModeCopy);
    for code in [KeyCode::Up, KeyCode::Char('k')] {
      s.keymap_add_c(code.into(), Action::CopyModeMove { dir: CopyMove::Up });
    }
    for code in [KeyCode::Right, KeyCode::Char('l')] {
      s.keymap_add_c(
        code.into(),
        Action::CopyModeMove {
          dir: CopyMove::Right,
        },
      );
    }
    for code in [KeyCode::Down, KeyCode::Char('j')] {
      s.keymap_add_c(
        code.into(),
        Action::CopyModeMove {
          dir: CopyMove::Down,
        },
      );
    }
    for code in [KeyCode::Left, KeyCode::Char('h')] {
      s.keymap_add_c(
        code.into(),
        Action::CopyModeMove {
          dir: CopyMove::Left,
        },
      );
    }
  }

  fn keymap_add_p(&mut self, key: Key, event: Action) {
    self.keymap_procs.insert(key, event);
  }

  fn keymap_add_t(&mut self, key: Key, event: Action) {
    self.keymap_term.insert(key, event);
  }

  fn keymap_add_c(&mut self, key: Key, event: Action) {
    self.keymap_copy.insert(key, event);
  }

  pub fn add_to_keymap(&self, keymap: &mut Keymap) -> Result<()> {
    for (key, event) in &self.keymap_procs {
      keymap.bind_p(*key, event.clone());
    }
    for (key, event) in &self.keymap_term {
      keymap.bind_t(*key, event.clone());
    }
    for (key, event) in &self.keymap_copy {
      keymap.bind_c(*key, event.clone());
    }

    Ok(())
  }
}
