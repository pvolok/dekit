use anyhow::{Result, bail};
use indexmap::IndexMap;

use crate::cfg::{CfgNode, CfgObj};
use crate::console::action::{Action, CopyMove, FoldOp, RunnerTab, ScrollUnit};
use crate::console::keymap::{Keymap, KeymapGroup};
use crate::term::key::{Key, KeyCode};

/// One key of a binding as the config spells it.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
enum SeqKey {
  /// `<leader>`: whatever key `keymap.leader` names.
  Leader,
  Key(Key),
}

/// The keys of a binding, typed one after another, in vim notation:
/// `gg`, `<C-w>v`, `<leader>z`. A character is its own key and `<...>`
/// names one.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct KeySeq(Vec<SeqKey>);

impl KeySeq {
  pub fn parse(text: &str) -> Result<KeySeq> {
    // A key's name without its `<>` would be its letters one by one.
    if !text.starts_with('<')
      && text.chars().count() > 1
      && Key::parse(&format!("<{text}>")).is_ok()
    {
      bail!("'{text}' is not a key: write '<{text}>'");
    }
    let mut keys = Vec::new();
    let mut rest = text;
    while let Some(ch) = rest.chars().next() {
      // A `<` that no `>` closes is the character.
      let len = match rest.find('>') {
        Some(close) if ch == '<' => close + 1,
        _ => ch.len_utf8(),
      };
      let (spec, tail) = rest.split_at(len);
      keys.push(if spec.eq_ignore_ascii_case("<leader>") {
        SeqKey::Leader
      } else if spec.starts_with('<') && len > 1 {
        SeqKey::Key(Key::parse(spec)?)
      } else {
        SeqKey::Key(KeyCode::Char(ch).into())
      });
      rest = tail;
    }
    if keys.is_empty() {
      bail!("Expected a key");
    }
    Ok(KeySeq(keys))
  }

  fn keys(&self, leader: &Key) -> Vec<Key> {
    let key = |key: &SeqKey| match key {
      SeqKey::Leader => leader.clone(),
      SeqKey::Key(key) => key.clone(),
    };
    self.0.iter().map(key).collect()
  }

  /// Whether one is the other or starts it. Nothing times a sequence
  /// out, so a group can bind only one of the two.
  fn overlaps(&self, other: &KeySeq) -> bool {
    let len = self.0.len().min(other.0.len());
    self.0[..len] == other.0[..len]
  }
}

impl From<Key> for KeySeq {
  fn from(key: Key) -> Self {
    KeySeq(vec![SeqKey::Key(key)])
  }
}

type Bindings = IndexMap<KeySeq, Action>;

#[derive(Clone, Debug)]
pub struct KeymapConfig {
  leader: Key,
  groups: [Bindings; KeymapGroup::ALL.len()],
}

impl Default for KeymapConfig {
  fn default() -> Self {
    let mut keymap = Self::classic();
    keymap.add_global_defaults();
    keymap
  }
}

impl KeymapConfig {
  /// The bindings mprocs has: nothing global, so no leader either.
  pub fn classic() -> Self {
    let mut keymap = Self {
      leader: KeyCode::Char('`').into(),
      groups: Default::default(),
    };
    keymap.add_defaults();
    keymap
  }

  /// Takes the file's `keymap`, giving what there is to warn of.
  pub fn merge(&mut self, obj: &CfgObj<'_>) -> Result<Vec<String>> {
    let mut warnings = Vec::new();
    let keymap = match obj.get("keymap") {
      Some(node) => node.as_obj()?,
      None => return Ok(warnings),
    };
    let groups = [
      ("global", KeymapGroup::Global),
      ("tasks", KeymapGroup::Tasks),
      ("term", KeymapGroup::Term),
      ("term_copy", KeymapGroup::Copy),
    ];
    let mut known = vec!["leader"];
    known.extend(groups.iter().map(|(name, _)| *name));
    keymap.known_keys(&known)?;
    if let Some(leader) = keymap.get("leader") {
      self.leader = match KeySeq::parse(leader.as_str()?)?.0.as_slice() {
        [SeqKey::Key(key)] => key.clone(),
        _ => return Err(leader.error("expected one key")),
      };
      // The key is the leader's now: what it ran or started would
      // keep the leader from being one.
      let taken = KeySeq::from(self.leader.clone());
      for bindings in &mut self.groups {
        bindings.retain(|keys, _| !keys.overlaps(&taken));
      }
    }
    for (name, group) in groups {
      if let Some(node) = keymap.get(name) {
        add_keys(self.group_mut(group), &node, &mut warnings)?;
      }
    }
    Ok(warnings)
  }

