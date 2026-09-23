use std::sync::atomic::{AtomicU64, Ordering};

use bytes::Bytes;
use tokio::sync::mpsc::{Sender, UnboundedSender};

use crate::{
  kernel::{
    copy_mode::{CopyMove, CopyState, Pos},
    kernel_message::SharedVt,
    task::TaskId,
  },
  task::logger::LogMsg,
  term::{
    Color, MouseProtocolEncoding, MouseProtocolMode, Reply, Screen, Size,
    TermEvent, VtEvent, Winsize,
    attrs::Attrs,
    grid::{Pos as GridPos, Rect},
    key::{Key, KeyCode, KeyMods},
    mouse::{MouseButton, MouseEvent, MouseEventKind},
    vt::emit,
  },
};

/// Size of a screen nothing is attached to yet.
pub const DEFAULT_SIZE: Size = Size {
  width: 80,
  height: 24,
};

/// Identifies one attachment to a screen. Allocated by the attacher, so
/// an attach needs no handshake before input and resize can follow it.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct ObserverId(u64);

impl ObserverId {
  pub fn new() -> Self {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    ObserverId(NEXT.fetch_add(1, Ordering::Relaxed))
  }
}

pub struct TaskScreen {
  task_id: TaskId,
  size: Winsize,
  vt: SharedVt,
  // Per-read events buffer; drained at the end of each process().
  events_buf: Vec<VtEvent>,

  observers: Vec<Observer>,
  /// An observer's sink closed since the last `settle`.
  lost_observers: bool,
  /// Raw output sinks (log files); they never affect the size.
  loggers: Vec<(u64, Sender<LogMsg>)>,
  next_logger_id: u64,

  copy: Option<CopySession>,
  /// Content cell of the last left mouse-down, so a drag can anchor the
  /// selection there and copy mode is only entered once a drag begins.
  mouse_down: Option<(u16, u16)>,
  /// Lines scrolled per mouse-wheel notch.
  wheel_lines: usize,
}

struct CopySession {
  state: CopyState,
  present: SharedVt,
}

struct Observer {
  id: ObserverId,
  size: Winsize,
  sink: UnboundedSender<ScreenNotify>,
}

pub enum TaskScreenCmd {
  Attach {
    observer: ObserverId,
    size: Winsize,
    sink: UnboundedSender<ScreenNotify>,
  },
  Detach {
    observer: ObserverId,
  },
  /// Terminal input from an attached observer. Resizes change that
  /// observer's requested geometry; keys and pastes not consumed by copy
  /// mode come back as effects for the task to feed its process.
  Input {
    observer: ObserverId,
    event: TermEvent,
  },

  CopyEnter,
  CopyLeave,
  CopyMove {
    dir: CopyMove,
  },
  CopyBeginSelection,
  /// Scroll the view by `delta` units: positive scrolls up into history.
  Scroll {
    delta: i32,
    unit: ScrollUnit,
  },
  CopyYank,
}

#[derive(Clone, Copy, Debug)]
pub enum ScrollUnit {
  Line,
  HalfScreen,
  Screen,
}

/// Delivered to an observer's sink. An observer whose sink is closed is
/// dropped; a screen that goes away closes every sink it held.
pub enum ScreenNotify {
  /// The attach took effect; paint the screen.
  Attached,
  Render,
  Bell,
  /// Copy mode began (`Some`, draw this surface instead) or ended.
  CopyPresent {
    screen: TaskId,
    vt: Option<SharedVt>,
  },
  Yank {
    text: String,
  },
}

pub enum TaskScreenEffect {
  Write(Vec<u8>),
  Resize(Winsize),
  Key(Key),
  Paste(String),
}

impl TaskScreen {
  pub fn vt(&self) -> &SharedVt {
    &self.vt
  }

  pub fn new(task_id: TaskId, vt: SharedVt, wheel_lines: usize) -> Self {
    let size = match vt.read() {
      Ok(screen) => screen.size(),
      Err(_) => Size {
        width: 80,
        height: 24,
      },
    };
    TaskScreen {
      task_id,
      size: Winsize {
        x: size.width,
        y: size.height,
        x_px: 0,
        y_px: 0,
      },
      vt,
      events_buf: Vec::new(),
      observers: Vec::new(),
      lost_observers: false,
      loggers: Vec::new(),
      next_logger_id: 0,
      copy: None,
      mouse_down: None,
      wheel_lines: wheel_lines.max(1),
    }
  }

