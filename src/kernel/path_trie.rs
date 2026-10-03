use indexmap::IndexMap;

use super::task::TaskId;
use super::task_path::TaskPath;

#[derive(Debug)]
struct TrieNode {
  task: Option<TaskId>,
  /// In list order.
  children: IndexMap<String, TrieNode>,
}

fn join(prefix: &str, component: &str) -> String {
  if prefix.is_empty() {
    component.to_string()
  } else {
    format!("{}/{}", prefix, component)
  }
}

impl TrieNode {
  fn new() -> Self {
    Self {
      task: None,
      children: IndexMap::new(),
    }
  }

  fn is_empty(&self) -> bool {
    self.task.is_none() && self.children.is_empty()
  }

  /// Collect all (path, task_id) pairs under this node via DFS.
  fn collect_all(&self, prefix: &str, result: &mut Vec<(TaskPath, TaskId)>) {
    if let Some(id) = self.task {
      if let Ok(path) = TaskPath::new(prefix) {
        result.push((path, id));
      }
    }
    for (component, child) in &self.children {
      child.collect_all(&join(prefix, component), result);
    }
  }

  #[cfg(test)]
  fn collect_ids(&self, result: &mut Vec<TaskId>) {
    result.extend(self.task);
    for child in self.children.values() {
      child.collect_ids(result);
    }
  }

  /// A task at or under this node.
  fn any_task(&self, prefix: &str) -> Option<TaskPath> {
    if self.task.is_some() {
      return TaskPath::new(prefix).ok();
    }
    self
      .children
      .iter()
      .find_map(|(component, child)| child.any_task(&join(prefix, component)))
  }

  /// The last task listed at or under this node.
  fn last_task(&self) -> Option<TaskId> {
    self
      .task
      .or_else(|| self.children.values().rev().find_map(TrieNode::last_task))
  }

  /// Walk the trie matching glob pattern components.
  fn glob_walk(
    &self,
    prefix: &str,
    pattern: &[&str],
    result: &mut Vec<(TaskPath, TaskId)>,
  ) {
    if pattern.is_empty() {
      // Pattern exhausted: collect this node if it's a task
      if let Some(id) = self.task {
        if let Ok(path) = TaskPath::new(prefix) {
          result.push((path, id));
        }
      }
      return;
    }

    let pat = pattern[0];
    let rest = &pattern[1..];

    if pat == "**" {
      // Match zero components (skip **)
      self.glob_walk(prefix, rest, result);
      // Match one or more components
      for (component, child) in &self.children {
        // Continue with ** (match more) and with rest (done matching **)
        child.glob_walk(&join(prefix, component), pattern, result);
      }
    } else if pat == "*" {
      // Match exactly one component
      for (component, child) in &self.children {
        child.glob_walk(&join(prefix, component), rest, result);
      }
    } else {
      // Literal match
      if let Some(child) = self.children.get(pat) {
        child.glob_walk(&join(prefix, pat), rest, result);
      }
    }
  }
}

/// Why a task cannot go at a path.
#[derive(Debug, Eq, PartialEq)]
pub enum PathConflict {
  /// A task is at the path already.
  Taken(TaskId),
  /// A task is above or under the path. Tasks are always leaves.
  Nested(TaskPath),
}

/// Task paths, the names under each path in list order: the order they
/// were added. Tasks are leaves, and every non-task node has a task under
/// it, but for the places `place` holds while a batch is registered.
pub struct PathTrie {
  root: TrieNode,
}

impl PathTrie {
  pub fn new() -> Self {
    Self {
      root: TrieNode::new(),
    }
  }

  /// Refused, with no change, if a task is at, above, or under `path`.
  /// A new path is listed last among those beside it, or right after
  /// `after` when that is one of them.
  pub fn insert(
    &mut self,
    path: &TaskPath,
    id: TaskId,
    after: Option<&TaskPath>,
  ) -> Result<(), PathConflict> {
    if let Some(above) = std::iter::successors(path.parent(), TaskPath::parent)
      .find(|parent| self.resolve(parent).is_some())
    {
      return Err(PathConflict::Nested(above));
    }
    if let Some(node) = self.walk_to(path) {
      if let Some(existing) = node.task {
        return Err(PathConflict::Taken(existing));
      }
      if let Some(below) = node.any_task(path.as_str()) {
        return Err(PathConflict::Nested(below));
      }
    }
    let beside = after
      .filter(|after| after.parent() == path.parent())
      .map(TaskPath::name);
    let mut node = &mut self.root;
    let mut components = path.components().peekable();
    while let Some(component) = components.next() {
      let index = match node.children.get_index_of(component) {
        Some(index) => index,
        None => {
          let last = node.children.len();
          node.children.insert(component.to_string(), TrieNode::new());
          let place = match beside.and_then(|b| node.children.get_index_of(b)) {
            Some(index) if components.peek().is_none() => index + 1,
            Some(_) | None => last,
          };
          node.children.move_index(last, place);
          place
        }
      };
      node = &mut node.children[index];
    }
    node.task = Some(id);
    Ok(())
  }

