use std::{cmp::Ordering, fmt};

use serde::{Deserialize, Serialize};

/// Ordered component by component, so a path comes right before
/// everything under it: `a`, `a/b`, `a-b`.
#[derive(Clone, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
pub struct TaskPath(String);

impl Ord for TaskPath {
  fn cmp(&self, other: &Self) -> Ordering {
    self.components().cmp(other.components())
  }
}

impl PartialOrd for TaskPath {
  fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
    Some(self.cmp(other))
  }
}

#[derive(Debug)]
pub struct InvalidPath(pub String);

impl fmt::Display for InvalidPath {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    write!(f, "invalid task path: {}", self.0)
  }
}

impl std::error::Error for InvalidPath {}

pub fn is_valid_component_char(c: char) -> bool {
  c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.'
}

/// A path component made from free text, such as an mprocs name or a
/// shell command: every other character becomes `_`, as in log file
/// names, so a name and its path give the same log file.
pub fn path_name(text: &str) -> String {
  let name: String = text
    .chars()
    .map(|c| if is_valid_component_char(c) { c } else { '_' })
    .collect();
  if name.is_empty() {
    "task".to_string()
  } else {
    name
  }
}

/// `base`, or `base-2`, `base-3`, ... — the first that is not `taken`.
pub fn unique(base: &str, taken: impl Fn(&str) -> bool) -> String {
  if !taken(base) {
    return base.to_string();
  }
  (2..)
    .map(|n| format!("{}-{}", base, n))
    .find(|name| !taken(name))
    .unwrap()
}

impl TaskPath {
  /// Parse a path of the form `a/b/c`. The root path is not parseable, so
  /// a task can never sit at the root.
  pub fn new(s: impl Into<String>) -> Result<Self, InvalidPath> {
    let s = s.into();
    if s.is_empty() {
      return Err(InvalidPath("path is empty".to_string()));
    }
    if s.starts_with('/') {
      return Err(InvalidPath("path must not start with '/'".to_string()));
    }
    for component in s.split('/') {
      if component.is_empty() {
        return Err(InvalidPath("path contains empty component".to_string()));
      }
      for c in component.chars() {
        if !is_valid_component_char(c) {
          return Err(InvalidPath(format!(
            "path contains invalid character: '{}'",
            c
          )));
        }
      }
    }
    Ok(TaskPath(s))
  }

  /// Validate a glob pattern: path components plus `*` and `**` wildcard
  /// components.
  pub fn check_glob(pattern: &str) -> Result<(), InvalidPath> {
    if pattern.is_empty() {
      return Err(InvalidPath("pattern is empty".to_string()));
    }
    if pattern.starts_with('/') {
      return Err(InvalidPath("pattern must not start with '/'".to_string()));
    }
    for component in pattern.split('/') {
      if component == "*" || component == "**" {
        continue;
      }
      if component.is_empty() {
        return Err(InvalidPath(
          "pattern contains empty component".to_string(),
        ));
      }
      for c in component.chars() {
        if !is_valid_component_char(c) {
          return Err(InvalidPath(format!(
            "pattern contains invalid character: '{}'",
            c
          )));
        }
      }
    }
    Ok(())
  }

  /// The root of the path tree. Never holds a task; used to address the
  /// whole tree (e.g. subscribing to everything).
  pub fn root() -> Self {
    TaskPath(String::new())
  }

  pub fn is_root(&self) -> bool {
    self.0.is_empty()
  }

  pub fn as_str(&self) -> &str {
    &self.0
  }

  /// Returns the parent path, or None for the root.
  pub fn parent(&self) -> Option<TaskPath> {
    if self.0.is_empty() {
      return None;
    }
    match self.0.rfind('/') {
      Some(pos) => Some(TaskPath(self.0[..pos].to_string())),
      None => Some(TaskPath::root()),
    }
  }

