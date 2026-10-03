use std::sync::Arc;

use crate::kernel::{
  kernel_message::SharedVt,
  task::{TaskId, TaskKind, TaskState},
  task_path::TaskPath,
};

pub struct TaskView {
  pub id: TaskId,
  pub label: Option<String>,
  pub path: Arc<TaskPath>,
  pub kind: TaskKind,
  pub status: TaskState,
  pub vt: SharedVt,
  /// Copy-mode surface, shown instead of `vt` while set.
  pub present: Option<SharedVt>,
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
    self.status.is_active()
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
