//! Config parsing: typed accessors over a YAML value that report where
//! a value is wrong.
//!
//! - [`CfgDoc`]: a parsed document
//! - [`CfgNode`] / [`CfgObj`] / [`CfgArr`]: typed accessors with the path
//!   to each value, for errors
//! - [`FromCfg`]: how a Rust type reads from a value
//! - [`CfgCx`]: the context a value is read in (the config's directory)

use std::ffi::OsString;
use std::fmt::{self, Display};
use std::path::{Component, Path, PathBuf};

use anyhow::{Result, bail};
use indexmap::IndexMap;
use serde_yaml::Value;

#[derive(Clone, Debug, Default)]
pub struct CfgPath(Vec<String>);

impl CfgPath {
  pub fn root() -> Self {
    Self(Vec::new())
  }

  /// Append a segment, returning a new child path.
  pub fn join(&self, segment: impl ToString) -> Self {
    let mut segs = self.0.clone();
    segs.push(segment.to_string());
    Self(segs)
  }
}

impl Display for CfgPath {
  fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
    write!(f, "<config>")?;
    for seg in &self.0 {
      write!(f, ".{}", seg)?;
    }
    Ok(())
  }
}

/// Context provided during config loading.
pub struct CfgCx {
  /// Parent directory of the config file, which relative paths are from.
  pub config_dir: PathBuf,
}

impl CfgCx {
  pub fn new(config_dir: PathBuf) -> Self {
    Self { config_dir }
  }

  /// The path `node` holds, relative to the config's directory.
  pub fn path(&self, node: &CfgNode<'_>) -> Result<PathBuf> {
    let path = node.as_str()?;
    if path.starts_with("<CONFIG_DIR>") {
      bail!(node.error(
        "'<CONFIG_DIR>' is not supported: a relative path is from the \
         file's directory already"
      ));
    }
    Ok(self.resolve_path(path))
  }

  /// A path from the config, relative to its directory. Without `.` and
  /// `..` steps, taken as written, so a path compares equal however it is
  /// written: `.` is the directory itself, and `../api` from `web` is
  /// `api`.
  fn resolve_path(&self, path: &str) -> PathBuf {
    let path = self.config_dir.join(path);
    let mut resolved = PathBuf::new();
    for component in path.components() {
      match (component, resolved.components().next_back()) {
        (Component::CurDir, _) => (),
        (Component::ParentDir, Some(Component::Normal(_))) => {
          resolved.pop();
        }
        // Above the root is the root.
        (
          Component::ParentDir,
          Some(Component::RootDir | Component::Prefix(_)),
        ) => (),
        (component, _) => resolved.push(component),
      }
    }
    resolved
  }
}

/// A parsed config document.
pub struct CfgDoc {
  resolved: Value,
}

impl CfgDoc {
  pub fn from_value(source: Value) -> Self {
    Self { resolved: source }
  }

  /// Load a YAML config file.
  pub fn load(path: &Path) -> Result<Self> {
    let content = std::fs::read_to_string(path)?;
    Ok(Self::from_value(serde_yaml::from_str(&content)?))
  }

  /// The tree, to change before it is parsed, as `!js` values are.
  pub fn value_mut(&mut self) -> &mut Value {
    &mut self.resolved
  }

  /// Root node of the resolved tree, ready for parsing.
  pub fn root(&self) -> CfgNode<'_> {
    CfgNode::new(&self.resolved, CfgPath::root())
  }
}

/// Reference to a resolved config value with path tracking for error messages.
#[derive(Clone)]
pub struct CfgNode<'a> {
  value: &'a Value,
  path: CfgPath,
}

impl<'a> CfgNode<'a> {
  pub fn new(value: &'a Value, path: CfgPath) -> Self {
    Self { value, path }
  }

  pub fn path(&self) -> &CfgPath {
    &self.path
  }

