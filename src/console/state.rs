use crate::console::{
  keymap::KeymapGroup, task_tree::TaskTree, task_view::TaskView,
  ui_header::HeaderHits,
};

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

  pub fn is_term(&self) -> bool {
    match self {
      Scope::Tasks => false,
      Scope::Term => true,
      Scope::TermZoom => true,
    }
  }
}

pub struct State {
  pub scope: Scope,
  pub tasks: TaskTree,
  pub hide_keymap_window: bool,
  pub quitting: bool,
  /// The clickable parts of the header as last drawn.
  pub header: HeaderHits,
  /// Where the mouse was last seen, for hover highlights.
  pub hover: Option<(u16, u16)>,
}

impl State {
  pub fn current_task(&self) -> Option<&TaskView> {
    self.tasks.current()
  }

  pub fn keymap_group(&self) -> KeymapGroup {
    match self.scope {
      Scope::Tasks => KeymapGroup::Tasks,
      Scope::Term | Scope::TermZoom => match self.current_task() {
        Some(task) if task.present.is_some() => KeymapGroup::Copy,
        _ => KeymapGroup::Term,
      },
    }
  }
}
