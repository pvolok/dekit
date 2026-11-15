use crate::command::Command;
use crate::console::{action::Action, keymap::Keymap, theme::Theme};
use crate::term::{Grid, grid::Rect, key::Key, mouse::MouseEvent};

pub enum ModalResult {
  Keep,
  Close,
  /// Close the modal, then run the action.
  Run(Action),
  /// Keep the modal open, run the commands one after another, and tell
  /// it how that went (`Modal::issued`).
  Issue(Vec<Command>),
  /// Close the modal and carry the reload out as chosen.
  ApplyReload(Box<crate::reload::Plan>),
  /// Close the modal and write back the items at the paths `chosen`,
  /// with the runner's own tasks `include`d.
  WriteBack {
    include: Vec<crate::kernel::task::TaskId>,
    chosen: Vec<String>,
    how: super::write_back::How,
  },
}

/// Commands a modal issued, one of which failed.
pub struct IssueError {
  /// How many went in before it.
  pub done: usize,
  pub message: String,
}

pub trait Modal: Send {
  fn handle_key(&mut self, key: &Key) -> ModalResult;

  /// Mouse input inside the modal while it is open. Ignored unless the
  /// modal says otherwise.
  fn handle_mouse(&mut self, _mouse: &MouseEvent) -> ModalResult {
    ModalResult::Keep
  }

  /// A press outside the modal: closes it unless it says otherwise.
  fn click_outside(&mut self) -> ModalResult {
    ModalResult::Close
  }

  /// Text pasted while the modal is open. Ignored unless the modal says
  /// otherwise.
  fn handle_paste(&mut self, _text: &str) -> ModalResult {
    ModalResult::Keep
  }

  /// Width and height, given the frame the modal is centered in.
  fn size(&self, frame: Rect) -> (u16, u16);

  /// What the shade under the modal leaves alone: the control it
  /// dropped down from, if any.
  fn unshaded(&self) -> Option<Rect> {
    None
  }

  fn render(&mut self, grid: &mut Grid, keymap: &Keymap, theme: &Theme);

  /// How the commands it issued went; closes it unless one failed.
  fn issued(&mut self, result: Result<(), IssueError>) -> ModalResult {
    match result {
      Ok(()) => ModalResult::Close,
      Err(_) => ModalResult::Keep,
    }
  }

  fn area(&self, frame: Rect) -> Rect {
    let (w, h) = self.size(frame);
    frame.centered(w, h)
  }
}