  pub fn has_observers(&self) -> bool {
    !self.observers.is_empty()
  }

  fn broadcast(&mut self, mut make: impl FnMut(TaskId) -> ScreenNotify) {
    let task_id = self.task_id;
    let before = self.observers.len();
    self
      .observers
      .retain(|obs| obs.sink.send(make(task_id)).is_ok());
    if self.observers.len() != before {
      self.lost_observers = true;
    }
  }

  /// Observers that vanished without detaching must stop shaping the size.
  fn settle(&mut self, effects: &mut Vec<TaskScreenEffect>) {
    if std::mem::take(&mut self.lost_observers) {
      self.apply_size(effects);
    }
  }

  /// The task drew into `vt` itself (a UI rather than process output).
  pub fn rendered(&mut self, effects: &mut Vec<TaskScreenEffect>) {
    self.broadcast(|_| ScreenNotify::Render);
    self.settle(effects);
  }

  /// Text copied on this task's behalf (e.g. by a screen it shows).
  pub fn yank(&mut self, text: String, effects: &mut Vec<TaskScreenEffect>) {
    self.broadcast(|_| ScreenNotify::Yank { text: text.clone() });
    self.settle(effects);
  }

  pub async fn process(
    &mut self,
    bytes: &[u8],
    effects: &mut Vec<TaskScreenEffect>,
  ) {
    if let Ok(mut vt) = self.vt.write() {
      vt.process(bytes, &mut self.events_buf);
    }

    let bell = self.events_buf.iter().any(|event| match event {
      VtEvent::Bell => true,
      VtEvent::Reply(_) => false,
    });
    if bell {
      self.broadcast(|_| ScreenNotify::Bell);
    }
    self.broadcast(|_| ScreenNotify::Render);

    for event in std::mem::take(&mut self.events_buf) {
      match event {
        VtEvent::Bell => (),
        VtEvent::Reply(reply) => {
          let mut seq = Vec::new();
          match reply {
            Reply::PrimaryDeviceAttrs => emit::da1_reply(&mut seq),
            Reply::CursorPos { row, col } => emit::cpr(&mut seq, row, col),
            Reply::KittyFlags(flags) => {
              emit::kitty_flags_reply(&mut seq, flags)
            }
          }
          effects.push(TaskScreenEffect::Write(seq));
        }
      }
    }

    if !self.loggers.is_empty() {
      let bytes = Bytes::copy_from_slice(bytes);
      for (_, sink) in &self.loggers {
        let _ = sink.send(LogMsg::Bytes(bytes.clone())).await;
      }
    }
    self.settle(effects);
  }

  pub fn handle_cmd(
    &mut self,
    cmd: TaskScreenCmd,
    effects: &mut Vec<TaskScreenEffect>,
  ) {
    match cmd {
      TaskScreenCmd::Attach {
        observer,
        size,
        sink,
      } => {
        // Re-attaching replaces the previous registration.
        self.observers.retain(|o| o.id != observer);
        self.observers.push(Observer {
          id: observer,
          size,
          sink,
        });
        // Size first so Attached/CopyPresent paint the applied geometry.
        let resized = self.sync_size(effects);
        if resized && self.copy.is_some() {
          self.render_present();
        }
        if let Some(obs) = self.observers.last() {
          let _ = obs.sink.send(ScreenNotify::Attached);
          if let Some(session) = &self.copy {
            let _ = obs.sink.send(ScreenNotify::CopyPresent {
              screen: self.task_id,
              vt: Some(session.present.clone()),
            });
          }
        }
        if resized {
          self.broadcast(|_| ScreenNotify::Render);
        }
      }
      TaskScreenCmd::Detach { observer } => {
        self.observers.retain(|o| o.id != observer);
        self.apply_size(effects);
      }
      TaskScreenCmd::Input { observer, event } => match event {
        TermEvent::Resize(width, height) => {
          if let Some(obs) =
            self.observers.iter_mut().find(|o| o.id == observer)
          {
            obs.size = Winsize {
              x: width,
              y: height,
              x_px: 0,
              y_px: 0,
            };
          }
          self.apply_size(effects);
          if self.copy.is_some() {
            self.render_present();
            self.broadcast(|_| ScreenNotify::Render);
          }
        }
        TermEvent::Mouse(event) => self.handle_mouse(event, effects),
        TermEvent::Key(key) => {
          if self.copy.is_some() {
            self.copy_key(key);
          } else {
            effects.push(TaskScreenEffect::Key(key));
          }
        }
        TermEvent::Paste(text) => {
          if self.copy.is_none() {
            effects.push(TaskScreenEffect::Paste(text));
          }
        }
        TermEvent::FocusGained | TermEvent::FocusLost => (),
      },

      TaskScreenCmd::CopyEnter => {
        if let Some(present) = self.enter_copy() {
          self.render_present();
          self.broadcast(|screen| ScreenNotify::CopyPresent {
            screen,
            vt: Some(present.clone()),
          });
        }
      }
      TaskScreenCmd::CopyLeave => {
        self.leave_copy();
      }
      TaskScreenCmd::CopyMove { dir } => self.copy_move(dir),
      TaskScreenCmd::CopyBeginSelection => self.copy_begin_selection(),
      TaskScreenCmd::Scroll { delta, unit } => self.scroll(delta, unit),
      TaskScreenCmd::CopyYank => self.copy_yank(),
    }
    self.settle(effects);
  }

