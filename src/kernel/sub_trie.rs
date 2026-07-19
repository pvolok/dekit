use std::collections::{BTreeMap, HashMap, HashSet};

use super::task::TaskId;
use super::task_path::TaskPath;

/// How a subscriber wants to match a path.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum SubMode {
  /// Notifications for this exact path only.
  Exact,
  /// Notifications for this path and all of its descendants.
  Subtree,
}

#[derive(Default)]
struct SubNode {
  exact: HashSet<TaskId>,
  subtree: HashSet<TaskId>,
  children: BTreeMap<String, SubNode>,
}

impl SubNode {
  fn is_empty(&self) -> bool {
    self.exact.is_empty() && self.subtree.is_empty() && self.children.is_empty()
  }
}

pub struct SubTrie {
  root: SubNode,
  by_subscriber: HashMap<TaskId, HashSet<(TaskPath, SubMode)>>,
}

impl SubTrie {
  pub fn new() -> Self {
    Self {
      root: SubNode::default(),
      by_subscriber: HashMap::new(),
    }
  }

  pub fn subscribe(
    &mut self,
    subscriber: TaskId,
    path: &TaskPath,
    mode: SubMode,
  ) {
    let mut node = &mut self.root;
    for component in path.components() {
      node = node.children.entry(component.to_string()).or_default();
    }
    match mode {
      SubMode::Exact => node.exact.insert(subscriber),
      SubMode::Subtree => node.subtree.insert(subscriber),
    };
    self
      .by_subscriber
      .entry(subscriber)
      .or_default()
      .insert((path.clone(), mode));
  }

  pub fn unsubscribe(
    &mut self,
    subscriber: TaskId,
    path: &TaskPath,
    mode: SubMode,
  ) {
    let components: Vec<&str> = path.components().collect();
    Self::remove_one(&mut self.root, &components, subscriber, mode);
    if let Some(set) = self.by_subscriber.get_mut(&subscriber) {
      set.remove(&(path.clone(), mode));
      if set.is_empty() {
        self.by_subscriber.remove(&subscriber);
      }
    }
  }

  /// Drop every subscription held by `subscriber`.
  pub fn remove_subscriber(&mut self, subscriber: TaskId) {
    let Some(subs) = self.by_subscriber.remove(&subscriber) else {
      return;
    };
    for (path, mode) in subs {
      let components: Vec<&str> = path.components().collect();
      Self::remove_one(&mut self.root, &components, subscriber, mode);
    }
  }

  pub fn collect(&self, path: &TaskPath, out: &mut HashSet<TaskId>) {
    let mut node = &self.root;
    out.extend(node.subtree.iter().copied());
    let mut reached = true;
    for component in path.components() {
      match node.children.get(component) {
        Some(child) => {
          node = child;
          out.extend(node.subtree.iter().copied());
        }
        None => {
          reached = false;
          break;
        }
      }
    }
    if reached {
      out.extend(node.exact.iter().copied());
    }
  }

  fn remove_one(
    node: &mut SubNode,
    components: &[&str],
    subscriber: TaskId,
    mode: SubMode,
  ) {
    match components.split_first() {
      Some((first, rest)) => {
        if let Some(child) = node.children.get_mut(*first) {
          Self::remove_one(child, rest, subscriber, mode);
          if child.is_empty() {
            node.children.remove(*first);
          }
        }
      }
      None => match mode {
        SubMode::Exact => {
          node.exact.remove(&subscriber);
        }
        SubMode::Subtree => {
          node.subtree.remove(&subscriber);
        }
      },
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  fn path(s: &str) -> TaskPath {
    TaskPath::new(s).unwrap()
  }

  fn collect(trie: &SubTrie, p: &str) -> HashSet<TaskId> {
    let mut out = HashSet::new();
    trie.collect(&path(p), &mut out);
    out
  }

  #[test]
  fn exact_matches_only_that_path() {
    let mut trie = SubTrie::new();
    trie.subscribe(TaskId(1), &path("services/api"), SubMode::Exact);

    assert_eq!(collect(&trie, "services/api"), HashSet::from([TaskId(1)]));
    assert!(collect(&trie, "services/api/v2").is_empty());
    assert!(collect(&trie, "services").is_empty());
  }

  #[test]
  fn subtree_matches_path_and_descendants() {
    let mut trie = SubTrie::new();
    trie.subscribe(TaskId(1), &path("services"), SubMode::Subtree);

    assert_eq!(collect(&trie, "services"), HashSet::from([TaskId(1)]));
    assert_eq!(collect(&trie, "services/api"), HashSet::from([TaskId(1)]));
    assert_eq!(
      collect(&trie, "services/api/v2"),
      HashSet::from([TaskId(1)])
    );
    assert!(collect(&trie, "tools").is_empty());
  }

  #[test]
  fn root_subtree_matches_everything() {
    let mut trie = SubTrie::new();
    trie.subscribe(TaskId(1), &TaskPath::root(), SubMode::Subtree);

    assert_eq!(collect(&trie, "a"), HashSet::from([TaskId(1)]));
    assert_eq!(collect(&trie, "a/b/c"), HashSet::from([TaskId(1)]));
  }

  #[test]
  fn overlapping_subscriptions_dedup() {
    let mut trie = SubTrie::new();
    trie.subscribe(TaskId(1), &path("a"), SubMode::Subtree);
    trie.subscribe(TaskId(1), &path("a/b"), SubMode::Exact);

    // Matches both subscriptions but is delivered as a single subscriber.
    assert_eq!(collect(&trie, "a/b"), HashSet::from([TaskId(1)]));
  }

  #[test]
  fn unsubscribe_removes_match() {
    let mut trie = SubTrie::new();
    trie.subscribe(TaskId(1), &path("a"), SubMode::Subtree);
    trie.unsubscribe(TaskId(1), &path("a"), SubMode::Subtree);
    assert!(collect(&trie, "a/b").is_empty());
  }

  #[test]
  fn unsubscribe_only_removes_named_mode() {
    let mut trie = SubTrie::new();
    trie.subscribe(TaskId(1), &path("a"), SubMode::Subtree);
    trie.subscribe(TaskId(1), &path("a"), SubMode::Exact);
    trie.unsubscribe(TaskId(1), &path("a"), SubMode::Exact);

    assert_eq!(collect(&trie, "a"), HashSet::from([TaskId(1)]));
    assert_eq!(collect(&trie, "a/b"), HashSet::from([TaskId(1)]));
  }

  #[test]
  fn remove_subscriber_purges_all() {
    let mut trie = SubTrie::new();
    trie.subscribe(TaskId(1), &path("a"), SubMode::Subtree);
    trie.subscribe(TaskId(1), &path("b/c"), SubMode::Exact);
    trie.subscribe(TaskId(2), &path("a"), SubMode::Subtree);

    trie.remove_subscriber(TaskId(1));

    assert_eq!(collect(&trie, "a"), HashSet::from([TaskId(2)]));
    assert!(collect(&trie, "b/c").is_empty());
  }

  #[test]
  fn ancestor_subtree_and_exact_both_match() {
    let mut trie = SubTrie::new();
    trie.subscribe(TaskId(1), &path("a"), SubMode::Subtree);
    trie.subscribe(TaskId(2), &path("a/b"), SubMode::Exact);
    trie.subscribe(TaskId(3), &path("a/b/c"), SubMode::Exact);

    assert_eq!(collect(&trie, "a/b"), HashSet::from([TaskId(1), TaskId(2)]));
  }
}