  pub fn raw(&self) -> &'a Value {
    self.value
  }

  /// Create an error anchored at this node's position.
  pub fn error(&self, msg: impl Display) -> anyhow::Error {
    anyhow::anyhow!("{} at {}", msg, self.path)
  }

  // Type checks

  pub fn is_null(&self) -> bool {
    self.value.is_null()
  }
  pub fn is_string(&self) -> bool {
    self.value.is_string()
  }
  pub fn is_mapping(&self) -> bool {
    self.value.is_mapping()
  }
  pub fn is_sequence(&self) -> bool {
    self.value.is_sequence()
  }

  // Primitive access

  pub fn as_str(&self) -> Result<&'a str> {
    self
      .value
      .as_str()
      .ok_or_else(|| self.error("expected string"))
  }

  pub fn as_bool(&self) -> Result<bool> {
    self
      .value
      .as_bool()
      .ok_or_else(|| self.error("expected bool"))
  }

  pub fn as_u64(&self) -> Result<u64> {
    self
      .value
      .as_u64()
      .ok_or_else(|| self.error("expected unsigned integer"))
  }

  pub fn as_i64(&self) -> Result<i64> {
    self
      .value
      .as_i64()
      .ok_or_else(|| self.error("expected integer"))
  }

  pub fn as_f64(&self) -> Result<f64> {
    self
      .value
      .as_f64()
      .ok_or_else(|| self.error("expected number"))
  }

  pub fn as_usize(&self) -> Result<usize> {
    self.as_u64().map(|v| v as usize)
  }

  // Composite access

  pub fn as_obj(&self) -> Result<CfgObj<'a>> {
    let map = self
      .value
      .as_mapping()
      .ok_or_else(|| self.error("expected object"))?;
    Ok(CfgObj {
      map,
      path: self.path.clone(),
    })
  }

  pub fn as_arr(&self) -> Result<CfgArr<'a>> {
    let seq = self
      .value
      .as_sequence()
      .ok_or_else(|| self.error("expected array"))?;
    Ok(CfgArr {
      seq: seq.as_slice(),
      path: self.path.clone(),
    })
  }

  /// Parse this node into a Rust type via [`FromCfg`].
  pub fn parse<T: FromCfg>(&self, cx: &CfgCx) -> Result<T> {
    T::from_cfg(self, cx)
  }
}

pub struct CfgObj<'a> {
  map: &'a serde_yaml::Mapping,
  path: CfgPath,
}

impl<'a> CfgObj<'a> {
  /// Look up a key, returning a [`CfgNode`] if present.
  pub fn get(&self, key: &str) -> Option<CfgNode<'a>> {
    self.map.get(&Value::from(key)).map(|v| CfgNode {
      value: v,
      path: self.path.join(key),
    })
  }

  /// Parse a required field. Errors if the key is missing.
  pub fn required<T: FromCfg>(&self, key: &str, cx: &CfgCx) -> Result<T> {
    match self.get(key) {
      Some(node) => T::from_cfg(&node, cx),
      None => bail!("missing required field '{}' at {}", key, self.path),
    }
  }

  /// Parse an optional field. Returns `None` if missing or null.
  pub fn optional<T: FromCfg>(
    &self,
    key: &str,
    cx: &CfgCx,
  ) -> Result<Option<T>> {
    match self.get(key) {
      Some(node) if !node.is_null() => Ok(Some(T::from_cfg(&node, cx)?)),
      _ => Ok(None),
    }
  }

  /// Parse a field with a fallback. Returns `default` if missing or null.
  pub fn default<T: FromCfg>(
    &self,
    key: &str,
    default: T,
    cx: &CfgCx,
  ) -> Result<T> {
    match self.get(key) {
      Some(node) if !node.is_null() => T::from_cfg(&node, cx),
      _ => Ok(default),
    }
  }

  /// Iterate over string-keyed entries. Non-string keys are silently skipped.
  pub fn iter(&self) -> impl Iterator<Item = (&'a str, CfgNode<'a>)> + '_ {
    self.map.iter().filter_map(move |(k, v)| {
      let key = k.as_str()?;
      Some((
        key,
        CfgNode {
          value: v,
          path: self.path.join(key),
        },
      ))
    })
  }

  pub fn path(&self) -> &CfgPath {
    &self.path
  }

  pub fn error(&self, msg: impl Display) -> anyhow::Error {
    anyhow::anyhow!("{} at {}", msg, self.path)
  }

  /// Error if this object contains any key not listed in `known`.
  ///
  /// Call this for fixed-schema sections so typos like `auto_restart` fail
  /// instead of being silently ignored. Free-form maps (task names, env vars,
  /// key bindings) should not use this.
  pub fn known_keys(&self, known: &[&str]) -> Result<()> {
    let mut unknown: Vec<&str> = Vec::new();
    for key in self.map.keys() {
      match key.as_str() {
        Some(key) if !known.iter().any(|k| *k == key) => unknown.push(key),
        None => {
          bail!("non-string key at {}", self.path);
        }
        _ => {}
      }
    }
    if unknown.is_empty() {
      return Ok(());
    }
    // Stable order so error messages are deterministic in tests.
    unknown.sort_unstable();

    let mut msg = if unknown.len() == 1 {
      format!("unknown field '{}' at {}", unknown[0], self.path)
    } else {
      format!(
        "unknown fields {} at {}",
        unknown
          .iter()
          .map(|k| format!("'{k}'"))
          .collect::<Vec<_>>()
          .join(", "),
        self.path
      )
    };

    if let Some(suggestion) = suggest_key(unknown[0], known) {
      msg.push_str(&format!("; did you mean '{suggestion}'?"));
    }

    bail!(msg)
  }
}