  /// Returns the last component (the "name"), or empty string for the root.
  pub fn name(&self) -> &str {
    match self.0.rfind('/') {
      Some(pos) => &self.0[pos + 1..],
      None => &self.0,
    }
  }

  /// Returns an iterator over path components. The root yields an empty
  /// iterator.
  pub fn components(&self) -> impl Iterator<Item = &str> {
    self.0.split('/').filter(|c| !c.is_empty())
  }

  /// Number of components. `a` = 1, `a/b` = 2, root = 0.
  pub fn depth(&self) -> usize {
    self.components().count()
  }
}

impl fmt::Display for TaskPath {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.write_str(&self.0)
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn test_valid_paths() {
    assert!(TaskPath::new("web").is_ok());
    assert!(TaskPath::new("services/api").is_ok());
    assert!(TaskPath::new("tools/my-watcher").is_ok());
    assert!(TaskPath::new("a/b.c/d_e").is_ok());
  }

  #[test]
  fn test_invalid_paths() {
    assert!(TaskPath::new("").is_err());
    assert!(TaskPath::new("/web").is_err());
    assert!(TaskPath::new("web/").is_err());
    assert!(TaskPath::new("web//api").is_err());
    assert!(TaskPath::new("web server").is_err());
    assert!(TaskPath::new("web@home").is_err());
  }

  #[test]
  fn test_check_glob() {
    assert!(TaskPath::check_glob("web").is_ok());
    assert!(TaskPath::check_glob("services/*").is_ok());
    assert!(TaskPath::check_glob("**").is_ok());
    assert!(TaskPath::check_glob("a/**/c").is_ok());

    assert!(TaskPath::check_glob("").is_err());
    assert!(TaskPath::check_glob("/web").is_err());
    assert!(TaskPath::check_glob("web/").is_err());
    assert!(TaskPath::check_glob("web*").is_err());
    assert!(TaskPath::check_glob("a//b").is_err());
  }

  #[test]
  fn test_root() {
    let root = TaskPath::root();
    assert!(root.is_root());
    assert_eq!(root.as_str(), "");
    assert_eq!(root.parent(), None);
    assert_eq!(root.name(), "");
    assert_eq!(root.depth(), 0);
    assert!(!TaskPath::new("web").unwrap().is_root());
  }

  #[test]
  fn test_parent() {
    assert_eq!(
      TaskPath::new("web").unwrap().parent(),
      Some(TaskPath::root())
    );
    assert_eq!(
      TaskPath::new("services/api")
        .unwrap()
        .parent()
        .unwrap()
        .as_str(),
      "services"
    );
  }

  #[test]
  fn test_name() {
    assert_eq!(TaskPath::new("web").unwrap().name(), "web");
    assert_eq!(TaskPath::new("services/api").unwrap().name(), "api");
  }

  #[test]
  fn test_components() {
    let p = TaskPath::root();
    assert_eq!(p.components().collect::<Vec<_>>(), Vec::<&str>::new());

    let p = TaskPath::new("web").unwrap();
    assert_eq!(p.components().collect::<Vec<_>>(), vec!["web"]);

    let p = TaskPath::new("services/api").unwrap();
    assert_eq!(p.components().collect::<Vec<_>>(), vec!["services", "api"]);
  }

  #[test]
  fn sorts_a_path_before_everything_under_it() {
    let mut paths: Vec<TaskPath> = ["b", "a-b", "a/c", "a", "a/b/c", "a.b"]
      .into_iter()
      .map(|p| TaskPath::new(p).unwrap())
      .collect();
    paths.sort();
    let paths: Vec<&str> = paths.iter().map(|p| p.as_str()).collect();
    assert_eq!(paths, ["a", "a/b/c", "a/c", "a-b", "a.b", "b"]);
  }

  #[test]
  fn test_depth() {
    assert_eq!(TaskPath::new("web").unwrap().depth(), 1);
    assert_eq!(TaskPath::new("a/b/c").unwrap().depth(), 3);
  }
}
