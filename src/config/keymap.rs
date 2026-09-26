use anyhow::Result;
use indexmap::IndexMap;

use crate::cfg::{CfgNode, CfgObj};
use crate::console::action::{Action, CopyMove, ScrollUnit};
use crate::console::keymap::{Keymap, KeymapGroup};
use crate::term::key::{Key, KeyCode, KeyMods, KeySpec};

#[derive(Clone, Debug)]
pub struct KeymapConfig {
  keymap_tasks: IndexMap<Key, Action>,
  keymap_term: IndexMap<Key, Action>,
  keymap_copy: IndexMap<Key, Action>,
}

impl Default for KeymapConfig {
  fn default() -> Self {
    let mut settings = Self {
      keymap_tasks: Default::default(),
      keymap_term: Default::default(),
      keymap_copy: Default::default(),
    };
    settings.add_defaults();
    settings
  }
}

impl KeymapConfig {
  pub fn merge(&mut self, obj: &CfgObj<'_>) -> Result<()> {
    let keymap = match obj.get("keymap") {
      Some(node) => node.as_obj()?,
      None => return Ok(()),
    };
    keymap.known_keys(&["tasks", "term", "term_copy"])?;
    if let Some(tasks) = keymap.get("tasks") {
      add_keys(&mut self.keymap_tasks, &tasks)?;
    }
    if let Some(term) = keymap.get("term") {
      add_keys(&mut self.keymap_term, &term)?;
    }
    if let Some(copy) = keymap.get("term_copy") {
      add_keys(&mut self.keymap_copy, &copy)?;
    }
    Ok(())
  }

  pub fn group_mut(
    &mut self,
    group: KeymapGroup,
  ) -> &mut IndexMap<Key, Action> {
    match group {
      KeymapGroup::Tasks => &mut self.keymap_tasks,
      KeymapGroup::Term => &mut self.keymap_term,
      KeymapGroup::Copy => &mut self.keymap_copy,
    }
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

    s.keymap_add_p(KeyCode::Char('q').into(), Action::Detach);
    s.keymap_add_p(KeyCode::Char('Q').into(), Action::Quit);
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
    for map in [&mut s.keymap_tasks, &mut s.keymap_copy] {
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
      map.insert(
        Key::new(KeyCode::Char('u'), KeyMods::CONTROL),
        Action::ScrollUp {
          n: 1,
          unit: ScrollUnit::HalfScreen,
        },
      );
      map.insert(
        Key::new(KeyCode::Char('d'), KeyMods::CONTROL),
        Action::ScrollDown {
          n: 1,
          unit: ScrollUnit::HalfScreen,
        },
      );
      map.insert(
        Key::new(KeyCode::PageUp, KeyMods::NONE),
        Action::ScrollUp {
          n: 1,
          unit: ScrollUnit::Screen,
        },
      );
      map.insert(
        Key::new(KeyCode::PageDown, KeyMods::NONE),
        Action::ScrollDown {
          n: 1,
          unit: ScrollUnit::Screen,
        },
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
    self.keymap_tasks.insert(key, event);
  }

  fn keymap_add_t(&mut self, key: Key, event: Action) {
    self.keymap_term.insert(key, event);
  }

  fn keymap_add_c(&mut self, key: Key, event: Action) {
    self.keymap_copy.insert(key, event);
  }

  /// Build the runtime [`Keymap`] from the merged bindings.
  pub fn build(&self) -> Keymap {
    let mut keymap = Keymap::new();
    for (key, event) in &self.keymap_tasks {
      keymap.bind(KeymapGroup::Tasks, *key, event.clone());
    }
    for (key, event) in &self.keymap_term {
      keymap.bind(KeymapGroup::Term, *key, event.clone());
    }
    for (key, event) in &self.keymap_copy {
      keymap.bind(KeymapGroup::Copy, *key, event.clone());
    }
    keymap
  }
}

fn add_keys(
  into: &mut IndexMap<Key, Action>,
  node: &CfgNode<'_>,
) -> Result<()> {
  let obj = node.as_obj()?;
  if let Some(reset) = obj.get("reset") {
    if reset.as_bool()? {
      into.clear();
    }
  }
  for (key, event) in obj.iter() {
    if key == "reset" {
      continue;
    }
    let key = KeySpec::parse(key)?.key();
    if event.is_null() {
      into.shift_remove(&key);
    } else {
      let raw = event.raw().clone();
      // `q: quit` is shorthand for `q: {action: quit}`.
      let raw = match raw {
        serde_yaml::Value::String(name) => {
          let mut map = serde_yaml::Mapping::new();
          map.insert("action".into(), name.into());
          serde_yaml::Value::Mapping(map)
        }
        raw => raw,
      };
      let event: Action = serde_yaml::from_value(raw)?;
      into.insert(key, event);
    }
  }
  Ok(())
}

#[cfg(test)]
mod tests {
  use crate::cfg::{CfgCx, CfgDoc};

  use super::*;

  #[test]
  fn config_spellings() {
    let yaml = r#"
keymap:
  tasks:
    <q>: detach
    <C-d>: {action: scroll-down, n: 3, unit: line}
    <C-u>: scroll-up
    <j>: null
"#;
    let value: serde_yaml::Value = serde_yaml::from_str(yaml).unwrap();
    let cx = CfgCx::new(std::path::PathBuf::from("."));
    let doc = CfgDoc::from_value(value, &cx).unwrap();
    let mut config = KeymapConfig::default();
    config.merge(&doc.root().as_obj().unwrap()).unwrap();
    let key = |s: &str| KeySpec::parse(s).unwrap().key();
    assert_eq!(config.keymap_tasks.get(&key("<q>")), Some(&Action::Detach));
    assert_eq!(
      config.keymap_tasks.get(&key("<C-d>")),
      Some(&Action::ScrollDown {
        n: 3,
        unit: ScrollUnit::Line
      })
    );
    assert_eq!(
      config.keymap_tasks.get(&key("<C-u>")),
      Some(&Action::ScrollUp {
        n: 1,
        unit: ScrollUnit::HalfScreen
      })
    );
    assert_eq!(config.keymap_tasks.get(&key("<j>")), None);
  }
}
