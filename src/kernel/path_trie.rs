use std::collections::BTreeMap;
use std::fmt;

use super::task::TaskId;
use super::task_path::TaskPath;

#[derive(Debug)]
struct TrieNode {
  task: Option<TaskId>,
  children: BTreeMap<String, TrieNode>,
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
      children: BTreeMap::new(),
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

#[derive(Debug)]
pub struct PathConflictError {
  pub path: TaskPath,
  pub existing_id: TaskId,
}

impl fmt::Display for PathConflictError {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    write!(
      f,
      "path {} already occupied by task {:?}",
      self.path, self.existing_id
    )
  }
}

impl std::error::Error for PathConflictError {}

pub struct PathTrie {
  root: TrieNode,
}

impl PathTrie {
  pub fn new() -> Self {
    Self {
      root: TrieNode::new(),
    }
  }

  /// Insert a task at the given path. Errors if path is already occupied.
  pub fn insert(
    &mut self,
    path: &TaskPath,
    id: TaskId,
  ) -> Result<(), PathConflictError> {
    let mut node = &mut self.root;
    for component in path.components() {
      node = node
        .children
        .entry(component.to_string())
        .or_insert_with(TrieNode::new);
    }
    if let Some(existing) = node.task {
      return Err(PathConflictError {
        path: path.clone(),
        existing_id: existing,
      });
    }
    node.task = Some(id);
    Ok(())
  }

  /// Remove the task at the given path. Returns the TaskId if found.
  /// Prunes empty ancestor nodes.
  pub fn remove(&mut self, path: &TaskPath) -> Option<TaskId> {
    let components: Vec<&str> = path.components().collect();
    Self::remove_recursive(&mut self.root, &components)
  }

  fn remove_recursive(
    node: &mut TrieNode,
    components: &[&str],
  ) -> Option<TaskId> {
    if components.is_empty() {
      return node.task.take();
    }

    let component = components[0];
    let rest = &components[1..];

    let result = if let Some(child) = node.children.get_mut(component) {
      Self::remove_recursive(child, rest)
    } else {
      return None;
    };

    // Prune empty child
    if let Some(child) = node.children.get(component) {
      if child.is_empty() {
        node.children.remove(component);
      }
    }

    result
  }

  /// Resolve a path to its TaskId. O(depth).
  pub fn resolve(&self, path: &TaskPath) -> Option<TaskId> {
    let mut node = &self.root;
    for component in path.components() {
      node = node.children.get(component)?;
    }
    node.task
  }