  /// Keys an attachment with no keymap of its own can use in copy mode.
  fn copy_key(&mut self, key: Key) {
    if key.mods != KeyMods::NONE {
      return;
    }
    match key.code {
      KeyCode::Esc => self.leave_copy(),
      KeyCode::Enter | KeyCode::Char('y') => self.copy_yank(),
      KeyCode::Char('v') | KeyCode::Char(' ') => self.copy_begin_selection(),
      KeyCode::Up | KeyCode::Char('k') => self.copy_move(CopyMove::Up),
      KeyCode::Down | KeyCode::Char('j') => self.copy_move(CopyMove::Down),
      KeyCode::Left | KeyCode::Char('h') => self.copy_move(CopyMove::Left),
      KeyCode::Right | KeyCode::Char('l') => self.copy_move(CopyMove::Right),
      _ => (),
    }
  }

  fn copy_move(&mut self, dir: CopyMove) {
    if let Some(session) = &mut self.copy {
      session.state.move_cursor(dir);
      self.render_present();
      self.broadcast(|_| ScreenNotify::Render);
    }
  }

  fn copy_begin_selection(&mut self) {
    if let Some(session) = &mut self.copy {
      session.state.begin_selection();
      self.render_present();
      self.broadcast(|_| ScreenNotify::Render);
    }
  }

  fn copy_yank(&mut self) {
    let text = self.copy.as_ref().and_then(|s| s.state.selected_text());
    if let Some(text) = text {
      self.broadcast(|_| ScreenNotify::Yank { text: text.clone() });
    }
    self.leave_copy();
  }

  /// Freezes the live screen into a copy session. Returns the presentation
  /// surface on fresh entry, None when already in copy mode.
  fn enter_copy(&mut self) -> Option<SharedVt> {
    if self.copy.is_some() {
      return None;
    }
    let snapshot = self.vt.read().ok()?.clone();
    let present = SharedVt::new(Screen::new(
      Size {
        height: self.size.y.max(1),
        width: self.size.x.max(1),
      },
      0,
    ));
    self.copy = Some(CopySession {
      state: CopyState::new(snapshot),
      present: present.clone(),
    });
    Some(present)
  }

  fn leave_copy(&mut self) {
    if self.copy.take().is_some() {
      self.broadcast(|screen| ScreenNotify::CopyPresent { screen, vt: None });
    }
  }