/// Best-effort typo hint: exact match after stripping `_`/`-`, else small edit distance.
fn suggest_key<'a>(unknown: &str, known: &[&'a str]) -> Option<&'a str> {
  fn normalize(s: &str) -> String {
    s.chars()
      .filter(|c| *c != '_' && *c != '-')
      .flat_map(|c| c.to_lowercase())
      .collect()
  }

  let u_norm = normalize(unknown);
  let mut best: Option<(&str, usize)> = None;
  for k in known {
    let k_norm = normalize(k);
    if k_norm == u_norm {
      return Some(*k);
    }
    let dist = edit_distance(&u_norm, &k_norm);
    let max_dist = if u_norm.len() <= 4 { 1 } else { 2 };
    if dist <= max_dist {
      match best {
        Some((_, d)) if d <= dist => {}
        _ => best = Some((*k, dist)),
      }
    }
  }
  best.map(|(k, _)| k)
}

fn edit_distance(a: &str, b: &str) -> usize {
  let a: Vec<char> = a.chars().collect();
  let b: Vec<char> = b.chars().collect();
  let mut prev: Vec<usize> = (0..=b.len()).collect();
  let mut cur = vec![0; b.len() + 1];
  for (i, ca) in a.iter().enumerate() {
    cur[0] = i + 1;
    for (j, cb) in b.iter().enumerate() {
      let cost = if ca == cb { 0 } else { 1 };
      cur[j + 1] = (prev[j + 1] + 1).min(cur[j] + 1).min(prev[j] + cost);
    }
    std::mem::swap(&mut prev, &mut cur);
  }
  prev[b.len()]
}

pub struct CfgArr<'a> {
  seq: &'a [Value],
  path: CfgPath,
}

impl<'a> CfgArr<'a> {
  pub fn len(&self) -> usize {
    self.seq.len()
  }

  pub fn is_empty(&self) -> bool {
    self.seq.is_empty()
  }

  pub fn get(&self, index: usize) -> Option<CfgNode<'a>> {
    self.seq.get(index).map(|v| CfgNode {
      value: v,
      path: self.path.join(index),
    })
  }

  pub fn iter(&self) -> impl Iterator<Item = CfgNode<'a>> + '_ {
    self.seq.iter().enumerate().map(move |(i, v)| CfgNode {
      value: v,
      path: self.path.join(i),
    })
  }

  /// Parse all elements as `T`.
  pub fn collect<T: FromCfg>(&self, cx: &CfgCx) -> Result<Vec<T>> {
    self.iter().map(|node| T::from_cfg(&node, cx)).collect()
  }

  pub fn path(&self) -> &CfgPath {
    &self.path
  }
}

//
// FromCfg
//