  /// Holds the path's place in the lists until a task is inserted there
  /// or `prune` drops it. Nothing is held where an insert would be
  /// refused for a task above.
  pub fn place(&mut self, path: &TaskPath) {
    let mut node = &mut self.root;
    for component in path.components() {
      if node.task.is_some() {
        return;
      }
      node = node
        .children
        .entry(component.to_string())
        .or_insert_with(TrieNode::new);
    }
  }

  /// Remove the task at the given path. Returns the TaskId if found.
  /// Prunes empty ancestor nodes.
  pub fn remove(&mut self, path: &TaskPath) -> Option<TaskId> {
    let mut node = &mut self.root;
    for component in path.components() {
      node = node.children.get_mut(component)?;
    }
    let id = node.task.take();
    self.prune(path);
    id
  }

  /// Drops the nodes along `path` that have no task at or under them.
  pub fn prune(&mut self, path: &TaskPath) {
    fn prune<'a>(
      node: &mut TrieNode,
      mut components: impl Iterator<Item = &'a str>,
    ) {
      let Some(component) = components.next() else {
        return;
      };
      let Some(child) = node.children.get_mut(component) else {
        return;
      };
      prune(child, components);
      if child.is_empty() {
        node.children.shift_remove(component);
      }
    }
    prune(&mut self.root, path.components());
  }

  /// Resolve a path to its TaskId. O(depth).
  pub fn resolve(&self, path: &TaskPath) -> Option<TaskId> {
    let mut node = &self.root;
    for component in path.components() {
      node = node.children.get(component)?;
    }
    node.task
  }

  /// The task listed just before `path`.
  pub fn before(&self, path: &TaskPath) -> Option<TaskId> {
    let mut node = &self.root;
    let mut before = None;
    for component in path.components() {
      let index = node.children.get_index_of(component)?;
      let earlier = node.children.get_range(..index)?;
      // A task under a later path part is listed after one beside an
      // earlier part.
      before = earlier
        .values()
        .rev()
        .find_map(TrieNode::last_task)
        .or(before);
      node = &node.children[index];
    }
    before
  }

  /// Where a path is listed: its index among the names beside it, at each
  /// part of the path. Paths in list order have these in order.
  pub fn position(&self, path: &TaskPath) -> Option<Vec<usize>> {
    let mut node = &self.root;
    let mut position = Vec::new();
    for component in path.components() {
      let (index, _, child) = node.children.get_full(component)?;
      position.push(index);
      node = child;
    }
    Some(position)
  }

  /// Every task, in list order.
  #[cfg(test)]
  pub fn tasks(&self) -> Vec<TaskId> {
    let mut result = Vec::new();
    self.root.collect_ids(&mut result);
    result
  }

  /// Recursively collect all tasks under a prefix path.
  pub fn descendants(&self, path: &TaskPath) -> Vec<(TaskPath, TaskId)> {
    let node = self.walk_to(path);
    let Some(node) = node else {
      return Vec::new();
    };
    let mut result = Vec::new();
    // Collect from children, not the node itself
    for (component, child) in &node.children {
      child.collect_all(&join(path.as_str(), component), &mut result);
    }
    result
  }

  /// Find all tasks whose paths match a glob pattern, sorted by path.
  /// Supports `*` (single component) and `**` (recursive).
  pub fn glob(&self, pattern: &str) -> Vec<(TaskPath, TaskId)> {
    let parts: Vec<&str> =
      pattern.split('/').filter(|c| !c.is_empty()).collect();
    let mut result = Vec::new();
    self.root.glob_walk("", &parts, &mut result);
    // A path can match through several `**` derivations.
    result.sort();
    result.dedup();
    result
  }

  fn walk_to(&self, path: &TaskPath) -> Option<&TrieNode> {
    let mut node = &self.root;
    for component in path.components() {
      node = node.children.get(component)?;
    }
    Some(node)
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  fn path(s: &str) -> TaskPath {
    TaskPath::new(s).unwrap()
  }

  #[test]
  fn test_insert_and_resolve() {
    let mut trie = PathTrie::new();
    trie.insert(&path("web"), TaskId(1), None).unwrap();
    trie.insert(&path("services/api"), TaskId(2), None).unwrap();
    trie
      .insert(&path("services/worker"), TaskId(3), None)
      .unwrap();

    assert_eq!(trie.resolve(&path("web")), Some(TaskId(1)));
    assert_eq!(trie.resolve(&path("services/api")), Some(TaskId(2)));
    assert_eq!(trie.resolve(&path("services/worker")), Some(TaskId(3)));
    assert_eq!(trie.resolve(&path("missing")), None);
    assert_eq!(trie.resolve(&path("services")), None); // intermediate node
  }

  #[test]
  fn test_insert_conflict() {
    let mut trie = PathTrie::new();
    trie.insert(&path("web"), TaskId(1), None).unwrap();
    let err = trie.insert(&path("web"), TaskId(2), None).unwrap_err();
    assert_eq!(err, PathConflict::Taken(TaskId(1)));
  }

  #[test]
  fn refuses_a_task_above_or_under_another() {
    let mut trie = PathTrie::new();
    trie.insert(&path("web"), TaskId(1), None).unwrap();
    trie.insert(&path("api/v1/http"), TaskId(2), None).unwrap();

    assert_eq!(
      trie.insert(&path("web/dev/a"), TaskId(3), None),
      Err(PathConflict::Nested(path("web")))
    );
    assert_eq!(
      trie.insert(&path("api"), TaskId(3), None),
      Err(PathConflict::Nested(path("api/v1/http")))
    );
    assert_eq!(
      trie.insert(&path("api/v1"), TaskId(3), None),
      Err(PathConflict::Nested(path("api/v1/http")))
    );
    assert!(
      trie.descendants(&path("web")).is_empty(),
      "nothing was made"
    );

    trie.remove(&path("web"));
    trie.insert(&path("web/dev/a"), TaskId(3), None).unwrap();
    trie.insert(&path("api/v2"), TaskId(4), None).unwrap();
  }

  #[test]
  fn test_remove() {
    let mut trie = PathTrie::new();
    trie.insert(&path("a/b"), TaskId(1), None).unwrap();
    trie.insert(&path("a/c"), TaskId(2), None).unwrap();

    assert_eq!(trie.remove(&path("a/b")), Some(TaskId(1)));
    assert_eq!(trie.resolve(&path("a/b")), None);
    // /a/c still exists
    assert_eq!(trie.resolve(&path("a/c")), Some(TaskId(2)));

    // Remove remaining child; /a should be pruned
    assert_eq!(trie.remove(&path("a/c")), Some(TaskId(2)));
    assert_eq!(trie.resolve(&path("a/c")), None);
  }

  #[test]
  fn test_remove_nonexistent() {
    let mut trie = PathTrie::new();
    assert_eq!(trie.remove(&path("nope")), None);
  }

  #[test]
  fn test_descendants() {
    let mut trie = PathTrie::new();
    trie.insert(&path("services/api"), TaskId(1), None).unwrap();
    trie
      .insert(&path("services/web/v1"), TaskId(2), None)
      .unwrap();
    trie
      .insert(&path("services/web/v2"), TaskId(3), None)
      .unwrap();
    trie.insert(&path("tools/lint"), TaskId(4), None).unwrap();

    let desc = trie.descendants(&path("services"));
    assert_eq!(desc.len(), 3);
    assert_eq!(desc[0], (path("services/api"), TaskId(1)));
    assert_eq!(desc[1], (path("services/web/v1"), TaskId(2)));
    assert_eq!(desc[2], (path("services/web/v2"), TaskId(3)));
  }

  #[test]
  fn test_glob_star() {
    let mut trie = PathTrie::new();
    trie.insert(&path("services/api"), TaskId(1), None).unwrap();
    trie
      .insert(&path("services/web/v1"), TaskId(2), None)
      .unwrap();
    trie
      .insert(&path("services/web/v2"), TaskId(3), None)
      .unwrap();
    trie.insert(&path("tools/lint"), TaskId(4), None).unwrap();

    let results = trie.glob("services/*");
    assert_eq!(results, [(path("services/api"), TaskId(1))]);

    let results = trie.glob("services/web/*");
    assert_eq!(results.len(), 2);
    assert_eq!(results[0], (path("services/web/v1"), TaskId(2)));
    assert_eq!(results[1], (path("services/web/v2"), TaskId(3)));
  }

  #[test]
  fn test_glob_double_star() {
    let mut trie = PathTrie::new();
    trie.insert(&path("services/api"), TaskId(1), None).unwrap();
    trie
      .insert(&path("services/web/v1"), TaskId(2), None)
      .unwrap();
    trie
      .insert(&path("services/web/v2"), TaskId(3), None)
      .unwrap();
    trie.insert(&path("tools/lint"), TaskId(4), None).unwrap();

    let results = trie.glob("services/**");
    assert_eq!(results.len(), 3);

    // A trailing `**` matches the path itself too.
    let results = trie.glob("services/api/**");
    assert_eq!(results, [(path("services/api"), TaskId(1))]);

    let results = trie.glob("**");
    assert_eq!(results.len(), 4);

    // Several `**` derivations can match the same path; no duplicates.
    let results = trie.glob("**/**");
    assert_eq!(results.len(), 4);
  }

  #[test]
  fn test_glob_mixed() {
    let mut trie = PathTrie::new();
    trie.insert(&path("a/b/c"), TaskId(1), None).unwrap();
    trie.insert(&path("a/x/c"), TaskId(2), None).unwrap();
    trie.insert(&path("a/b/d"), TaskId(3), None).unwrap();

    let results = trie.glob("a/*/c");
    assert_eq!(results.len(), 2);
    assert_eq!(results[0], (path("a/b/c"), TaskId(1)));
    assert_eq!(results[1], (path("a/x/c"), TaskId(2)));
  }

  fn listed(trie: &PathTrie) -> Vec<usize> {
    trie.tasks().iter().map(|id| id.0).collect()
  }

  #[test]
  fn lists_paths_as_added_with_a_group_at_its_first_task() {
    let mut trie = PathTrie::new();
    for (i, p) in ["web/dev", "db", "web/ui", "api/a/b"].iter().enumerate() {
      trie.insert(&path(p), TaskId(i + 1), None).unwrap();
    }
    assert_eq!(listed(&trie), [1, 3, 2, 4]);
    assert_eq!(trie.before(&path("web/dev")), None);
    assert_eq!(trie.before(&path("web/ui")), Some(TaskId(1)));
    assert_eq!(trie.before(&path("db")), Some(TaskId(3)));
    assert_eq!(trie.before(&path("api/a/b")), Some(TaskId(2)));

    trie.remove(&path("web/dev"));
    assert_eq!(listed(&trie), [3, 2, 4], "the others keep their order");
    trie.remove(&path("web/ui"));
    trie.insert(&path("web/dev"), TaskId(5), None).unwrap();
    assert_eq!(listed(&trie), [2, 4, 5], "an emptied group is gone");
  }

  #[test]
  fn lists_a_path_right_after_the_one_it_names() {
    let mut trie = PathTrie::new();
    trie.insert(&path("db"), TaskId(1), None).unwrap();
    trie.insert(&path("web/api"), TaskId(2), None).unwrap();
    trie.insert(&path("web/ui"), TaskId(3), None).unwrap();
    trie
      .insert(&path("web/api-2"), TaskId(4), Some(&path("web/api")))
      .unwrap();
    trie
      .insert(&path("db-2"), TaskId(5), Some(&path("db")))
      .unwrap();
    assert_eq!(listed(&trie), [1, 5, 2, 4, 3]);

    // Not beside the one it names: listed last.
    trie
      .insert(&path("cache"), TaskId(6), Some(&path("web/api")))
      .unwrap();
    assert_eq!(listed(&trie), [1, 5, 2, 4, 3, 6]);
  }

  #[test]
  fn a_held_place_is_filled_or_dropped() {
    let mut trie = PathTrie::new();
    trie.insert(&path("a"), TaskId(1), None).unwrap();
    for p in ["web/api", "db", "web/ui", "a/under"] {
      trie.place(&path(p));
    }
    assert_eq!(listed(&trie), [1]);
    assert_eq!(trie.insert(&path("web"), TaskId(9), None), Ok(()));
    trie.remove(&path("web"));

    trie.insert(&path("db"), TaskId(2), None).unwrap();
    trie.insert(&path("web/ui"), TaskId(3), None).unwrap();
    assert_eq!(listed(&trie), [1, 3, 2]);
    assert_eq!(trie.before(&path("web/ui")), Some(TaskId(1)));
    assert_eq!(trie.before(&path("db")), Some(TaskId(3)));

    trie.prune(&path("web/api"));
    trie.prune(&path("a/under"));
    assert_eq!(listed(&trie), [1, 3, 2]);
    assert_eq!(trie.root.children.len(), 3);
    assert_eq!(trie.root.children["web"].children.len(), 1);
    assert!(trie.root.children["a"].children.is_empty());
  }
}