  fn handle_mouse(
    &mut self,
    event: MouseEvent,
    effects: &mut Vec<TaskScreenEffect>,
  ) {
    let (mouse_mode, encoding) = self
      .vt
      .read()
      .map(|screen| {
        (
          screen.mouse_protocol_mode(),
          screen.mouse_protocol_encoding(),
        )
      })
      .unwrap_or((MouseProtocolMode::None, MouseProtocolEncoding::Default));

    if mouse_mode != MouseProtocolMode::None {
      if mouse_forwarded(mouse_mode, event.kind) {
        let mut event = event;
        // X10 mode predates modifier reporting.
        if mouse_mode == MouseProtocolMode::Press {
          event.mods = KeyMods::NONE;
        }
        let mut seq = Vec::new();
        emit::mouse(&mut seq, &event, encoding);
        effects.push(TaskScreenEffect::Write(seq));
      }
      return;
    }

    let row = event.y.max(0) as u16;
    let col = event.x.max(0) as u16;
    match event.kind {
      MouseEventKind::Down(MouseButton::Left) => {
        self.mouse_down = Some((row, col));
        // Reposition the anchor if already selecting; a bare click in the
        // terminal does not enter copy mode.
        if let Some(session) = &mut self.copy {
          let pos = session.state.pos_at(row, col);
          session.state.set_anchor(pos);
          self.render_present();
          self.broadcast(|_| ScreenNotify::Render);
        }
      }
      MouseEventKind::Drag(MouseButton::Left) => {
        let entered = self.enter_copy();
        // A fresh drag anchors at the press cell; later drags only extend.
        let anchor = self.mouse_down.unwrap_or((row, col));
        if let Some(session) = &mut self.copy {
          if entered.is_some() {
            let apos = session.state.pos_at(anchor.0, anchor.1);
            session.state.set_anchor(apos);
          }
          let epos = session.state.pos_at(row, col);
          session.state.set_extent(epos);
        }
        self.render_present();
        match entered {
          Some(present) => self.broadcast(|screen| ScreenNotify::CopyPresent {
            screen,
            vt: Some(present.clone()),
          }),
          None => self.broadcast(|_| ScreenNotify::Render),
        }
      }
      MouseEventKind::Up(_) => self.mouse_down = None,
      MouseEventKind::ScrollUp => {
        self.scroll(self.wheel_lines as i32, ScrollUnit::Line)
      }
      MouseEventKind::ScrollDown => {
        self.scroll(-(self.wheel_lines as i32), ScrollUnit::Line)
      }
      MouseEventKind::Down(_)
      | MouseEventKind::Drag(_)
      | MouseEventKind::Moved
      | MouseEventKind::ScrollLeft
      | MouseEventKind::ScrollRight => {}
    }
  }

  /// Positive `delta` scrolls up into history.
  fn scroll(&mut self, delta: i32, unit: ScrollUnit) {
    let height = self.size.y.max(1) as i32;
    let delta = delta
      * match unit {
        ScrollUnit::Line => 1,
        ScrollUnit::HalfScreen => (height / 2).max(1),
        // Keep one line of overlap for continuity when paging.
        ScrollUnit::Screen => (height - 1).max(1),
      };
    if let Some(session) = &mut self.copy {
      if delta >= 0 {
        session.state.scroll_up(delta as usize);
      } else {
        session.state.scroll_down(delta.unsigned_abs() as usize);
      }
      self.render_present();
    } else if let Ok(mut screen) = self.vt.write() {
      if delta >= 0 {
        screen.scroll_screen_up(delta as usize);
      } else {
        screen.scroll_screen_down(delta.unsigned_abs() as usize);
      }
    }
    self.broadcast(|_| ScreenNotify::Render);
  }

  /// Composes the frozen snapshot (scrolled), the selection highlight, the HUD
  /// badge, and the selection cursor into the `present` surface that observers
  /// render.
  fn render_present(&mut self) {
    let Some(session) = &self.copy else {
      return;
    };
    let copy = &session.state;
    let Ok(mut present) = session.present.write() else {
      return;
    };
    let size = Size {
      width: self.size.x.max(1),
      height: self.size.y.max(1),
    };
    present.set_size(size.height, size.width);
    let grid = present.grid_mut();
    grid.set_scrollback(0);
    grid.erase_all(Attrs::default());

    let snapshot = copy.snapshot();
    let scrollback = copy.scrollback() as i32;
    let start = copy.start();
    let end = copy.end().unwrap_or(start);
    let highlight = Attrs::default().fg(Color::BLACK).bg(Color::CYAN);

    for row in 0..size.height {
      for col in 0..size.width {
        let Some(cell) = snapshot.cell(row, col) else {
          continue;
        };
        let Some(dst) = grid.drawing_cell_mut(GridPos { row, col }) else {
          continue;
        };
        *dst = cell.clone();
        if !cell.has_contents() {
          dst.set_str(" ");
        }
        let target = Pos {
          y: row as i32 - scrollback,
          x: col as i32,
        };
        if Pos::within(start, end, target) {
          dst.set_attrs(highlight);
        }
      }
    }

    // HUD badge in the top-right corner.
    let off = copy.scrollback();
    let label = if off > 0 {
      format!(" COPY -{} ", off)
    } else {
      " COPY ".to_string()
    };
    let width = (label.len() as u16).min(size.width);
    grid.draw_text(
      Rect::new(size.width - width, 0, width, 1),
      &label,
      Attrs::default().fg(Color::BLACK).bg(Color::BRIGHT_YELLOW),
    );

    // Place the cursor at the selection position.
    let cursor = copy.cursor();
    let cy = cursor.y + scrollback;
    if cy >= 0
      && cy < size.height as i32
      && cursor.x >= 0
      && cursor.x < size.width as i32
    {
      grid.set_pos(GridPos {
        row: cy as u16,
        col: cursor.x as u16,
      });
    }
  }

