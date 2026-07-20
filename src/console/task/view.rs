use crate::kernel::{
  kernel_message::SharedVt,
  task::{TaskId, TaskState},
};

pub struct TaskView {
  pub id: TaskId,
  pub name: String,

  pub status: TaskState,
  pub vt: SharedVt,
  /// Presentation surface from the kernel's copy mode, rendered instead of
  /// `vt` while copy mode is active. Set/cleared by `CopyEntered`/`CopyLeft`.
  pub present: Option<SharedVt>,

  pub changed: bool,
}

impl TaskView {
  pub fn new(
    id: TaskId,
    name: String,
    status: TaskState,
    vt: SharedVt,
  ) -> Self {
    Self {
      id,
      name,
      status,
      vt,
      present: None,
      changed: false,
    }
  }

  pub fn set_name(&mut self, name: String) {
    self.name = name;
  }

  pub fn id(&self) -> TaskId {
    self.id
  }

  pub fn exit_code(&self) -> Option<i32> {
    match self.status {
      TaskState::Done(info) | TaskState::Exited(info) => info.code,
      TaskState::Idle
      | TaskState::Starting
      | TaskState::Running
      | TaskState::Ready
      | TaskState::Stopping
      | TaskState::Backoff => None,
    }
  }

  pub fn name(&self) -> &str {
    &self.name
  }

  pub fn is_up(&self) -> bool {
    self.status.is_active()
  }

  pub fn copy_active(&self) -> bool {
    self.present.is_some()
  }

  pub fn focus(&mut self) {
    self.changed = false;
  }
}
