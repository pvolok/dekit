use std::collections::HashMap;

use indexmap::IndexMap;

use crate::console::action::Action;
use crate::term::key::Key;

/// The bindings of one group: a key sequence to its action. No sequence
/// is the start of another, so a sequence runs as soon as it is typed.
#[derive(Default)]
struct Bindings {
  by_keys: IndexMap<Vec<Key>, Action>,
  by_action: HashMap<Action, Vec<Key>>,
}

impl Bindings {
  /// The bindings that go on from `keys`.
  fn after<'a>(
    &'a self,
    keys: &'a [Key],
  ) -> impl Iterator<Item = (&'a [Key], &'a Action)> {
    self
      .by_keys
      .iter()
      .filter(move |(bound, _)| {
        bound.len() > keys.len() && bound.starts_with(keys)
      })
      .map(|(bound, action)| (bound.as_slice(), action))
  }
}

#[derive(Default)]
pub struct Keymap {
  groups: [Bindings; KeymapGroup::ALL.len()],
}

/// `Global` holds in every scope, after the scope's own group.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum KeymapGroup {
  Global,
  Tasks,
  Term,
  Copy,
}

impl KeymapGroup {
  pub const ALL: [KeymapGroup; 4] = [
    KeymapGroup::Global,
    KeymapGroup::Tasks,
    KeymapGroup::Term,
    KeymapGroup::Copy,
  ];
}

/// What a key comes to after the keys before it.
pub enum Lookup<'a> {
  Action(&'a Action),
  /// More keys to come; the key was added to the pending ones.
  Prefix,
  None,
}

impl Keymap {
  pub fn new() -> Self {
    Self::default()
  }

  pub fn bind(&mut self, group: KeymapGroup, keys: Vec<Key>, action: Action) {
    let bindings = &mut self.groups[group as usize];
    bindings.by_keys.insert(keys.clone(), action.clone());
    bindings.by_action.insert(action, keys);
  }

  /// The group's bindings, then the global ones.
  fn tables(&self, group: KeymapGroup) -> [&Bindings; 2] {
    [
      &self.groups[group as usize],
      &self.groups[KeymapGroup::Global as usize],
    ]
  }

  /// Takes `key` as the one after `pending`, which is left empty unless
  /// more keys are to come.
  pub fn lookup(
    &self,
    group: KeymapGroup,
    pending: &mut Vec<Key>,
    key: &Key,
  ) -> Lookup<'_> {
    let tables = self.tables(group);
    pending.push(key.plain());
    let bound = tables
      .iter()
      .find_map(|bindings| bindings.by_keys.get(pending.as_slice()));
    if let Some(action) = bound {
      pending.clear();
      return Lookup::Action(action);
    }
    if tables
      .iter()
      .any(|bindings| bindings.after(pending).next().is_some())
    {
      return Lookup::Prefix;
    }
    pending.clear();
    Lookup::None
  }

  /// The keys bound to an action in the group, or else globally.
  pub fn keys(&self, group: KeymapGroup, action: &Action) -> Option<&[Key]> {
    self
      .tables(group)
      .iter()
      .find_map(|bindings| bindings.by_action.get(action))
      .map(Vec::as_slice)
  }

  /// The first keys of bindings that take more than one, the leader
  /// for one.
  pub fn prefix_keys(&self, group: KeymapGroup) -> Vec<&Key> {
    let mut keys: Vec<&Key> = Vec::new();
    for bindings in self.tables(group) {
      for (bound, _) in bindings.after(&[]) {
        if bound.len() > 1 && !keys.contains(&&bound[0]) {
          keys.push(&bound[0]);
        }
      }
    }
    keys
  }

  /// Each key that can follow `pending`, with the action it runs, or
  /// `None` when more keys follow it.
  pub fn next_keys<'a>(
    &'a self,
    group: KeymapGroup,
    pending: &'a [Key],
  ) -> IndexMap<&'a Key, Option<&'a Action>> {
    let mut next = IndexMap::new();
    for bindings in self.tables(group) {
      for (keys, action) in bindings.after(pending) {
        let last = keys.len() == pending.len() + 1;
        next
          .entry(&keys[pending.len()])
          .or_insert(last.then_some(action));
      }
    }
    next
  }
}

/// Keys as hints show them: `C-a`, `` ` t r ``.
pub fn keys_label(keys: &[Key]) -> String {
  use std::fmt::Write;
  let mut label = String::new();
  for key in keys {
    if !label.is_empty() {
      label.push(' ');
    }
    let _ = write!(label, "{key}");
  }
  label
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::term::key::KeyCode;

  fn key(ch: char) -> Key {
    KeyCode::Char(ch).into()
  }

  fn keymap() -> Keymap {
    let mut keymap = Keymap::new();
    keymap.bind(KeymapGroup::Global, vec![key('`'), key('z')], Action::Zoom);
    keymap.bind(
      KeymapGroup::Global,
      vec![key('`'), key('t'), key('r')],
      Action::RestartTask,
    );
    keymap.bind(KeymapGroup::Tasks, vec![key('j')], Action::NextTask);
    keymap.bind(KeymapGroup::Tasks, vec![key('J')], Action::MoveTaskDown);
    keymap
  }

  /// Types the keys, giving the action of the last one.
  fn press(
    keymap: &Keymap,
    group: KeymapGroup,
    keys: &[Key],
  ) -> Option<Action> {
    let mut pending = Vec::new();
    let mut action = None;
    for key in keys {
      action = match keymap.lookup(group, &mut pending, key) {
        Lookup::Action(action) => Some(action.clone()),
        Lookup::Prefix | Lookup::None => None,
      };
    }
    action
  }

  #[test]
  fn global_sequences_run_in_every_group() {
    let keymap = keymap();
    for group in [KeymapGroup::Tasks, KeymapGroup::Term, KeymapGroup::Copy] {
      assert_eq!(
        press(&keymap, group, &[key('`'), key('z')]),
        Some(Action::Zoom)
      );
      assert_eq!(
        press(&keymap, group, &[key('`'), key('t'), key('r')]),
        Some(Action::RestartTask)
      );
    }
    assert_eq!(press(&keymap, KeymapGroup::Term, &[key('j')]), None);
  }

  #[test]
  fn a_key_no_binding_goes_on_with_ends_the_sequence() {
    let keymap = keymap();
    let mut pending = Vec::new();
    let group = KeymapGroup::Tasks;
    assert!(matches!(
      keymap.lookup(group, &mut pending, &key('`')),
      Lookup::Prefix
    ));
    assert_eq!(pending, [key('`')]);
    assert!(matches!(
      keymap.lookup(group, &mut pending, &key('j')),
      Lookup::None
    ));
    assert!(pending.is_empty());
    // And the next key starts over.
    assert!(matches!(
      keymap.lookup(group, &mut pending, &key('j')),
      Lookup::Action(Action::NextTask)
    ));
  }

  #[test]
  fn next_keys_are_the_ways_on() {
    let keymap = keymap();
    let leader = [key('`')];
    let next = keymap.next_keys(KeymapGroup::Term, &leader);
    let next: Vec<_> = next.into_iter().collect();
    assert_eq!(next, [(&key('z'), Some(&Action::Zoom)), (&key('t'), None)]);
    assert_eq!(
      keys_label(
        keymap
          .keys(KeymapGroup::Term, &Action::RestartTask)
          .unwrap()
      ),
      "` t r"
    );
  }
}