  /// Waits until every logger has written what it was sent.
  pub async fn flush_loggers(&self) {
    for (_, sink) in &self.loggers {
      let (tx, rx) = tokio::sync::oneshot::channel();
      if sink.send(LogMsg::Flush(tx)).await.is_ok() {
        let _ = rx.await;
      }
    }
  }

  pub fn add_logger(&mut self, sink: Sender<LogMsg>) -> u64 {
    let id = self.next_logger_id;
    self.next_logger_id += 1;
    self.loggers.push((id, sink));
    id
  }

  pub fn remove_logger(&mut self, id: u64) {
    self.loggers.retain(|(logger, _)| *logger != id);
  }

  fn apply_size(&mut self, effects: &mut Vec<TaskScreenEffect>) -> bool {
    if !self.sync_size(effects) {
      return false;
    }
    if self.copy.is_some() {
      self.render_present();
    }
    self.broadcast(|_| ScreenNotify::Render);
    true
  }

  fn sync_size(&mut self, effects: &mut Vec<TaskScreenEffect>) -> bool {
    // The smallest attached viewer wins, so every observer sees the whole
    // screen. With no observers the last size is kept.
    let size = self
      .observers
      .iter()
      .map(|obs| obs.size)
      .reduce(|a, b| Winsize {
        x: a.x.min(b.x),
        y: a.y.min(b.y),
        x_px: 0,
        y_px: 0,
      })
      .unwrap_or(self.size);
    // An observer on a degenerate terminal (0 rows or columns) must not
    // shrink the vt to nothing: the grid needs at least one cell.
    let size = Winsize {
      x: size.x.max(1),
      y: size.y.max(1),
      x_px: 0,
      y_px: 0,
    };
    if size == self.size {
      return false;
    }
    self.size = size;
    // Observers paint from vt; resize it before the following Render.
    if let Ok(mut vt) = self.vt.write() {
      vt.set_size(size.y, size.x);
    }
    effects.push(TaskScreenEffect::Resize(size));
    true
  }
}

/// Whether the child asked to receive this kind of mouse event under the
/// mouse mode it enabled.
fn mouse_forwarded(mode: MouseProtocolMode, kind: MouseEventKind) -> bool {
  match mode {
    MouseProtocolMode::None => false,
    MouseProtocolMode::Press => match kind {
      MouseEventKind::Down(_)
      | MouseEventKind::ScrollDown
      | MouseEventKind::ScrollUp
      | MouseEventKind::ScrollLeft
      | MouseEventKind::ScrollRight => true,
      MouseEventKind::Up(_)
      | MouseEventKind::Drag(_)
      | MouseEventKind::Moved => false,
    },
    MouseProtocolMode::PressRelease => match kind {
      MouseEventKind::Down(_)
      | MouseEventKind::Up(_)
      | MouseEventKind::ScrollDown
      | MouseEventKind::ScrollUp
      | MouseEventKind::ScrollLeft
      | MouseEventKind::ScrollRight => true,
      MouseEventKind::Drag(_) | MouseEventKind::Moved => false,
    },
    MouseProtocolMode::ButtonMotion => match kind {
      MouseEventKind::Down(_)
      | MouseEventKind::Up(_)
      | MouseEventKind::ScrollDown
      | MouseEventKind::Drag(_)
      | MouseEventKind::ScrollUp
      | MouseEventKind::ScrollLeft
      | MouseEventKind::ScrollRight => true,
      MouseEventKind::Moved => false,
    },
    MouseProtocolMode::AnyMotion => true,
  }
}

#[cfg(test)]
mod tests {
  use tokio::sync::mpsc::{UnboundedReceiver, unbounded_channel};

  use super::*;

  fn winsize(x: u16, y: u16) -> Winsize {
    Winsize {
      x,
      y,
      x_px: 0,
      y_px: 0,
    }
  }

  fn screen() -> TaskScreen {
    TaskScreen::new(TaskId(1), SharedVt::new(Screen::new(DEFAULT_SIZE, 0)), 1)
  }

