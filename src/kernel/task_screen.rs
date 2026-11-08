use std::{
  sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
  },
  time::Duration,
};

use bytes::Bytes;
use tokio::{
  sync::mpsc::{Sender, UnboundedSender},
  time::Instant,
};

use crate::{
  kernel::{
    copy_mode::{CopyMove, CopyState, Pos},
    task::TaskId,
  },
  protocol::caps::Background,
  task::logger::LogMsg,
  term::{
    Color, Frame, Geometry, MouseProtocolMode, Screen, ScreenHandle, Size,
    TermEvent, VtEvent, Winsize,
    cell::{Cell, CellText},
    common::CursorStyle,
    frame::FrameColors,
    grid::Pos as GridPos,
    image::ImageStore,
    key::{Key, KeyCode, KeyEventKind, KeyMods},
    mouse::{MouseButton, MouseEvent, MouseEventKind},
    style::{Style, StyleId},
    vt::emit,
  },
};

/// Size of a screen nothing is attached to yet.
pub const DEFAULT_SIZE: Size = Size {
  width: 80,
  height: 24,
};

const DRAG_SCROLL_INTERVAL: Duration = Duration::from_millis(50);

/// A synchronized update a program leaves open is painted anyway after
/// this long, as terminals do.
const SYNC_TIMEOUT: Duration = Duration::from_millis(150);

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

/// A task's screen: the terminal its program draws on, the observers
/// watching it, and the copy mode they share. The screen itself is owned
/// here; observers read the frames it publishes.
pub struct TaskScreen {
  task_id: TaskId,
  size: Winsize,
  screen: Screen,
  handle: ScreenHandle,
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
  /// Content cell of the latest left drag, held until the button is
  /// released, so a scroll can keep the selection extent under the pointer.
  drag: Option<(u16, u16)>,
  /// When a drag resting on the top or bottom row next scrolls the view.
  drag_scroll_at: Option<Instant>,
  /// Lines scrolled per mouse-wheel notch.
  wheel_lines: usize,
  /// A synchronized update began then; paints wait for its end.
  sync_since: Option<Instant>,
}

struct CopySession {
  state: CopyState,
  /// The images the frozen grid's placements refer to.
  images: ImageStore,
  /// The screen colors the live screen had.
  colors: FrameColors,
  /// Styles interned into the frozen grid for the selection and the
  /// badge.
  highlight: StyleId,
  badge: StyleId,
  /// What observers draw while in copy mode: the frozen screen with the
  /// selection and the badge over it.
  present_handle: ScreenHandle,
}

struct Observer {
  id: ObserverId,
  /// The size this observer shows, and its cell size when it said.
  geometry: Geometry,
  sink: UnboundedSender<ScreenNotify>,
  /// The dekit version of the client behind this observer, when it is
  /// an attach session rather than another screen.
  client_version: Option<String>,
}

pub enum TaskScreenCmd {
  Attach {
    observer: ObserverId,
    geometry: Geometry,
    sink: UnboundedSender<ScreenNotify>,
    /// The attaching client's dekit version; `None` for an in-process
    /// observer like the console.
    client_version: Option<String>,
    /// The attaching client's terminal background, when it knows it.
    background: Option<Background>,
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
  /// The screen changed; paint it.
  Render {
    screen: TaskId,
  },
  Bell,
  /// Copy mode began (`Some`, draw this surface instead) or ended.
  CopyPresent {
    screen: TaskId,
    handle: Option<ScreenHandle>,
  },
  Yank {
    text: String,
  },
  /// Sent to one attach session: its client should leave this runner
  /// and attach to the console of the named one.
  Switch {
    kind: String,
    root: String,
  },
}

pub enum TaskScreenEffect {
  Write(Vec<u8>),
  Resize(Winsize),
  Key(Key),
  Paste(String),
}

impl TaskScreen {
  pub fn new(
    task_id: TaskId,
    screen: Screen,
    handle: ScreenHandle,
    wheel_lines: usize,
  ) -> Self {
    let size = screen.size();
    TaskScreen {
      task_id,
      // Pixels are reported once a client measures its cells; until then
      // 0 says unknown, as a terminal without a pixel size does.
      size: Geometry {
        size,
        cell_pixels: None,
      }
      .winsize(),
      screen,
      handle,
      events_buf: Vec::new(),
      observers: Vec::new(),
      lost_observers: false,
      loggers: Vec::new(),
      next_logger_id: 0,
      copy: None,
      mouse_down: None,
      drag: None,
      drag_scroll_at: None,
      wheel_lines: wheel_lines.max(1),
      sync_since: None,
    }
  }

  /// Where observers read this screen.
  #[cfg(test)]
  pub fn handle(&self) -> &ScreenHandle {
    &self.handle
  }

  pub fn screen(&self) -> &Screen {
    &self.screen
  }

  /// For a task that draws its screen itself (a UI). Call `rendered`
  /// after drawing so observers see it.
  pub fn screen_mut(&mut self) -> &mut Screen {
    &mut self.screen
  }