  pub fn group_mut(&mut self, group: KeymapGroup) -> &mut Bindings {
    &mut self.groups[group as usize]
  }

  fn bind(&mut self, group: KeymapGroup, keys: &str, action: Action) {
    let keys = KeySeq::parse(keys).expect("the keys of a default binding");
    self.group_mut(group).insert(keys, action);
  }

  fn add_defaults(&mut self) {
    use KeymapGroup::{Copy, Tasks, Term};

    let scroll_up = |n, unit| Action::ScrollUp { n, unit };
    let scroll_down = |n, unit| Action::ScrollDown { n, unit };
    let fold = |op| Action::Fold {
      op,
      target: Some("**".to_string()),
    };
    let ctrl_c = Key::parse("<C-c>").expect("a key");

    for group in [Tasks, Term, Copy] {
      self.bind(group, "<C-a>", Action::ToggleFocus);
    }

    // Of two keys for an action, hints show the later one.
    for (keys, action) in [
      ("q", Action::ShowQuit),
      ("Q", Action::Down),
      ("p", Action::ShowCommandsMenu),
      (
        "P",
        Action::ShowRunnerSelect {
          tab: RunnerTab::Switch,
        },
      ),
      ("<Down>", Action::NextTask),
      ("j", Action::NextTask),
      ("<Up>", Action::PrevTask),
      ("k", Action::PrevTask),
      ("<Left>", Action::Collapse),
      ("h", Action::Collapse),
      ("<Right>", Action::Expand),
      ("l", Action::Expand),
      ("H", fold(FoldOp::Close)),
      ("L", fold(FoldOp::Open)),
      ("s", Action::StartTask),
      ("x", Action::StopTask),
      ("X", Action::KillTask),
      ("r", Action::RestartTask),
      ("R", Action::ForceRestartTask),
      ("e", Action::ShowEditTask),
      ("<C-c>", Action::SendKey { key: ctrl_c }),
      ("a", Action::ShowAddTask),
      ("C", Action::ShowDuplicateTask),
      ("<Space>", Action::ShowTaskMenu),
      ("K", Action::MoveTaskUp),
      ("J", Action::MoveTaskDown),
      ("y", Action::CopyTaskCommand),
      ("d", Action::ShowRemoveTask),
      ("z", Action::Zoom),
      ("?", Action::ToggleKeymapWindow),
      ("v", Action::CopyModeEnter),
    ] {
      self.bind(Tasks, keys, action);
    }
    #[cfg(debug_assertions)]
    self.bind(Tasks, "T", Action::ToggleTheme);
    for index in 0..8 {
      let keys = format!("<M-{}>", index + 1);
      self.bind(Tasks, &keys, Action::SelectTask { index });
    }

    for group in [Tasks, Copy] {
      for (keys, action) in [
        ("<C-y>", scroll_up(3, ScrollUnit::Line)),
        ("<C-e>", scroll_down(3, ScrollUnit::Line)),
        ("<C-u>", scroll_up(1, ScrollUnit::HalfScreen)),
        ("<C-d>", scroll_down(1, ScrollUnit::HalfScreen)),
        ("<PageUp>", scroll_up(1, ScrollUnit::Screen)),
        ("<PageDown>", scroll_down(1, ScrollUnit::Screen)),
      ] {
        self.bind(group, keys, action);
      }
    }

    for (keys, action) in [
      ("<Esc>", Action::CopyModeLeave),
      ("v", Action::CopyModeEnd),
      ("c", Action::CopyModeCopy),
    ] {
      self.bind(Copy, keys, action);
    }
    for (arrow, letter, dir) in [
      ("<Up>", "k", CopyMove::Up),
      ("<Right>", "l", CopyMove::Right),
      ("<Down>", "j", CopyMove::Down),
      ("<Left>", "h", CopyMove::Left),
    ] {
      for keys in [arrow, letter] {
        self.bind(Copy, keys, Action::CopyModeMove { dir });
      }
    }
  }