  /// List direct children of a path node.
  /// Returns (component_name, Option<TaskId>) for each child.
  pub fn children(&self, path: &TaskPath) -> Vec<(String, Option<TaskId>)> {
    let node = self.walk_to(path);
    let Some(node) = node else {
      return Vec::new();
    };
    node
      .children
      .iter()
      .map(|(name, child)| (name.clone(), child.task))
      .collect()
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

  /// Iterate all (path, task_id) pairs in sorted order (DFS over BTreeMap).
  pub fn iter(&self) -> Vec<(TaskPath, TaskId)> {
    let mut result = Vec::new();
    self.root.collect_all("", &mut result);
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
    trie.insert(&path("web"), TaskId(1)).unwrap();
    trie.insert(&path("services/api"), TaskId(2)).unwrap();
    trie.insert(&path("services/worker"), TaskId(3)).unwrap();

    assert_eq!(trie.resolve(&path("web")), Some(TaskId(1)));
    assert_eq!(trie.resolve(&path("services/api")), Some(TaskId(2)));
    assert_eq!(trie.resolve(&path("services/worker")), Some(TaskId(3)));
    assert_eq!(trie.resolve(&path("missing")), None);
    assert_eq!(trie.resolve(&path("services")), None); // intermediate node
  }

  #[test]
  fn test_insert_conflict() {
    let mut trie = PathTrie::new();
    trie.insert(&path("web"), TaskId(1)).unwrap();
    let err = trie.insert(&path("web"), TaskId(2)).unwrap_err();
    assert_eq!(err.existing_id, TaskId(1));
  }

  #[test]
  fn test_remove() {
    let mut trie = PathTrie::new();
    trie.insert(&path("a/b"), TaskId(1)).unwrap();
    trie.insert(&path("a/c"), TaskId(2)).unwrap();

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
  fn test_children() {
    let mut trie = PathTrie::new();
    trie.insert(&path("services/api"), TaskId(1)).unwrap();
    trie.insert(&path("services/web"), TaskId(2)).unwrap();
    trie.insert(&path("services/web/v2"), TaskId(3)).unwrap();
    trie.insert(&path("tools/lint"), TaskId(4)).unwrap();

    let root_children = trie.children(&TaskPath::root());
    assert_eq!(root_children.len(), 2); // services, tools
    assert_eq!(root_children[0].0, "services");
    assert_eq!(root_children[0].1, None); // intermediate
    assert_eq!(root_children[1].0, "tools");
    assert_eq!(root_children[1].1, None);

    let svc_children = trie.children(&path("services"));
    assert_eq!(svc_children.len(), 2); // api, web
    assert_eq!(svc_children[0], ("api".to_string(), Some(TaskId(1))));
    assert_eq!(svc_children[1], ("web".to_string(), Some(TaskId(2))));
  }

  #[test]
  fn test_descendants() {
    let mut trie = PathTrie::new();
    trie.insert(&path("services/api"), TaskId(1)).unwrap();
    trie.insert(&path("services/web"), TaskId(2)).unwrap();
    trie.insert(&path("services/web/v2"), TaskId(3)).unwrap();
    trie.insert(&path("tools/lint"), TaskId(4)).unwrap();

    let desc = trie.descendants(&path("services"));
    assert_eq!(desc.len(), 3);
    assert_eq!(desc[0], (path("services/api"), TaskId(1)));
    assert_eq!(desc[1], (path("services/web"), TaskId(2)));
    assert_eq!(desc[2], (path("services/web/v2"), TaskId(3)));
  }

  #[test]
  fn test_glob_star() {
    let mut trie = PathTrie::new();
    trie.insert(&path("services/api"), TaskId(1)).unwrap();
    trie.insert(&path("services/web"), TaskId(2)).unwrap();
    trie.insert(&path("services/web/v2"), TaskId(3)).unwrap();
    trie.insert(&path("tools/lint"), TaskId(4)).unwrap();

    let results = trie.glob("services/*");
    assert_eq!(results.len(), 2);
    assert_eq!(results[0], (path("services/api"), TaskId(1)));
    assert_eq!(results[1], (path("services/web"), TaskId(2)));
  }

  #[test]
  fn test_glob_double_star() {
    let mut trie = PathTrie::new();
    trie.insert(&path("services/api"), TaskId(1)).unwrap();
    trie.insert(&path("services/web"), TaskId(2)).unwrap();
    trie.insert(&path("services/web/v2"), TaskId(3)).unwrap();
    trie.insert(&path("tools/lint"), TaskId(4)).unwrap();

    let results = trie.glob("services/**");
    assert_eq!(results.len(), 3);

    let results = trie.glob("**");
    assert_eq!(results.len(), 4);

    // Several `**` derivations can match the same path; no duplicates.
    let results = trie.glob("**/**");
    assert_eq!(results.len(), 4);
  }

  #[test]
  fn test_glob_mixed() {
    let mut trie = PathTrie::new();
    trie.insert(&path("a/b/c"), TaskId(1)).unwrap();
    trie.insert(&path("a/x/c"), TaskId(2)).unwrap();
    trie.insert(&path("a/b/d"), TaskId(3)).unwrap();

    let results = trie.glob("a/*/c");
    assert_eq!(results.len(), 2);
    assert_eq!(results[0], (path("a/b/c"), TaskId(1)));
    assert_eq!(results[1], (path("a/x/c"), TaskId(2)));
  }

  #[test]
  fn test_iter_sorted() {
    let mut trie = PathTrie::new();
    trie.insert(&path("z"), TaskId(1)).unwrap();
    trie.insert(&path("a/b"), TaskId(2)).unwrap();
    trie.insert(&path("a/a"), TaskId(3)).unwrap();
    trie.insert(&path("m"), TaskId(4)).unwrap();

    let items = trie.iter();
    let paths: Vec<&str> = items.iter().map(|(p, _)| p.as_str()).collect();
    assert_eq!(paths, vec!["a/a", "a/b", "m", "z"]);
  }
}
