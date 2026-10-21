//! A folder entry: a key under `tasks:` that ends in `/`, as in
//! `apps/: {collapsed: true}`. It names a folder, which holds the tasks
//! under its path and stays while it is empty.

use anyhow::{Result, bail};
use serde_yaml::Value;

use crate::cfg::{CfgCx, CfgNode};
use crate::kernel::kernel_message::FolderSettings;
use crate::kernel::task_path::TaskPath;
use crate::spec::{Source, TaskSpec};

/// Keys a folder entry takes.
pub(crate) const FOLDER_KEYS: &[&str] = &["collapsed"];

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct FolderConfig {
  pub path: String,
  /// Collapsed when the console first lists it.
  pub collapsed: bool,
  /// Where in the config the folder is written.
  pub source: Option<Source>,
  /// How many task entries the config has before it: its place among
  /// them.
  pub at: usize,
}

impl FolderConfig {
  /// The entry as the base records it and a reload compares it.
  pub fn spec(&self) -> TaskSpec {
    let mut spec = TaskSpec::default();
    if self.collapsed {
      spec.set("collapsed", Some(Value::Bool(true)));
    }
    spec
  }

  /// What a folder from this entry is.
  pub fn settings(&self) -> FolderSettings {
    FolderSettings {
      collapsed: self.collapsed,
      origin: self.source.clone(),
    }
  }
}

/// The folder an entry key names: the path before the `/` it ends in.
pub fn folder_key(key: &str) -> Option<&str> {
  key.strip_suffix('/')
}

/// Whether a folder entry, as `FolderConfig::spec` writes it, collapses.
pub fn collapsed(spec: &TaskSpec) -> bool {
  spec.get("collapsed").and_then(Value::as_bool) == Some(true)
}

pub(crate) fn folder_from_cfg(
  path: String,
  node: &CfgNode<'_>,
  cx: &CfgCx,
) -> Result<FolderConfig> {
  if let Err(err) = TaskPath::new(path.as_str()) {
    bail!("folder '{}/': {}", path, err);
  }
  let mut folder = FolderConfig {
    path,
    ..FolderConfig::default()
  };
  if node.is_null() {
    return Ok(folder);
  }
  let obj = node.as_obj()?;
  obj.known_keys(FOLDER_KEYS)?;
  folder.collapsed = obj.default("collapsed", false, cx)?;
  Ok(folder)
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::cfg::CfgDoc;
  use std::path::PathBuf;

  fn parse(yaml: &str) -> Result<FolderConfig> {
    let value: Value = serde_yaml::from_str(yaml).unwrap();
    let cx = CfgCx::new(PathBuf::from("."));
    let doc = CfgDoc::from_value(value);
    folder_from_cfg("apps".into(), &doc.root(), &cx)
  }

  #[test]
  fn takes_collapsed_or_nothing() {
    assert!(parse("collapsed: true").unwrap().collapsed);
    assert!(!parse("collapsed: false").unwrap().collapsed);
    assert!(!parse("{}").unwrap().collapsed);
    assert!(!parse("~").unwrap().collapsed);
    for (yaml, expected) in [
      ("collapse: true", "did you mean 'collapsed'?"),
      ("cmd: x", "unknown field 'cmd'"),
      ("collapsed: 1", "expected bool"),
      ("[]", "expected object"),
    ] {
      let err = parse(yaml).unwrap_err().to_string();
      assert!(err.contains(expected), "{yaml}: {err}");
    }
  }

  #[test]
  fn the_spec_says_only_what_is_set() {
    let folder = |collapsed| FolderConfig {
      collapsed,
      ..FolderConfig::default()
    };
    assert_eq!(folder(false).spec(), TaskSpec::default());
    assert!(collapsed(&folder(true).spec()));
    assert!(!collapsed(&folder(false).spec()));
  }
}
