//! Helpers for the unit tests.

use std::path::PathBuf;

use crate::kernel::kernel_message::{TaskDescription, TaskInfo};
use crate::kernel::task::{TaskId, TaskKind, TaskState};
use crate::kernel::task_key::TaskSpaceId;
use crate::kernel::task_path::TaskPath;
use crate::spec::{Source, TaskSpec};

/// A definition as YAML writes it.
pub fn spec(yaml: &str) -> TaskSpec {
  serde_yaml::from_str(yaml).unwrap()
}

/// The entry `key` in `dekit.yaml`.
pub fn source(key: &str) -> Source {
  Source {
    file: "dekit.yaml".into(),
    key: key.into(),
    mount: String::new(),
  }
}

/// An idle task at `path` defined by `spec`, from the entry `origin` or
/// the runner's own.
pub fn task(
  id: usize,
  path: &str,
  spec: TaskSpec,
  origin: Option<Source>,
) -> TaskDescription {
  TaskDescription {
    info: TaskInfo {
      id: TaskId(id),
      space: TaskSpaceId::default_space(),
      path: Some(TaskPath::new(path).unwrap()),
      label: None,
      state: TaskState::Idle,
      vt: None,
    },
    kind: TaskKind::Service,
    spec: Some(spec),
    origin,
  }
}

/// The runner with these tasks and no folders.
pub fn live_with(
  tasks: impl IntoIterator<Item = TaskDescription>,
) -> crate::reload::Live {
  crate::reload::Live {
    tasks: tasks.into_iter().collect(),
    folders: Vec::new(),
  }
}

/// A fresh dir under the system temp dir; removed when dropped.
pub struct TempDir(pub PathBuf);

impl TempDir {
  pub fn new(name: &str) -> Self {
    let dir =
      std::env::temp_dir().join(format!("dekit-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    TempDir(dir)
  }
}

impl Drop for TempDir {
  fn drop(&mut self) {
    let _ = std::fs::remove_dir_all(&self.0);
  }
}
