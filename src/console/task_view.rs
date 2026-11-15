use std::sync::Arc;

use crate::console::app::RunnerId;
use crate::kernel::{
  task::{TaskId, TaskKind, TaskState},
  task_path::TaskPath,
};
use crate::spec::{Source, TaskSpec};

pub struct TaskView {
  /// Its id in the list.
  pub id: TaskId,
  /// The runner it is on, and its id there.
  pub runner: RunnerId,
  pub remote: TaskId,
  pub label: Option<String>,
  /// Where it is in the list: under its runner's group, when the list
  /// has one.
  pub path: Arc<TaskPath>,
  pub kind: TaskKind,
  pub status: TaskState,
  /// Its definition, when the runner says; an older one does not.
  pub spec: Option<TaskSpec>,
  /// The config entry it is written in; none for the runner's own.
  pub origin: Option<Source>,
}

impl TaskView {
  pub fn name(&self) -> String {
    self
      .label
      .clone()
      .unwrap_or_else(|| self.path.name().to_string())
  }

  pub fn exit_code(&self) -> Option<i32> {
    match self.status {
      TaskState::Done(info)
      | TaskState::Exited(info)
      | TaskState::Backoff(info) => info.code,
      TaskState::Idle
      | TaskState::Starting
      | TaskState::Running
      | TaskState::Ready
      | TaskState::Stopping => None,
    }
  }

  pub fn is_up(&self) -> bool {
    self.status.is_up(self.kind)
  }

  /// Ended badly: a non-zero exit, or not ready in time.
  pub fn failed(&self) -> bool {
    match self.status {
      TaskState::Done(info)
      | TaskState::Exited(info)
      | TaskState::Backoff(info) => {
        info.ready_timeout || info.code.is_some_and(|code| code != 0)
      }
      TaskState::Idle
      | TaskState::Starting
      | TaskState::Running
      | TaskState::Ready
      | TaskState::Stopping => false,
    }
  }
}