pub trait FromCfg: Sized {
  fn from_cfg(node: &CfgNode<'_>, cx: &CfgCx) -> Result<Self>;
}

// Primitive impls

impl FromCfg for String {
  fn from_cfg(node: &CfgNode<'_>, _cx: &CfgCx) -> Result<Self> {
    Ok(node.as_str()?.to_owned())
  }
}

impl FromCfg for bool {
  fn from_cfg(node: &CfgNode<'_>, _cx: &CfgCx) -> Result<Self> {
    node.as_bool()
  }
}

impl FromCfg for usize {
  fn from_cfg(node: &CfgNode<'_>, _cx: &CfgCx) -> Result<Self> {
    node.as_usize()
  }
}

impl FromCfg for u64 {
  fn from_cfg(node: &CfgNode<'_>, _cx: &CfgCx) -> Result<Self> {
    node.as_u64()
  }
}

impl FromCfg for i64 {
  fn from_cfg(node: &CfgNode<'_>, _cx: &CfgCx) -> Result<Self> {
    node.as_i64()
  }
}

impl FromCfg for f64 {
  fn from_cfg(node: &CfgNode<'_>, _cx: &CfgCx) -> Result<Self> {
    node.as_f64()
  }
}

impl FromCfg for PathBuf {
  fn from_cfg(node: &CfgNode<'_>, cx: &CfgCx) -> Result<Self> {
    cx.path(node)
  }
}

impl FromCfg for OsString {
  fn from_cfg(node: &CfgNode<'_>, _cx: &CfgCx) -> Result<Self> {
    Ok(OsString::from(node.as_str()?))
  }
}

impl FromCfg for Value {
  fn from_cfg(node: &CfgNode<'_>, _cx: &CfgCx) -> Result<Self> {
    Ok(node.raw().clone())
  }
}

// Composite impls

impl<T: FromCfg> FromCfg for Vec<T> {
  fn from_cfg(node: &CfgNode<'_>, cx: &CfgCx) -> Result<Self> {
    node.as_arr()?.collect(cx)
  }
}

impl<T: FromCfg> FromCfg for Option<T> {
  fn from_cfg(node: &CfgNode<'_>, cx: &CfgCx) -> Result<Self> {
    if node.is_null() {
      Ok(None)
    } else {
      Ok(Some(T::from_cfg(node, cx)?))
    }
  }
}

impl<T: FromCfg> FromCfg for IndexMap<String, T> {
  fn from_cfg(node: &CfgNode<'_>, cx: &CfgCx) -> Result<Self> {
    let obj = node.as_obj()?;
    obj
      .iter()
      .map(|(k, v)| Ok((k.to_owned(), T::from_cfg(&v, cx)?)))
      .collect()
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn a_resolved_path_has_no_dot_steps() {
    let cx = CfgCx::new(PathBuf::from("/p"));
    assert_eq!(cx.resolve_path("."), Path::new("/p"));
    assert_eq!(cx.resolve_path("./a/./b"), Path::new("/p/a/b"));
    assert_eq!(cx.resolve_path("/q/."), Path::new("/q"));
    assert_eq!(cx.resolve_path("../q/../r"), Path::new("/r"));
    assert_eq!(cx.resolve_path("/../.."), Path::new("/"));
    let relative = CfgCx::new(PathBuf::from("p"));
    assert_eq!(relative.resolve_path("../../q"), Path::new("../q"));
  }

  fn obj_from_yaml(yaml: &str) -> CfgObj<'static> {
    // Leak for 'static test lifetime convenience.
    let value =
      Box::leak(Box::new(serde_yaml::from_str::<Value>(yaml).expect("yaml")));
    CfgNode::new(value, CfgPath::root())
      .as_obj()
      .expect("object")
  }

  #[test]
  fn known_keys_accepts_only_listed_fields() {
    let obj = obj_from_yaml(
      r#"
      autorestart: true
      cwd: /tmp
      "#,
    );
    obj
      .known_keys(&["autorestart", "cwd", "env"])
      .expect("known keys ok");
  }

  #[test]
  fn known_keys_rejects_typo_with_suggestion() {
    let obj = obj_from_yaml("auto_restart: true\n");
    let err = obj
      .known_keys(&["autorestart", "autostart", "cwd"])
      .unwrap_err()
      .to_string();
    assert!(err.contains("unknown field 'auto_restart'"), "err={err}");
    assert!(err.contains("did you mean 'autorestart'?"), "err={err}");
  }

  #[test]
  fn known_keys_lists_multiple_unknown_fields() {
    let obj = obj_from_yaml(
      r#"
      foo: 1
      bar: 2
      ok: true
      "#,
    );
    let err = obj.known_keys(&["ok"]).unwrap_err().to_string();
    assert!(err.contains("unknown fields"), "err={err}");
    assert!(err.contains("'bar'"), "err={err}");
    assert!(err.contains("'foo'"), "err={err}");
  }

  #[test]
  fn suggest_key_matches_after_stripping_underscores() {
    assert_eq!(
      suggest_key("scroll_back_len", &["scrollback_len", "cwd"]),
      Some("scrollback_len")
    );
  }

  #[test]
  fn edit_distance_basic() {
    assert_eq!(edit_distance("kitten", "sitting"), 3);
    assert_eq!(edit_distance("same", "same"), 0);
  }
}