  pub fn size(&self) -> Size {
    self.screen.size()
  }

  /// The pty size: cells, and pixels from the cell size an observer
  /// reported (0 while none has).
  pub fn winsize(&self) -> Winsize {
    self.size
  }

  /// Clears the screen for a fresh program, which keeps
  /// `scrollback_len` lines and scrolls `wheel_lines` a wheel notch.
  pub fn reset_screen(&mut self, scrollback_len: usize, wheel_lines: usize) {
    self.screen.reset_with(scrollback_len);
    self.wheel_lines = wheel_lines.max(1);
    self.publish();
  }

  fn publish(&mut self) {
    self.handle.publish(self.screen.frame());
  }

  /// Runs `f` on the emulator. A bug there must not take the task down
  /// with it (the handler would be gone, and with it the task's place in
  /// an upgrade): the panic is logged and the screen reset, the program
  /// keeps running.
  fn guarded(&mut self, what: &str, f: impl FnOnce(&mut Screen)) {
    let run = std::panic::AssertUnwindSafe(|| f(&mut self.screen));
    if std::panic::catch_unwind(run).is_err() {
      log::error!(
        "task {}: the terminal emulation panicked while {what}; the screen is reset",
        self.task_id.0
      );
      self.screen.reset();
    }
  }

  /// Publishes the screen and tells observers to paint it.
  fn render(&mut self) {
    self.publish();
    self.broadcast(|screen| ScreenNotify::Render { screen });
  }

  /// When the view should scroll again for a drag resting on the top or
  /// bottom row. The owner sleeps until then and calls `drag_scroll_tick`.
  pub fn drag_scroll_deadline(&self) -> Option<Instant> {
    self.drag_scroll_at
  }

  /// Scrolls one more line for a drag still resting on the top or bottom
  /// row, and schedules the next tick while there is more to scroll.
  pub fn drag_scroll_tick(&mut self) {
    self.drag_scroll_at = None;
    if self.drag_edge_scroll() {
      self.sync_drag_extent();
      self.repaint_copy();
    }
  }

  /// When a synchronized update left open must be painted anyway.
  pub fn sync_deadline(&self) -> Option<Instant> {
    self.sync_since.map(|since| since + SYNC_TIMEOUT)
  }

  /// Ends an overlong synchronized update and paints.
  pub fn sync_timeout(&mut self) {
    if self.sync_since.take().is_some() {
      self.screen.end_sync_update();
      self.render();
    }
  }

  pub fn has_observers(&self) -> bool {
    !self.observers.is_empty()
  }

  /// The smallest cell size an observer reported, if any did.
  pub fn reported_cell_pixels(&self) -> Option<(u16, u16)> {
    self
      .observers
      .iter()
      .filter_map(|obs| obs.geometry.cell_pixels)
      .reduce(|a, b| (a.0.min(b.0), a.1.min(b.1)))
  }

  /// The dekit versions of the attached clients, one per attach session.
  pub fn client_versions(&self) -> impl Iterator<Item = &str> {
    self
      .observers
      .iter()
      .filter_map(|o| o.client_version.as_deref())
  }

