use std::collections::{HashMap, HashSet};

use super::{
  kernel_message::SpaceSelector,
  path_trie::{PathConflict, PathTrie},
  sub_trie::{SubMode, SubTrie},
  task::TaskId,
  task_key::{TaskKey, TaskSpaceId},
  task_path::TaskPath,
};

/// Naming and pub/sub for tasks: resolves paths to ids and routes
/// notifications to subscribers. Bundles the path index and the subscription
/// trie behind one façade so the graph deals with a single collaborator for
/// everything name- and subscription-related.
pub struct Namespace {
  spaces: HashMap<TaskSpaceId, SpaceNamespace>,
}

struct SpaceNamespace {
  paths: PathTrie,
  subs: SubTrie,
}

impl SpaceNamespace {
  fn new() -> Self {
    Self {
      paths: PathTrie::new(),
      subs: SubTrie::new(),
    }
  }
}

impl Namespace {
  pub fn new() -> Self {
    Self {
      spaces: HashMap::new(),
    }
  }

  fn space_mut(&mut self, space: &TaskSpaceId) -> &mut SpaceNamespace {
    self
      .spaces
      .entry(space.clone())
      .or_insert_with(SpaceNamespace::new)
  }

  // ---- Naming ----

  pub fn insert(
    &mut self,
    key: &TaskKey,
    id: TaskId,
    after: Option<&TaskPath>,
  ) -> Result<(), PathConflict> {
    self
      .space_mut(&key.space)
      .paths
      .insert(&key.path, id, after)
  }

  pub fn remove(&mut self, key: &TaskKey) -> Option<TaskId> {
    self
      .spaces
      .get_mut(&key.space)
      .and_then(|space| space.paths.remove(&key.path))
  }

  pub fn place(&mut self, key: &TaskKey) {
    self.space_mut(&key.space).paths.place(&key.path);
  }

  pub fn prune(&mut self, key: &TaskKey) {
    if let Some(space) = self.spaces.get_mut(&key.space) {
      space.paths.prune(&key.path);
    }
  }

  /// The task listed just before the one at `key`.
  pub fn before(&self, key: &TaskKey) -> Option<TaskId> {
    self.spaces.get(&key.space)?.paths.before(&key.path)
  }

  /// See `PathTrie::position`.
  pub fn position(
    &self,
    space: &TaskSpaceId,
    path: &TaskPath,
  ) -> Option<Vec<usize>> {
    self.spaces.get(space)?.paths.position(path)
  }

  pub fn glob(&self, space: &SpaceSelector, pattern: &str) -> Vec<TaskId> {
    let spaces: Vec<&SpaceNamespace> = match space {
      SpaceSelector::One(space) => self.spaces.get(space).into_iter().collect(),
      SpaceSelector::Any => self.spaces.values().collect(),
    };
    spaces
      .into_iter()
      .flat_map(|ns| ns.paths.glob(pattern))
      .map(|(_, id)| id)
      .collect()
  }

  /// Tasks a subscription at `key` with `mode` would match.
  pub fn in_scope(
    &self,
    key: &TaskKey,
    mode: SubMode,
  ) -> Vec<(TaskPath, TaskId)> {
    let Some(space) = self.spaces.get(&key.space) else {
      return Vec::new();
    };
    let mut result = Vec::new();
    if let Some(id) = space.paths.resolve(&key.path) {
      result.push((key.path.clone(), id));
    }
    match mode {
      SubMode::Exact => (),
      SubMode::Subtree => result.extend(space.paths.descendants(&key.path)),
    }
    result
  }

  // ---- Pub/sub ----

  pub fn subscribe(
    &mut self,
    subscriber: TaskId,
    key: &TaskKey,
    mode: SubMode,
  ) {
    self
      .space_mut(&key.space)
      .subs
      .subscribe(subscriber, &key.path, mode);
  }

  pub fn unsubscribe(
    &mut self,
    subscriber: TaskId,
    key: &TaskKey,
    mode: SubMode,
  ) {
    if let Some(space) = self.spaces.get_mut(&key.space) {
      space.subs.unsubscribe(subscriber, &key.path, mode);
    }
  }

  pub fn remove_subscriber(&mut self, subscriber: TaskId) {
    for space in self.spaces.values_mut() {
      space.subs.remove_subscriber(subscriber);
    }
  }

  pub fn is_subscribed(
    &self,
    subscriber: TaskId,
    space: &TaskSpaceId,
    path: &TaskPath,
  ) -> bool {
    self
      .spaces
      .get(space)
      .is_some_and(|ns| ns.subs.is_subscribed(subscriber, path))
  }

  pub fn collect(&self, key: &TaskKey, out: &mut HashSet<TaskId>) {
    if let Some(space) = self.spaces.get(&key.space) {
      space.subs.collect(&key.path, out);
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn subscriptions_are_space_local() {
    let mut ns = Namespace::new();
    let subscriber = TaskId(1);
    ns.subscribe(
      subscriber,
      &TaskKey::default_space(TaskPath::root()),
      SubMode::Subtree,
    );

    let mut default_targets = HashSet::new();
    ns.collect(
      &TaskKey::default_space(TaskPath::new("app").unwrap()),
      &mut default_targets,
    );
    assert_eq!(default_targets, HashSet::from([subscriber]));

    let mut dekit_targets = HashSet::new();
    ns.collect(
      &TaskKey::new(TaskSpaceId::dekit(), TaskPath::new("console").unwrap()),
      &mut dekit_targets,
    );
    assert!(dekit_targets.is_empty());
  }
}
