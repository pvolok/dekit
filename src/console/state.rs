use crate::console::server_message::ClientId;
use crate::console::{
  keymap::KeymapGroup, task::view::TaskView, widgets::list::ListState,
};
use crate::kernel::task::TaskId;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Scope {
  Tasks,
  Term,
  TermZoom,
}

impl Scope {
  pub fn toggle(&self) -> Self {
    match self {
      Scope::Tasks => Scope::Term,
      Scope::Term => Scope::Tasks,
      Scope::TermZoom => Scope::Tasks,
    }
  }

  pub fn is_zoomed(&self) -> bool {
    match self {
      Scope::Tasks => false,
      Scope::Term => false,
      Scope::TermZoom => true,
    }
  }
}

pub struct State {
  pub current_client_id: Option<ClientId>,

  pub scope: Scope,
  pub tasks: Vec<TaskView>,
  pub tasks_list: ListState,
  pub hide_keymap_window: bool,

  pub quitting: bool,
}

impl State {
  pub fn selected(&self) -> usize {
    self.tasks_list.selected()
  }

  pub fn get_current_task(&self) -> Option<&TaskView> {
    self.tasks.get(self.tasks_list.selected())
  }

  pub fn select_task(&mut self, index: usize) {
    self.tasks_list.select(index);
    if let Some(task_handle) = self.tasks.get_mut(index) {
      task_handle.focus();
    }
  }

  pub fn get_task_mut(&mut self, id: TaskId) -> Option<&mut TaskView> {
    self.tasks.iter_mut().find(|p| p.id() == id)
  }

  pub fn get_keymap_group(&self) -> KeymapGroup {
    match self.scope {
      Scope::Tasks => KeymapGroup::Tasks,
      Scope::Term | Scope::TermZoom => match self.get_current_task() {
        Some(task) if task.copy_active() => KeymapGroup::Copy,
        _ => KeymapGroup::Term,
      },
    }
  }

  pub fn all_tasks_down(&self) -> bool {
    self.tasks.iter().all(|p| !p.is_up())
  }

  pub fn toggle_keymap_window(&mut self) {
    self.hide_keymap_window = !self.hide_keymap_window;
  }
}