  /// The keys after the leader. `h` `j` `k` `l` go by where things are
  /// on screen, the rest are tmux's where it has the same thing, and
  /// `t` leads to the selected task's keys of the task list.
  fn add_global_defaults(&mut self) {
    let keys = [
      ("j", Action::NextTask),
      ("k", Action::PrevTask),
      ("h", Action::FocusTasks),
      ("l", Action::FocusTerm),
      ("z", Action::Zoom),
      ("[", Action::CopyModeEnter),
      (":", Action::ShowCommandsMenu),
      (
        "s",
        Action::ShowRunnerSelect {
          tab: RunnerTab::Switch,
        },
      ),
      ("?", Action::ToggleKeymapWindow),
      ("d", Action::Detach),
      ("q", Action::ShowQuit),
      ("ts", Action::StartTask),
      ("tx", Action::StopTask),
      ("tX", Action::KillTask),
      ("tr", Action::RestartTask),
      ("tR", Action::ForceRestartTask),
      ("te", Action::ShowEditTask),
      ("ta", Action::ShowAddTask),
      ("tC", Action::ShowDuplicateTask),
      ("td", Action::ShowRemoveTask),
      ("ty", Action::CopyTaskCommand),
      ("<leader>", Action::SendLeader),
    ];
    for (keys, action) in keys {
      self.bind(KeymapGroup::Global, &format!("<leader>{keys}"), action);
    }
    for index in 0..9 {
      let keys = format!("<leader>{}", index + 1);
      self.bind(KeymapGroup::Global, &keys, Action::SelectTask { index });
    }
  }

  /// Build the runtime [`Keymap`] from the merged bindings.
  pub fn build(&self) -> Keymap {
    let mut keymap = Keymap::new();
    for group in KeymapGroup::ALL {
      for (keys, action) in &self.groups[group as usize] {
        keymap.bind(group, keys.keys(&self.leader), action.clone());
      }
    }
    keymap
  }
}

fn add_keys(
  into: &mut Bindings,
  node: &CfgNode<'_>,
  warnings: &mut Vec<String>,
) -> Result<()> {
  let obj = node.as_obj()?;
  if let Some(reset) = obj.get("reset") {
    if reset.as_bool()? {
      into.clear();
    }
  }
  let mut added: Vec<(&str, KeySeq)> = Vec::new();
  for (name, event) in obj.iter() {
    if name == "reset" {
      continue;
    }
    let keys = KeySeq::parse(name)?;
    if event.is_null() {
      into.shift_remove(&keys);
      continue;
    }
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
    // It takes the place of what it starts, and of what starts it: of
    // a default without a word, of a binding next to it with one.
    added.retain(|(other, bound)| {
      let gives_way = bound.overlaps(&keys);
      if gives_way {
        let dropped =
          format!("'{other}' is dropped for '{name}' (one starts the other)");
        warnings.push(node.error(dropped).to_string());
      }
      !gives_way
    });
    into.retain(|bound, _| !bound.overlaps(&keys));
    into.insert(keys.clone(), event);
    added.push((name, keys));
  }
  Ok(())
}

#[cfg(test)]
mod tests {
  use crate::cfg::CfgDoc;
  use crate::console::keymap::{Lookup, keys_label};

  use super::*;

  fn merge(config: &mut KeymapConfig, yaml: &str) -> Result<Vec<String>> {
    let value: serde_yaml::Value = serde_yaml::from_str(yaml).unwrap();
    config.merge(&CfgDoc::from_value(value).root().as_obj().unwrap())
  }

  fn merged(yaml: &str) -> Result<KeymapConfig> {
    let mut config = KeymapConfig::default();
    merge(&mut config, yaml)?;
    Ok(config)
  }

  fn warnings(yaml: &str) -> Vec<String> {
    merge(&mut KeymapConfig::default(), yaml).unwrap()
  }

