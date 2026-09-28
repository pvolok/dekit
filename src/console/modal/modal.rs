use crate::console::{action::Action, keymap::Keymap};
use crate::term::{Grid, grid::Rect, key::Key};

pub enum ModalResult {
  Keep,
  Close,
  /// Close the modal, then run the action.
  Run(Action),
  /// Close the modal and detach the attachment that pressed the key.
  Detach,
}

pub trait Modal: Send {
  fn handle_key(&mut self, key: &Key) -> ModalResult;

  /// Width and height, given the frame the modal is centered in.
  fn size(&self, frame: Rect) -> (u16, u16);

  fn render(&mut self, grid: &mut Grid, keymap: &Keymap);

  fn area(&self, frame: Rect) -> Rect {
    let (w, h) = self.size(frame);
    let w = w.min(frame.width);
    let h = h.min(frame.height);
    Rect {
      x: (frame.width - w) / 2,
      y: (frame.height - h) / 2,
      width: w,
      height: h,
    }
  }
}
