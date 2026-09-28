use crate::kernel::{
  kernel_message::SharedVt,
  task::{TaskId, TaskState},
  task_path::TaskPath,
};

pub struct TaskView {
  pub id: TaskId,
  pub label: Option<String>,
  pub path: Option<TaskPath>,
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
      .or_else(|| self.path.as_ref().map(|p| p.name().to_string()))
      .unwrap_or_else(|| format!("task-{}", self.id.0))
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
}