  fn bound<'a>(
    config: &'a KeymapConfig,
    group: KeymapGroup,
    keys: &str,
  ) -> Option<&'a Action> {
    config.groups[group as usize].get(&KeySeq::parse(keys).unwrap())
  }

  #[test]
  fn config_spellings() {
    let config = merged(
      r#"
keymap:
  tasks:
    <q>: detach
    <C-d>: {action: scroll-down, n: 3, unit: line}
    <C-u>: scroll-up
    j: null
    <F2>: show-rename-task
"#,
    )
    .unwrap();
    let tasks = |keys| bound(&config, KeymapGroup::Tasks, keys);
    // A key in `<>` is the key.
    assert_eq!(tasks("q"), Some(&Action::Detach));
    // The name the edit dialog had as the rename dialog.
    assert_eq!(tasks("<F2>"), Some(&Action::ShowEditTask));
    assert_eq!(
      tasks("<C-d>"),
      Some(&Action::ScrollDown {
        n: 3,
        unit: ScrollUnit::Line
      })
    );
    assert_eq!(
      tasks("<C-u>"),
      Some(&Action::ScrollUp {
        n: 1,
        unit: ScrollUnit::HalfScreen
      })
    );
    assert_eq!(tasks("j"), None);
  }

  #[test]
  fn sequences_are_keys_one_after_another() {
    let key = |text| SeqKey::Key(Key::parse(text).unwrap());
    assert_eq!(KeySeq::parse("gg").unwrap().0, [key("<g>"), key("<g>")]);
    assert_eq!(
      KeySeq::parse("<C-w>v<Enter>").unwrap().0,
      [key("<C-w>"), key("<v>"), key("<Enter>")]
    );
    assert_eq!(
      KeySeq::parse("<Leader>ё<").unwrap().0,
      [SeqKey::Leader, key("<ё>"), key("<lt>")]
    );
    assert!(KeySeq::parse("").is_err());
    assert!(KeySeq::parse("<Bogus>").is_err());
    // A name that lost its `<>` is not five keys.
    for name in ["del", "Enter", "C-r", "F5"] {
      let err = KeySeq::parse(name).unwrap_err().to_string();
      assert!(err.contains(&format!("<{name}>")), "{err}");
    }
    assert!(merged("keymap: {tasks: {del: show-remove-task}}").is_err());
  }

  #[test]
  fn a_binding_replaces_what_it_starts_and_what_starts_it() {
    let config = merged(
      r#"
keymap:
  tasks:
    gg: next-task
    q: null
  global:
    <leader>t: restart-task
"#,
    )
    .unwrap();
    let global = |keys| bound(&config, KeymapGroup::Global, keys);
    assert_eq!(global("<leader>t"), Some(&Action::RestartTask));
    assert_eq!(global("<leader>tr"), None);
    assert_eq!(global("<leader>z"), Some(&Action::Zoom));

    let mut config = merged("keymap: {tasks: {gg: next-task}}").unwrap();
    merge(&mut config, "keymap: {tasks: {g: prev-task}}").unwrap();
    let tasks = |keys| bound(&config, KeymapGroup::Tasks, keys);
    assert_eq!(tasks("g"), Some(&Action::PrevTask));
    assert_eq!(tasks("gg"), None);

    // Of two in one file the later one stays, and the file is told.
    let yaml = "keymap: {tasks: {g: prev-task, gg: next-task, q: null}}";
    let config = merged(yaml).unwrap();
    let tasks = |keys| bound(&config, KeymapGroup::Tasks, keys);
    assert_eq!(tasks("g"), None);
    assert_eq!(tasks("gg"), Some(&Action::NextTask));
    assert_eq!(
      warnings(yaml),
      ["'g' is dropped for 'gg' (one starts the other) at \
         <config>.keymap.tasks"]
    );
    // Taking a default's place is what a config is for.
    assert!(warnings("keymap: {global: {<leader>t: zoom}}").is_empty());
  }

  #[test]
  fn the_leader_is_set_once_for_every_binding() {
    let config = merged("keymap: {leader: <C-b>}").unwrap();
    let keymap = config.build();
    let zoom = keymap.keys(KeymapGroup::Term, &Action::Zoom).unwrap();
    assert_eq!(keys_label(zoom), "C-b z");

    // Twice, it is sent on.
    let mut pending = Vec::new();
    let leader = Key::parse("<C-b>").unwrap();
    let group = KeymapGroup::Term;
    assert!(matches!(
      keymap.lookup(group, &mut pending, &leader),
      Lookup::Prefix
    ));
    assert!(matches!(
      keymap.lookup(group, &mut pending, &leader),
      Lookup::Action(Action::SendLeader)
    ));

    // And the key is the leader's, whatever it was bound to.
    let key = Key::parse("<C-a>").unwrap();
    let keymap = merged("keymap: {leader: <C-a>}").unwrap().build();
    for group in [KeymapGroup::Tasks, KeymapGroup::Term, KeymapGroup::Copy] {
      let mut pending = Vec::new();
      assert!(matches!(
        keymap.lookup(group, &mut pending, &key),
        Lookup::Prefix
      ));
    }
    // Unless the file binds it next to that.
    let yaml = "keymap: {leader: <C-a>, term: {<C-a>: toggle-focus}}";
    let keymap = merged(yaml).unwrap().build();
    assert!(matches!(
      keymap.lookup(KeymapGroup::Term, &mut Vec::new(), &key),
      Lookup::Action(Action::ToggleFocus)
    ));

    assert!(merged("keymap: {leader: gg}").is_err());
    assert!(merged("keymap: {leader: <leader>}").is_err());
  }

  #[test]
  fn classic_has_no_leader_keys() {
    let config = KeymapConfig::classic();
    let keymap = config.build();
    let mut pending = Vec::new();
    let leader = config.leader.clone();
    assert!(matches!(
      keymap.lookup(KeymapGroup::Term, &mut pending, &leader),
      Lookup::None
    ));
  }
}