  fn attach(
    screen: &mut TaskScreen,
    size: Winsize,
  ) -> (
    ObserverId,
    UnboundedReceiver<ScreenNotify>,
    Vec<TaskScreenEffect>,
  ) {
    let observer = ObserverId::new();
    let (sink, rx) = unbounded_channel();
    let mut effects = Vec::new();
    screen.handle_cmd(
      TaskScreenCmd::Attach {
        observer,
        size,
        sink,
      },
      &mut effects,
    );
    (observer, rx, effects)
  }

  fn take_notifies(
    rx: &mut UnboundedReceiver<ScreenNotify>,
  ) -> Vec<&'static str> {
    let mut out = Vec::new();
    loop {
      match rx.try_recv() {
        Ok(ScreenNotify::Attached) => out.push("attached"),
        Ok(ScreenNotify::Render) => out.push("render"),
        Ok(ScreenNotify::Bell) => out.push("bell"),
        Ok(ScreenNotify::CopyPresent { .. }) => out.push("copy"),
        Ok(ScreenNotify::Yank { .. }) => out.push("yank"),
        Err(_) => break,
      }
    }
    out
  }

  fn resize_of(effects: &[TaskScreenEffect]) -> Option<(u16, u16)> {
    effects.iter().find_map(|effect| match effect {
      TaskScreenEffect::Resize(size) => Some((size.x, size.y)),
      _ => None,
    })
  }

  fn vt_size(screen: &TaskScreen) -> Size {
    screen.vt().read().unwrap().size()
  }

  #[test]
  fn attach_at_default_size_sends_attached_only() {
    let mut screen = screen();
    let (_, mut rx, effects) = attach(&mut screen, winsize(80, 24));
    assert_eq!(take_notifies(&mut rx), ["attached"]);
    assert!(resize_of(&effects).is_none());
    assert_eq!(vt_size(&screen), DEFAULT_SIZE);
  }

  #[test]
  fn attach_at_other_size_resizes_vt_and_renders() {
    let mut screen = screen();
    let (_, mut rx, effects) = attach(&mut screen, winsize(100, 30));
    assert_eq!(take_notifies(&mut rx), ["attached", "render"]);
    assert_eq!(resize_of(&effects), Some((100, 30)));
    assert_eq!(
      vt_size(&screen),
      Size {
        width: 100,
        height: 30
      }
    );
  }

  #[test]
  fn resize_notifies_observers_after_geometry_change() {
    let mut screen = screen();
    let (observer, mut rx, _) = attach(&mut screen, winsize(80, 24));
    let _ = take_notifies(&mut rx);

    let mut effects = Vec::new();
    screen.handle_cmd(
      TaskScreenCmd::Input {
        observer,
        event: TermEvent::Resize(120, 40),
      },
      &mut effects,
    );
    assert_eq!(take_notifies(&mut rx), ["render"]);
    assert_eq!(resize_of(&effects), Some((120, 40)));
    assert_eq!(
      vt_size(&screen),
      Size {
        width: 120,
        height: 40
      }
    );
  }

  #[test]
  fn same_size_resize_is_silent() {
    let mut screen = screen();
    let (observer, mut rx, _) = attach(&mut screen, winsize(80, 24));
    let _ = take_notifies(&mut rx);

    let mut effects = Vec::new();
    screen.handle_cmd(
      TaskScreenCmd::Input {
        observer,
        event: TermEvent::Resize(80, 24),
      },
      &mut effects,
    );
    assert!(take_notifies(&mut rx).is_empty());
    assert!(resize_of(&effects).is_none());
  }

  #[test]
  fn detach_of_smaller_observer_grows_and_renders() {
    let mut screen = screen();
    let (small, mut small_rx, _) = attach(&mut screen, winsize(40, 12));
    let _ = take_notifies(&mut small_rx);
    let (_, mut large_rx, _) = attach(&mut screen, winsize(80, 24));
    // Second attach keeps the min size, so no geometry change.
    assert_eq!(take_notifies(&mut large_rx), ["attached"]);

    let mut effects = Vec::new();
    screen.handle_cmd(TaskScreenCmd::Detach { observer: small }, &mut effects);
    assert_eq!(take_notifies(&mut large_rx), ["render"]);
    assert_eq!(resize_of(&effects), Some((80, 24)));
    assert_eq!(vt_size(&screen), DEFAULT_SIZE);
  }
}