  /// Sends a notification to one observer. A closed sink is noticed the
  /// way a failed broadcast is.
  pub fn notify(&mut self, observer: ObserverId, notify: ScreenNotify) {
    if let Some(obs) = self.observers.iter().find(|o| o.id == observer)
      && obs.sink.send(notify).is_err()
    {
      self.lost_observers = true;
    }
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

  /// The task drew into the screen itself (a UI rather than process
  /// output).
  pub fn rendered(&mut self, effects: &mut Vec<TaskScreenEffect>) {
    self.render();
    self.settle(effects);
  }

  /// Text copied on this task's behalf (e.g. by a screen it shows).
  pub fn yank(&mut self, text: String, effects: &mut Vec<TaskScreenEffect>) {
    self.broadcast(|_| ScreenNotify::Yank { text: text.clone() });
    self.settle(effects);
  }

  /// Moves the lines the screen logged so far to `out`.
  pub fn take_log_lines(&mut self, out: &mut Vec<String>) {
    self.screen.take_log_lines(out);
  }

  pub async fn process(
    &mut self,
    bytes: &[u8],
    effects: &mut Vec<TaskScreenEffect>,
  ) {
    self.guarded("processing output", |screen| screen.process(bytes));
    self.screen.take_events(&mut self.events_buf);
    for event in std::mem::take(&mut self.events_buf) {
      match event {
        VtEvent::Bell => self.broadcast(|_| ScreenNotify::Bell),
        VtEvent::Reply(bytes) => effects.push(TaskScreenEffect::Write(bytes)),
        // The program put text on the clipboard: the attached clients
        // own the user's clipboard.
        VtEvent::SetClipboard(text) => {
          self.broadcast(|_| ScreenNotify::Yank { text: text.clone() });
        }
        VtEvent::Notify { title, body } => {
          log::debug!(
            "notification from task {}: {title} {body}",
            self.task_id.0
          );
        }
      }
    }
    // A synchronized update (`?2026`) left open holds the paint, from
    // when it was first seen open.
    if self.screen.in_sync_update() {
      self.sync_since.get_or_insert_with(Instant::now);
    } else {
      self.sync_since = None;
      self.render();
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
        geometry,
        sink,
        client_version,
        background: _,
      } => {
        // Re-attaching replaces the previous registration.
        self.observers.retain(|o| o.id != observer);
        self.observers.push(Observer {
          id: observer,
          geometry,
          sink,
          client_version,
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
              handle: Some(session.present_handle.clone()),
            });
          }
        }
        if resized {
          self.render();
        }
      }
      TaskScreenCmd::Detach { observer } => {
        self.observers.retain(|o| o.id != observer);
        self.apply_size(effects);
      }
      TaskScreenCmd::Input { observer, event } => match event {
        TermEvent::Resize(geometry) => {
          if let Some(obs) =
            self.observers.iter_mut().find(|o| o.id == observer)
          {
            obs.geometry = geometry;
          }
          self.apply_size(effects);
          if self.copy.is_some() {
            self.repaint_copy();
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
        TermEvent::FocusGained | TermEvent::FocusLost => {
          if self.screen.modes().focus_report {
            let mut seq = Vec::new();
            emit::focus(&mut seq, event == TermEvent::FocusGained);
            effects.push(TaskScreenEffect::Write(seq));
          }
        }
      },

      TaskScreenCmd::CopyEnter => {
        if let Some(present) = self.enter_copy() {
          self.render_present();
          self.broadcast(|screen| ScreenNotify::CopyPresent {
            screen,
            handle: Some(present.clone()),
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
    // A terminal reporting releases would otherwise act twice.
    if key.mods != KeyMods::NONE || key.kind == KeyEventKind::Release {
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
      self.repaint_copy();
    }
  }

  fn copy_begin_selection(&mut self) {
    if let Some(session) = &mut self.copy {
      session.state.begin_selection();
      self.repaint_copy();
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
  fn enter_copy(&mut self) -> Option<ScreenHandle> {
    if self.copy.is_some() {
      return None;
    }
    let mut snapshot = self.screen.grid().clone();
    let highlight =
      snapshot.intern_style(Style::default().fg(Color::BLACK).bg(Color::CYAN));
    let badge = snapshot
      .intern_style(Style::default().fg(Color::BLACK).bg(Color::BRIGHT_YELLOW));
    let present_handle = ScreenHandle::new(Frame::of_grid(&snapshot));
    self.copy = Some(CopySession {
      state: CopyState::new(snapshot),
      images: self.screen.images().clone(),
      colors: self.screen.frame_colors(),
      highlight,
      badge,
      present_handle: present_handle.clone(),
    });
    Some(present_handle)
  }

  fn leave_copy(&mut self) {
    self.drag_scroll_at = None;
    if self.copy.take().is_some() {
      self.broadcast(|screen| ScreenNotify::CopyPresent {
        screen,
        handle: None,
      });
    }
  }

  /// Returns whether the view moved.
  fn drag_edge_scroll(&mut self) -> bool {
    let (Some((row, _)), Some(session)) = (self.drag, &mut self.copy) else {
      self.drag_scroll_at = None;
      return false;
    };
    let before = session.state.scrollback();
    let more = if row == 0 {
      session.state.scroll_up(1);
      session.state.scrollback() < session.state.max_scrollback()
    } else if row + 1 >= self.size.y.max(1) {
      session.state.scroll_down(1);
      session.state.scrollback() > 0
    } else {
      false
    };
    let moved = session.state.scrollback() != before;
    self.drag_scroll_at =
      (moved && more).then(|| Instant::now() + DRAG_SCROLL_INTERVAL);
    moved
  }

  /// Keeps the selection extent on the cell under the held pointer after
  /// the view scrolled underneath it.
  fn sync_drag_extent(&mut self) {
    if let (Some((row, col)), Some(session)) = (self.drag, &mut self.copy) {
      let pos = session.state.pos_at(row, col);
      session.state.set_extent(pos);
    }
  }

  fn handle_mouse(
    &mut self,
    event: MouseEvent,
    effects: &mut Vec<TaskScreenEffect>,
  ) {
    let modes = self.screen.observed_modes();

    if modes.mouse != MouseProtocolMode::None && self.copy.is_none() {
      if mouse_forwarded(modes.mouse, event.kind) {
        let mut event = event;
        // X10 mode predates modifier reporting.
        if modes.mouse == MouseProtocolMode::Press {
          event.mods = KeyMods::NONE;
        }
        let mut seq = Vec::new();
        emit::mouse(
          &mut seq,
          &event,
          modes.mouse_encoding,
          self.screen.cell_pixels(),
        );
        effects.push(TaskScreenEffect::Write(seq));
      }
      return;
    }

    // In the alternate screen the wheel drives the program as arrow
    // keys (`?1007`), unless it is in copy mode.
    if modes.alt_screen && modes.alt_scroll && self.copy.is_none() {
      let key = match event.kind {
        MouseEventKind::ScrollUp => Some(KeyCode::Up),
        MouseEventKind::ScrollDown => Some(KeyCode::Down),
        _ => None,
      };
      if let Some(code) = key {
        for _ in 0..self.wheel_lines {
          effects.push(TaskScreenEffect::Key(Key::new(code, KeyMods::NONE)));
        }
        return;
      }
    }

    let row = event.y.max(0) as u16;
    let col = event.x.max(0) as u16;
    match event.kind {
      MouseEventKind::Down(MouseButton::Left) => {
        self.mouse_down = Some((row, col));
        self.drag = None;
        self.drag_scroll_at = None;
        // Reposition the anchor if already selecting; a bare click in the
        // terminal does not enter copy mode.
        if let Some(session) = &mut self.copy {
          let pos = session.state.pos_at(row, col);
          session.state.set_anchor(pos);
          self.repaint_copy();
        }
      }
      MouseEventKind::Drag(MouseButton::Left) => {
        let entered = self.enter_copy();
        // A fresh drag anchors at the press cell; later drags only extend.
        let anchor = self.mouse_down.unwrap_or((row, col));
        if let (Some(session), true) = (&mut self.copy, entered.is_some()) {
          let apos = session.state.pos_at(anchor.0, anchor.1);
          session.state.set_anchor(apos);
        }
        self.drag = Some((row, col));
        // Resting on the top or bottom row scrolls the view under the
        // pointer, so the selection can grow past what is visible.
        self.drag_edge_scroll();
        self.sync_drag_extent();
        self.render_present();
        match entered {
          Some(present) => self.broadcast(|screen| ScreenNotify::CopyPresent {
            screen,
            handle: Some(present.clone()),
          }),
          None => self.broadcast(|screen| ScreenNotify::Render { screen }),
        }
      }
      MouseEventKind::Up(_) => {
        self.mouse_down = None;
        self.drag = None;
        self.drag_scroll_at = None;
      }
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
      // A wheel turned mid-drag moves the content, not the pointer.
      self.sync_drag_extent();
      self.repaint_copy();
    } else {
      if delta >= 0 {
        self.screen.scroll_screen_up(delta as usize);
      } else {
        self
          .screen
          .scroll_screen_down(delta.unsigned_abs() as usize);
      }
      self.render();
    }
  }

  /// Composes the frozen snapshot (scrolled), the selection highlight, the HUD
  /// badge, and the selection cursor into the `present` surface that observers
  /// render.
  /// Repaints copy mode for its observers.
  fn repaint_copy(&mut self) {
    self.render_present();
    self.broadcast(|screen| ScreenNotify::Render { screen });
  }

  /// Publishes what observers draw in copy mode: the frozen screen at
  /// the observers' size, the selection over it, the badge, the cursor.
  /// Rows are shared with the frozen grid; only those the selection or
  /// the badge touch are copied.
  fn render_present(&mut self) {
    let Some(session) = &mut self.copy else {
      return;
    };
    let size = Size {
      width: self.size.x.max(1),
      height: self.size.y.max(1),
    };
    session.state.snapshot_mut().set_size(size);
    let copy = &session.state;
    let snapshot = copy.snapshot();
    let scrollback = copy.scrollback() as i32;
    let start = copy.start();
    let end = copy.end().unwrap_or(start);

    let mut rows = snapshot.visible_rows();
    let images = &session.images;
    let placements =
      snapshot.visible_placements(&rows, &|id| images.key_of(id));
    let mut tables = snapshot.tables().clone();
    if !session.colors.is_plain() {
      tables.styles = Arc::new(session.colors.apply_table(&tables.styles));
    }

    // The selection, on the rows it covers.
    let (low, high) = Pos::to_low_high(start, end);
    for (r, row) in rows.iter_mut().enumerate() {
      let y = r as i32 - scrollback;
      if y < low.y || y > high.y {
        continue;
      }
      let row = Arc::make_mut(row);
      for (x, cell) in row.cells.iter_mut().enumerate() {
        if Pos::within(start, end, Pos { y, x: x as i32 }) {
          cell.style = session.highlight;
        }
      }
    }

    // HUD badge in the top-right corner.
    let off = copy.scrollback();
    let label = if off > 0 {
      format!(" COPY -{off} ")
    } else {
      " COPY ".to_string()
    };
    if let Some(top) = rows.first_mut() {
      let top = Arc::make_mut(top);
      let first = size.width.saturating_sub(label.len() as u16);
      for (i, c) in label.chars().enumerate() {
        let cell = Cell::new(CellText::scalar(c, 1), session.badge);
        top.set(first + i as u16, cell);
      }
    }

    // The cursor at the selection position.
    let cursor = copy.cursor();
    let cy = cursor.y + scrollback;
    let cursor = (cy >= 0
      && cy < i32::from(size.height)
      && cursor.x >= 0
      && cursor.x < i32::from(size.width))
    .then_some(GridPos {
      row: cy as u16,
      col: cursor.x as u16,
    });
    session.present_handle.publish(Frame {
      size,
      rows,
      tables,
      cursor,
      cursor_style: CursorStyle::Default,
      title: Arc::from(""),
      images: images.table(),
      placements,
    });
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
    self.render();
    true
  }

  fn sync_size(&mut self, effects: &mut Vec<TaskScreenEffect>) -> bool {
    // The smallest attached viewer wins, so every observer sees the whole
    // screen. With no observers the last size is kept.
    let size = self
      .observers
      .iter()
      .map(|obs| obs.geometry.size)
      .reduce(|a, b| Size {
        width: a.width.min(b.width),
        height: a.height.min(b.height),
      })
      .unwrap_or(Size {
        width: self.size.x,
        height: self.size.y,
      });
    // The cell size is the smallest any observer reported: images sized
    // by it fit everyone. The pty learns it too, as pixels.
    let cell_pixels = self.reported_cell_pixels();
    if let Some((w, h)) = cell_pixels {
      self.screen.set_cell_pixels(w, h);
    }
    // An observer on a degenerate terminal (0 rows or columns) must not
    // shrink the screen to nothing: the grid needs at least one cell.
    let size = Geometry {
      size: Size {
        width: size.width.max(1),
        height: size.height.max(1),
      },
      cell_pixels,
    }
    .winsize();
    let changed = size != self.size;
    if changed {
      self.size = size;
      // Observers paint from the published frame; resize before the
      // following Render.
      self.guarded("resizing", |screen| screen.set_size(size.y, size.x));
    }
    // What the size or the cell size made the screen say (an in-band
    // resize report) goes to the child now, not with its next output.
    self.screen.take_events(&mut self.events_buf);
    for event in std::mem::take(&mut self.events_buf) {
      if let VtEvent::Reply(bytes) = event {
        effects.push(TaskScreenEffect::Write(bytes));
      }
    }
    if changed {
      effects.push(TaskScreenEffect::Resize(size));
    }
    changed
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

  fn cells(x: u16, y: u16) -> Geometry {
    Geometry::cells(x, y)
  }

  fn screen_of(size: Size, scrollback: usize, wheel: usize) -> TaskScreen {
    let screen = Screen::new(size, scrollback);
    let handle = ScreenHandle::new(screen.frame());
    TaskScreen::new(TaskId(1), screen, handle, wheel)
  }

  fn screen() -> TaskScreen {
    screen_of(DEFAULT_SIZE, 0, 1)
  }

  fn attach(
    screen: &mut TaskScreen,
    geometry: Geometry,
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
        geometry,
        sink,
        client_version: None,
        background: None,
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
        Ok(ScreenNotify::Render { .. }) => out.push("render"),
        Ok(ScreenNotify::Bell) => out.push("bell"),
        Ok(ScreenNotify::CopyPresent { .. }) => out.push("copy"),
        Ok(ScreenNotify::Yank { .. }) => out.push("yank"),
        Ok(ScreenNotify::Switch { .. }) => out.push("switch"),
        Err(_) => break,
      }
    }
    out
  }

  fn resize_of(effects: &[TaskScreenEffect]) -> Option<(u16, u16)> {
    winsize_of(effects).map(|size| (size.x, size.y))
  }

  fn winsize_of(effects: &[TaskScreenEffect]) -> Option<Winsize> {
    effects.iter().find_map(|effect| match effect {
      TaskScreenEffect::Resize(size) => Some(*size),
      _ => None,
    })
  }

  fn vt_size(screen: &TaskScreen) -> Size {
    screen.handle().load().size
  }

  #[test]
  fn attach_at_default_size_sends_attached_only() {
    let mut screen = screen();
    let (_, mut rx, effects) = attach(&mut screen, cells(80, 24));
    assert_eq!(take_notifies(&mut rx), ["attached"]);
    assert!(resize_of(&effects).is_none());
    assert_eq!(vt_size(&screen), DEFAULT_SIZE);
  }

  #[test]
  fn attach_at_other_size_resizes_vt_and_renders() {
    let mut screen = screen();
    let (_, mut rx, effects) = attach(&mut screen, cells(100, 30));
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
    let (observer, mut rx, _) = attach(&mut screen, cells(80, 24));
    let _ = take_notifies(&mut rx);

    let mut effects = Vec::new();
    screen.handle_cmd(
      TaskScreenCmd::Input {
        observer,
        event: TermEvent::resize(120, 40),
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
    let (observer, mut rx, _) = attach(&mut screen, cells(80, 24));
    let _ = take_notifies(&mut rx);

    let mut effects = Vec::new();
    screen.handle_cmd(
      TaskScreenCmd::Input {
        observer,
        event: TermEvent::resize(80, 24),
      },
      &mut effects,
    );
    assert!(take_notifies(&mut rx).is_empty());
    assert!(resize_of(&effects).is_none());
  }

  fn writes_of(effects: &[TaskScreenEffect]) -> Vec<u8> {
    effects
      .iter()
      .filter_map(|effect| match effect {
        TaskScreenEffect::Write(bytes) => Some(bytes.as_slice()),
        _ => None,
      })
      .flatten()
      .copied()
      .collect()
  }

  #[test]
  fn pty_pixels_follow_a_measured_cell_size() {
    let mut screen = screen();
    let (observer, _rx, effects) = attach(&mut screen, cells(80, 24));
    // Nothing measured yet: the pty's pixel size is unknown.
    assert!(resize_of(&effects).is_none());
    let size = screen.winsize();
    assert_eq!((size.x_px, size.y_px), (0, 0));
    // The child asked for in-band resize reports (and got one at once).
    screen.screen_mut().process(b"\x1b[?2048h");
    let mut events = Vec::new();
    screen.screen_mut().take_events(&mut events);
    assert_eq!(events.len(), 1);
    let geometry = TermEvent::Resize(Geometry {
      size: Size {
        width: 80,
        height: 24,
      },
      cell_pixels: Some((10, 20)),
    });
    let mut effects = Vec::new();
    screen.handle_cmd(
      TaskScreenCmd::Input {
        observer,
        event: geometry.clone(),
      },
      &mut effects,
    );
    let size = winsize_of(&effects).expect("the pty learns the pixel size");
    assert_eq!((size.x, size.y, size.x_px, size.y_px), (80, 24, 800, 480));
    // The child hears of the new cell size now, not with its next output.
    assert_eq!(writes_of(&effects), b"\x1b[48;24;80;480;800t");
    // The same geometry again changes nothing.
    let mut effects = Vec::new();
    screen.handle_cmd(
      TaskScreenCmd::Input {
        observer,
        event: geometry,
      },
      &mut effects,
    );
    assert!(effects.is_empty());
  }

  #[test]
  fn detach_of_smaller_observer_grows_and_renders() {
    let mut screen = screen();
    let (small, mut small_rx, _) = attach(&mut screen, cells(40, 12));
    let _ = take_notifies(&mut small_rx);
    let (_, mut large_rx, _) = attach(&mut screen, cells(80, 24));
    // Second attach keeps the min size, so no geometry change.
    assert_eq!(take_notifies(&mut large_rx), ["attached"]);

    let mut effects = Vec::new();
    screen.handle_cmd(TaskScreenCmd::Detach { observer: small }, &mut effects);
    assert_eq!(take_notifies(&mut large_rx), ["render"]);
    assert_eq!(resize_of(&effects), Some((80, 24)));
    assert_eq!(vt_size(&screen), DEFAULT_SIZE);
  }

  fn mouse(kind: MouseEventKind, x: i32, y: i32) -> TermEvent {
    TermEvent::Mouse(MouseEvent {
      kind,
      x,
      y,
      mods: KeyMods::NONE,
    })
  }

  /// A screen with history: 60 lines fed into a 24-row view, so rows
  /// `line37..line59` are visible and 37 rows sit above them.
  async fn screen_with_history()
  -> (TaskScreen, ObserverId, UnboundedReceiver<ScreenNotify>) {
    let mut screen = screen_of(DEFAULT_SIZE, 200, 3);
    let (observer, rx, mut effects) = attach(&mut screen, cells(80, 24));
    let mut out = String::new();
    for i in 0..60 {
      out.push_str(&format!("line{i}\r\n"));
    }
    screen.process(out.as_bytes(), &mut effects).await;
    (screen, observer, rx)
  }

  fn input(screen: &mut TaskScreen, observer: ObserverId, event: TermEvent) {
    let mut effects = Vec::new();
    screen.handle_cmd(TaskScreenCmd::Input { observer, event }, &mut effects);
  }

  fn copy_offset(screen: &TaskScreen) -> usize {
    screen.copy.as_ref().unwrap().state.scrollback()
  }

  fn copy_end(screen: &TaskScreen) -> Pos {
    screen.copy.as_ref().unwrap().state.end().unwrap()
  }

  #[tokio::test]
  async fn drag_on_top_row_scrolls_into_history_until_release() {
    let (mut screen, observer, _rx) = screen_with_history().await;
    input(
      &mut screen,
      observer,
      mouse(MouseEventKind::Down(MouseButton::Left), 0, 20),
    );
    input(
      &mut screen,
      observer,
      mouse(MouseEventKind::Drag(MouseButton::Left), 4, 0),
    );
    assert_eq!(copy_offset(&screen), 1);
    assert_eq!(copy_end(&screen), Pos { y: -1, x: 4 });
    assert!(screen.drag_scroll_deadline().is_some());

    // Every drag event on the edge row moves one more line.
    input(
      &mut screen,
      observer,
      mouse(MouseEventKind::Drag(MouseButton::Left), 4, 0),
    );
    assert_eq!(copy_offset(&screen), 2);
    assert_eq!(copy_end(&screen), Pos { y: -2, x: 4 });

    // So does every timer tick while the pointer rests there.
    screen.drag_scroll_tick();
    assert_eq!(copy_offset(&screen), 3);
    assert_eq!(copy_end(&screen), Pos { y: -3, x: 4 });
    assert!(screen.drag_scroll_deadline().is_some());
    let text = screen.copy.as_ref().unwrap().state.selected_text().unwrap();
    assert!(text.starts_with("34\nline35\nline36\nline37\n"), "{text:?}");

    // Release ends the drag and the timer; a stray tick does nothing.
    input(
      &mut screen,
      observer,
      mouse(MouseEventKind::Up(MouseButton::Left), 4, 0),
    );
    assert!(screen.drag_scroll_deadline().is_none());
    screen.drag_scroll_tick();
    assert_eq!(copy_offset(&screen), 3);
    assert_eq!(copy_end(&screen), Pos { y: -3, x: 4 });
  }

  #[tokio::test]
  async fn drag_on_bottom_row_scrolls_back_and_stops_at_live_screen() {
    let (mut screen, observer, _rx) = screen_with_history().await;
    input(
      &mut screen,
      observer,
      mouse(MouseEventKind::Down(MouseButton::Left), 0, 10),
    );
    input(
      &mut screen,
      observer,
      mouse(MouseEventKind::Drag(MouseButton::Left), 4, 10),
    );
    input(
      &mut screen,
      observer,
      mouse(MouseEventKind::ScrollUp, 4, 10),
    );
    assert_eq!(copy_offset(&screen), 3);

    input(
      &mut screen,
      observer,
      mouse(MouseEventKind::Drag(MouseButton::Left), 4, 23),
    );
    assert_eq!(copy_offset(&screen), 2);
    assert_eq!(copy_end(&screen), Pos { y: 21, x: 4 });
    screen.drag_scroll_tick();
    screen.drag_scroll_tick();
    assert_eq!(copy_offset(&screen), 0);
    assert_eq!(copy_end(&screen), Pos { y: 23, x: 4 });
    // Nothing further to scroll: the timer is disarmed.
    assert!(screen.drag_scroll_deadline().is_none());
  }

  #[tokio::test]
  async fn drag_inside_view_does_not_scroll_or_arm_timer() {
    let (mut screen, observer, _rx) = screen_with_history().await;
    input(
      &mut screen,
      observer,
      mouse(MouseEventKind::Down(MouseButton::Left), 0, 20),
    );
    input(
      &mut screen,
      observer,
      mouse(MouseEventKind::Drag(MouseButton::Left), 4, 10),
    );
    assert_eq!(copy_offset(&screen), 0);
    assert_eq!(copy_end(&screen), Pos { y: 10, x: 4 });
    assert!(screen.drag_scroll_deadline().is_none());
  }

  #[tokio::test]
  async fn wheel_mid_drag_keeps_extent_under_pointer() {
    let (mut screen, observer, _rx) = screen_with_history().await;
    input(
      &mut screen,
      observer,
      mouse(MouseEventKind::Down(MouseButton::Left), 0, 20),
    );
    input(
      &mut screen,
      observer,
      mouse(MouseEventKind::Drag(MouseButton::Left), 4, 20),
    );
    assert_eq!(copy_end(&screen), Pos { y: 20, x: 4 });
    input(
      &mut screen,
      observer,
      mouse(MouseEventKind::ScrollUp, 4, 20),
    );
    assert_eq!(copy_offset(&screen), 3);
    assert_eq!(copy_end(&screen), Pos { y: 17, x: 4 });
  }

  #[tokio::test]
  async fn output_held_by_a_synchronized_update_is_published_at_the_end() {
    let mut screen = screen_of(DEFAULT_SIZE, 0, 1);
    let (_observer, _rx, mut effects) = attach(&mut screen, cells(80, 24));
    screen
      .process(b"\x1b[?2026hfinal words", &mut effects)
      .await;
    assert_eq!(screen.handle().load().row_text(0, 0, 11).trim_end(), "");
    assert!(screen.sync_deadline().is_some());
    // What the task does once the program's output ends.
    screen.sync_timeout();
    assert_eq!(screen.handle().load().row_text(0, 0, 11), "final words");
  }

  #[tokio::test]
  async fn a_panic_in_the_emulator_resets_the_screen_and_keeps_the_task() {
    let mut screen = screen_of(DEFAULT_SIZE, 100, 3);
    let (_observer, _rx, mut effects) = attach(&mut screen, cells(80, 24));
    screen.process(b"hello", &mut effects).await;
    // Swallowed by the hook, not by the task.
    let outcome =
      std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        futures::executor::block_on(
          screen.process(b"\x1b_PANIC\x1b\\", &mut effects),
        )
      }));
    assert!(outcome.is_ok(), "the panic must not escape the task screen");
    screen.process(b"after", &mut effects).await;
    let frame = screen.handle().load();
    assert_eq!(frame.row_text(0, 0, 10).trim_end(), "after");
  }

  #[tokio::test]
  async fn copy_mode_shows_images() {
    use base64::Engine as _;
    let mut screen = screen_of(DEFAULT_SIZE, 100, 3);
    let (_observer, _rx, mut effects) = attach(&mut screen, cells(80, 24));
    let data =
      base64::engine::general_purpose::STANDARD.encode([255, 0, 0, 0, 255, 0]);
    let put = format!("\x1b[2;1H\x1b_Ga=T,f=24,s=2,v=1,i=1,q=2;{data}\x1b\\");
    screen.process(put.as_bytes(), &mut effects).await;
    screen.handle_cmd(TaskScreenCmd::CopyEnter, &mut effects);
    let present = screen.copy.as_ref().unwrap().present_handle.load();
    assert_eq!(present.placements.len(), 1);
    assert_eq!(
      (present.placements[0].row, present.placements[0].col),
      (1, 0)
    );
    assert!(present.images.contains_key(&present.placements[0].key));
  }

  #[tokio::test]
  async fn key_releases_do_not_drive_copy_mode() {
    let (mut screen, observer, _rx) = screen_with_history().await;
    let mut effects = Vec::new();
    screen.handle_cmd(TaskScreenCmd::CopyEnter, &mut effects);
    let before = screen.copy.as_ref().unwrap().state.cursor();
    let release = Key::new_with_kind(
      KeyCode::Char('k'),
      KeyMods::NONE,
      KeyEventKind::Release,
    );
    input(&mut screen, observer, TermEvent::Key(release));
    assert_eq!(screen.copy.as_ref().unwrap().state.cursor(), before);
    let press = Key::new(KeyCode::Char('k'), KeyMods::NONE);
    input(&mut screen, observer, TermEvent::Key(press));
    assert_ne!(screen.copy.as_ref().unwrap().state.cursor(), before);
  }

  #[tokio::test]
  async fn wheel_in_the_alternate_screen_sends_arrows() {
    let mut screen = screen_of(DEFAULT_SIZE, 0, 3);
    let (observer, _rx, mut effects) = attach(&mut screen, cells(80, 24));
    screen.process(b"\x1b[?1049h", &mut effects).await;
    let mut effects = Vec::new();
    screen.handle_cmd(
      TaskScreenCmd::Input {
        observer,
        event: mouse(MouseEventKind::ScrollUp, 1, 1),
      },
      &mut effects,
    );
    let keys: Vec<KeyCode> = effects
      .iter()
      .filter_map(|e| match e {
        TaskScreenEffect::Key(key) => Some(key.code),
        _ => None,
      })
      .collect();
    assert_eq!(keys, vec![KeyCode::Up, KeyCode::Up, KeyCode::Up]);
    // Off, the wheel scrolls nothing in the alternate screen.
    screen.process(b"\x1b[?1007l", &mut effects).await;
    let mut effects = Vec::new();
    screen.handle_cmd(
      TaskScreenCmd::Input {
        observer,
        event: mouse(MouseEventKind::ScrollUp, 1, 1),
      },
      &mut effects,
    );
    assert!(effects.is_empty());
  }

  #[tokio::test]
  async fn synchronized_updates_hold_paints() {
    let mut screen = screen();
    let (_, mut rx, mut effects) = attach(&mut screen, cells(80, 24));
    let _ = take_notifies(&mut rx);
    screen.process(b"\x1b[?2026hhidden", &mut effects).await;
    assert!(take_notifies(&mut rx).is_empty());
    assert!(screen.sync_deadline().is_some());
    screen.process(b" more\x1b[?2026l", &mut effects).await;
    assert_eq!(take_notifies(&mut rx), ["render"]);
    assert!(screen.sync_deadline().is_none());
    // Left open, the timeout paints.
    screen.process(b"\x1b[?2026hstuck", &mut effects).await;
    screen.sync_timeout();
    assert_eq!(take_notifies(&mut rx), ["render"]);
  }

  #[tokio::test]
  async fn clipboard_and_focus() {
    let mut screen = screen();
    let (observer, mut rx, mut effects) = attach(&mut screen, cells(80, 24));
    let _ = take_notifies(&mut rx);
    screen.process(b"\x1b]52;c;aGk=\x07", &mut effects).await;
    assert!(take_notifies(&mut rx).contains(&"yank"));
    screen.process(b"\x1b[?1004h", &mut effects).await;
    let mut effects = Vec::new();
    screen.handle_cmd(
      TaskScreenCmd::Input {
        observer,
        event: TermEvent::FocusGained,
      },
      &mut effects,
    );
    assert!(
      matches!(&effects[..], [TaskScreenEffect::Write(seq)] if seq == b"\x1b[I")
    );
  }
}
