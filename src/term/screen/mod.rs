//! The terminal emulator: everything a program's output does to a
//! screen, and everything the screen tells back.

mod charset;
mod csi;
mod dcs;
mod graphics;
mod keyboard;
mod modes;
mod osc;
mod sgr;
pub mod snapshot;
mod text;

use std::sync::{Arc, Mutex, Weak};

pub use charset::CharSet;
pub use keyboard::KittyStack;
pub use modes::Modes;
pub(crate) use osc::parse_color_spec;

use super::{
  color::Rgb,
  common::{CursorStyle, Size},
  frame::{Frame, FrameColors, Tables},
  grid::{Grid, Pos},
  image::{ImageStore, kitty::Pending},
  modes::{MouseProtocolEncoding, MouseProtocolMode, ObservedModes},
  palette::Palette,
  style::{Link, LinkId, Style, StyleId, StyleTable},
  vt::{Scanner, Seq, emit},
};

/// The active grid's style table as frames show it, with the screen
/// colors applied, kept while the colors and the table stay the same.
/// The source is held weakly: the grid's next new style disassociates
/// it (`Arc::make_mut`), which is the cache miss.
#[derive(Clone, Debug)]
struct ShownStyles {
  colors: FrameColors,
  source: Weak<StyleTable>,
  len: usize,
  table: Arc<StyleTable>,
}

/// The cache slot, filled from `frame(&self)`. A mutex rather than a
/// `RefCell` only so the screen stays `Sync`; it is never contended.
#[derive(Debug, Default)]
struct ShownCache(Mutex<Option<ShownStyles>>);

impl Clone for ShownCache {
  fn clone(&self) -> Self {
    ShownCache(Mutex::new(self.get()))
  }
}

impl ShownCache {
  fn get(&self) -> Option<ShownStyles> {
    self.0.lock().unwrap_or_else(|e| e.into_inner()).clone()
  }

  fn set(&self, shown: ShownStyles) {
    *self.0.lock().unwrap_or_else(|e| e.into_inner()) = Some(shown);
  }
}

/// What a screen reports to its owner while processing output.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum VtEvent {
  Bell,
  /// Bytes the program expects back (a report or a query answer).
  Reply(Vec<u8>),
  /// OSC 52: the program put text on the clipboard.
  SetClipboard(String),
  /// OSC 9 / 777: a desktop notification.
  Notify {
    title: String,
    body: String,
  },
}

/// Cell size in pixels assumed until a client says otherwise.
pub const DEFAULT_CELL_PIXELS: (u16, u16) = (8, 16);

#[derive(Clone, Debug)]
pub struct Screen {
  scanner: Scanner,
  grid: Grid,
  alt_grid: Grid,

  style: Style,
  saved_style: Style,
  /// The hyperlink new cells belong to, and its id in each grid.
  link: Option<Link>,
  link_ids: [Option<LinkId>; 2],
  sgr_stack: Vec<Style>,

  modes: Modes,
  mouse_mode: MouseProtocolMode,
  mouse_encoding: MouseProtocolEncoding,
  cursor_style: CursorStyle,

  charsets: [CharSet; 4],
  /// Which of G0..G3 is in GL (SI, SO, LS2, LS3).
  gl: u8,
  /// SS2/SS3: the set for the next character only.
  single_shift: Option<u8>,

  kitty: KittyStack,
  alt_kitty: KittyStack,

  title: Arc<str>,
  title_stack: Vec<Arc<str>>,
  cwd: Option<String>,

  /// The colors a program sees when it asks (OSC 4/10/11), and what it
  /// set them to.
  palette: Palette,
  dynamic_fg: Option<Rgb>,
  dynamic_bg: Option<Rgb>,
  dynamic_cursor: Option<Rgb>,
  shown_styles: ShownCache,

  /// The size of a cell in pixels, for size reports and images.
  cell_pixels: (u16, u16),

  /// The last printed character, for REP.
  last_char: Option<char>,
  /// How many of a placeholder cell's diacritics were seen: the next one
  /// is its row (0), column (1), or image id's high byte (2).
  placeholder_marks: u8,

  /// Images programs transmitted, shared by both screens.
  images: ImageStore,
  /// A kitty transmission arriving in chunks.
  pending_image: Option<Pending>,

  events: Vec<VtEvent>,
}

impl Screen {
  pub fn new(size: Size, scrollback_len: usize) -> Self {
    let mut grid = Grid::new(size, scrollback_len);
    grid.enable_log();
    Screen {
      scanner: Scanner::default(),
      grid,
      alt_grid: Grid::new(size, 0),
      style: Style::DEFAULT,
      saved_style: Style::DEFAULT,
      link: None,
      link_ids: [None, None],
      sgr_stack: Vec::new(),
      modes: Modes::default(),
      mouse_mode: MouseProtocolMode::None,
      mouse_encoding: MouseProtocolEncoding::Default,
      cursor_style: CursorStyle::Default,
      charsets: [CharSet::Ascii; 4],
      gl: 0,
      single_shift: None,
      kitty: KittyStack::default(),
      alt_kitty: KittyStack::default(),
      title: Arc::from(""),
      title_stack: Vec::new(),
      cwd: None,
      palette: Palette::dark(),
      dynamic_fg: None,
      dynamic_bg: None,
      dynamic_cursor: None,
      shown_styles: ShownCache::default(),
      cell_pixels: DEFAULT_CELL_PIXELS,
      last_char: None,
      placeholder_marks: 0,
      images: ImageStore::default(),
      pending_image: None,
      events: Vec::new(),
    }
  }

  /// Clears the screen and scrollback, returning it to the initial state
  /// at the current size. The owner's settings (palette, cell size)
  /// stay, and so does the log's state: lines not taken yet, and what it
  /// looks for.
  pub fn reset(&mut self) {
    self.reset_with(self.grid.scrollback_len());
  }

  /// `reset`, keeping `scrollback_len` lines of history from now on.
  pub fn reset_with(&mut self, scrollback_len: usize) {
    let size = self.grid.size();
    let mut fresh = Screen::new(size, scrollback_len);
    fresh.grid.carry_log(&mut self.grid);
    *self = Screen {
      palette: self.palette.clone(),
      cell_pixels: self.cell_pixels,
      // Replies from earlier in this chunk are still owed.
      events: std::mem::take(&mut self.events),
      ..fresh
    };
  }

  /// DECSTR: a soft reset of modes, margins, and styles; the screen's
  /// contents stay.
  fn soft_reset(&mut self) {
    let width = self.grid.size().width;
    let height = self.grid.size().height;
    self.modes = Modes {
      alt_screen: self.modes.alt_screen,
      ..Modes::default()
    };
    self.style = Style::DEFAULT;
    self.saved_style = Style::DEFAULT;
    self.link = None;
    self.link_ids = [None, None];
    self.cursor_style = CursorStyle::Default;
    self.charsets = [CharSet::Ascii; 4];
    self.gl = 0;
    self.single_shift = None;
    self.mouse_mode = MouseProtocolMode::None;
    self.mouse_encoding = MouseProtocolEncoding::Default;
    for grid in [&mut self.grid, &mut self.alt_grid] {
      grid.set_scroll_region(0, height - 1);
      grid.set_margins(0, width - 1);
      grid.set_origin_mode(false);
    }
    let pos = self.grid.pos();
    self.grid_mut().set_pos(pos);
  }

  pub fn set_size(&mut self, rows: u16, cols: u16) {
    let size = Size {
      height: rows,
      width: cols,
    };
    if self.grid.size() == size {
      return;
    }
    self.grid.set_size(size);
    self.alt_grid.set_size(size);
    if self.modes.inband_resize {
      self.report_size_inband();
    }
  }

  fn report_size_inband(&mut self) {
    let size = self.grid.size();
    let (cell_w, cell_h) = self.cell_pixels;
    let mut out = Vec::new();
    emit::inband_resize(
      &mut out,
      size.height,
      size.width,
      size.height as u32 * cell_h as u32,
      size.width as u32 * cell_w as u32,
    );
    self.events.push(VtEvent::Reply(out));
  }

  pub fn size(&self) -> Size {
    self.grid.size()
  }

  /// The size of a cell in pixels, as the client that shows this screen
  /// reported it.
  pub fn set_cell_pixels(&mut self, width: u16, height: u16) {
    if width > 0 && height > 0 && self.cell_pixels != (width, height) {
      self.cell_pixels = (width, height);
      if self.modes.inband_resize {
        self.report_size_inband();
      }
    }
  }

  pub fn cell_pixels(&self) -> (u16, u16) {
    self.cell_pixels
  }

  // Grids

  fn active_grid(&self) -> &Grid {
    if self.modes.alt_screen {
      &self.alt_grid
    } else {
      &self.grid
    }
  }

  pub fn grid(&self) -> &Grid {
    self.active_grid()
  }

  pub fn grid_mut(&mut self) -> &mut Grid {
    if self.modes.alt_screen {
      &mut self.alt_grid
    } else {
      &mut self.grid
    }
  }

  fn enter_alternate_grid(&mut self, clear: bool) {
    self.grid.set_scrollback(0);
    if !self.modes.alt_screen {
      self.modes.alt_screen = true;
      std::mem::swap(&mut self.kitty, &mut self.alt_kitty);
    }
    if clear {
      let style = self.alt_grid.intern_style(Style::DEFAULT);
      self.alt_grid.erase_all(style);
      self.alt_grid.remove_placements(|_| true);
      self.alt_grid.set_pos(Pos::default());
    }
  }

  fn exit_alternate_grid(&mut self) {
    if self.modes.alt_screen {
      self.modes.alt_screen = false;
      std::mem::swap(&mut self.kitty, &mut self.alt_kitty);
    }
  }

  fn save_cursor(&mut self) {
    self.grid_mut().save_cursor();
    self.saved_style = self.style;
    self.modes.alt_saved_cursor = true;
  }

  fn restore_cursor(&mut self) {
    self.grid_mut().restore_cursor();
    let link = self.style.link;
    self.style = self.saved_style.with_link(link);
  }

  // Styles

  fn gl_set(&self) -> CharSet {
    self.charsets[usize::from(self.gl & 3)]
  }

  /// The current style, with the current link, interned in the active
  /// grid.
  fn style_id(&mut self) -> StyleId {
    let link = self.current_link_id();
    let style = self.style.with_link(link);
    self.grid_mut().intern_style(style)
  }

  fn current_link_id(&mut self) -> LinkId {
    let Some(link) = &self.link else {
      return LinkId::NONE;
    };
    let which = usize::from(self.modes.alt_screen);
    if let Some(id) = self.link_ids[which] {
      return id;
    }
    let link = link.clone();
    let id = self.grid_mut().intern_link(link);
    self.link_ids[which] = Some(id);
    id
  }

  fn set_link(&mut self, link: Option<Link>) {
    if self.link == link {
      return;
    }
    self.link = link;
    self.link_ids = [None, None];
  }

  // Observers

  pub fn title(&self) -> &str {
    &self.title
  }

  /// The cursor as (row, col), never past the last column.
  pub fn cursor_position(&self) -> (u16, u16) {
    let grid = self.active_grid();
    let pos = grid.pos();
    (pos.row, pos.col.min(grid.size().width - 1))
  }

  pub fn hide_cursor(&self) -> bool {
    self.modes.hide_cursor
  }

  /// For a task that draws into the screen directly instead of feeding
  /// it terminal output.
  pub fn set_hide_cursor(&mut self, hide: bool) {
    self.modes.hide_cursor = hide;
  }

  /// The cursor's shape, as `DECSCUSR` would set it; for the same tasks.
  pub fn set_cursor_style(&mut self, style: CursorStyle) {
    self.cursor_style = style;
  }

  pub fn application_cursor(&self) -> bool {
    self.modes.app_cursor
  }

  pub fn bracketed_paste(&self) -> bool {
    self.modes.bracketed_paste
  }

  pub fn mouse_protocol_mode(&self) -> MouseProtocolMode {
    self.mouse_mode
  }

  pub fn mouse_protocol_encoding(&self) -> MouseProtocolEncoding {
    self.mouse_encoding
  }

  pub fn modes(&self) -> &Modes {
    &self.modes
  }

  pub fn observed_modes(&self) -> ObservedModes {
    ObservedModes {
      mouse: self.mouse_mode,
      mouse_encoding: self.mouse_encoding,
      bracketed_paste: self.modes.bracketed_paste,
      kitty_flags: self.kitty.active(),
      app_cursor: self.modes.app_cursor,
      app_keypad: self.modes.app_keypad,
      focus_reporting: self.modes.focus_report,
      alt_screen: self.modes.alt_screen,
      alt_scroll: self.modes.alt_scroll,
      hide_cursor: self.modes.hide_cursor,
    }
  }

  /// Whether a synchronized update is holding paints.
  pub fn in_sync_update(&self) -> bool {
    self.modes.sync
  }

  /// Ends a synchronized update the program left open too long.
  pub fn end_sync_update(&mut self) {
    self.modes.sync = false;
  }

  /// The cell at `(row, col)` of the view.
  pub fn cell(&self, row: u16, col: u16) -> Option<super::cell::Cell> {
    self.active_grid().visible_cell(Pos { row, col })
  }

  pub fn images(&self) -> &ImageStore {
    &self.images
  }

  /// The screen-wide colors cells show through.
  pub fn frame_colors(&self) -> FrameColors {
    FrameColors {
      fg: self.dynamic_fg,
      bg: self.dynamic_bg,
      reverse: self.modes.reverse_video,
    }
  }

  /// The active grid's styles as shown: the grid's own table when no
  /// screen colors are set, else a mapped copy, cached while neither
  /// changes so consecutive frames share it.
  fn shown_styles(&self) -> Arc<StyleTable> {
    let colors = self.frame_colors();
    let source = &self.active_grid().tables().styles;
    if colors.is_plain() {
      return source.clone();
    }
    if let Some(shown) = self.shown_styles.get()
      && shown.colors == colors
      && shown.len == source.len()
      && shown
        .source
        .upgrade()
        .is_some_and(|s| Arc::ptr_eq(&s, source))
    {
      return shown.table;
    }
    let table = Arc::new(colors.apply_table(source));
    self.shown_styles.set(ShownStyles {
      colors,
      source: Arc::downgrade(source),
      len: source.len(),
      table: table.clone(),
    });
    table
  }

  /// A picture of the screen for observers.
  pub fn frame(&self) -> Frame {
    let grid = self.active_grid();
    let (row, col) = self.cursor_position();
    let store = &self.images;
    let rows = grid.visible_rows();
    let placements = grid.visible_placements(&rows, &|id| store.key_of(id));
    let images = if grid.images().is_empty() {
      store.table()
    } else {
      let mut merged = (*store.table()).clone();
      for (key, image) in grid.images() {
        merged.insert(*key, image.clone());
      }
      Arc::new(merged)
    };
    Frame {
      size: grid.size(),
      rows,
      tables: Tables {
        styles: self.shown_styles(),
        graphemes: grid.tables().graphemes.clone(),
        links: grid.tables().links.clone(),
      },
      cursor: if self.modes.hide_cursor || grid.scrollback() > 0 {
        None
      } else {
        Some(Pos { row, col })
      },
      cursor_style: self.cursor_style,
      title: self.title.clone(),
      images,
      placements,
    }
  }

  // Scrollback view

  pub fn scrollback(&self) -> usize {
    self.active_grid().scrollback()
  }

  pub fn max_scrollback(&self) -> usize {
    self.active_grid().max_scrollback()
  }

  pub fn set_scrollback(&mut self, rows: usize) {
    self.grid_mut().set_scrollback(rows);
  }

  pub fn scroll_screen_up(&mut self, n: usize) {
    let pos = self.scrollback().saturating_add(n);
    self.set_scrollback(pos);
  }

  pub fn scroll_screen_down(&mut self, n: usize) {
    let pos = self.scrollback().saturating_sub(n);
    self.set_scrollback(pos);
  }

  pub fn get_selected_text(
    &self,
    low_x: i32,
    low_y: i32,
    high_x: i32,
    high_y: i32,
  ) -> String {
    self
      .active_grid()
      .get_selected_text(low_x, low_y, high_x, high_y)
  }

  // Events and finished lines

  /// After a chunk: rows that scrolled under a scrolled-back view are
  /// brought into it, and images whose cell-bound placements were
  /// written over are freed when nothing shows them any more.
  fn settle_views(&mut self) {
    self.grid.settle_view();
    self.alt_grid.settle_view();
    let mut dropped = self.grid.take_dropped_images();
    dropped.append(&mut self.alt_grid.take_dropped_images());
    for key in dropped {
      let placed = self.grid.placements().iter().any(|p| p.key == key)
        || self.alt_grid.placements().iter().any(|p| p.key == key);
      if !placed && let Some(image) = self.images.get_by_key(key) {
        self.images.remove(image.id);
      }
    }
  }

  #[cfg(test)]
  pub fn has_pending_events(&self) -> bool {
    !self.events.is_empty()
  }

  /// Moves the events of the output processed so far into `out`.
  pub fn take_events(&mut self, out: &mut Vec<VtEvent>) {
    out.append(&mut self.events);
  }

  /// Moves the lines the main screen logged so far to `out` (see
  /// `Grid::log_newline`). The alternate screen logs nothing.
  pub fn take_log_lines(&mut self, out: &mut Vec<String>) {
    self.grid.take_log_lines(out);
  }

  /// Looks for `needle` in the main screen's lines: each logged line, and
  /// the line under way now, at each carriage return, and at the end of
  /// each output, so text a program overwrites in place is seen too.
  /// `None` stops looking.
  pub fn look_for(&mut self, needle: Option<String>) {
    self.grid.look_for(needle);
  }

  /// Whether the needle was seen since the last take; once seen, it is no
  /// longer looked for.
  pub fn take_found(&mut self) -> bool {
    self.grid.take_found()
  }

  /// The main screen's line under way: the cursor's, when it is not
  /// logged yet.
  pub fn open_log_line(&self) -> Option<String> {
    self.grid.open_log_line()
  }

  /// Logs the line under way as it is: the program is gone.
  pub fn finish_log_line(&mut self) {
    self.grid.finish_log_line();
  }

  // Processing

  /// <https://man7.org/linux/man-pages/man4/console_codes.4.html>
  /// <https://en.wikipedia.org/wiki/ANSI_escape_code>
  /// <https://terminalguide.namepad.de/seq>/
  /// <https://vt100.net/docs/vt510-rm/contents.html>
  /// <https://xtermjs.org/docs/api/vtfeatures/>
  /// <https://invisible-island.net/xterm/ctlseqs/ctlseqs.html>
  /// <https://sw.kovidgoyal.net/kitty/protocol-extensions/>
  pub fn process(&mut self, data: &[u8]) {
    #[cfg(test)]
    if data == b"\x1b_PANIC\x1b\\" {
      panic!("test: emulator panic");
    }
    let mut scanner = std::mem::take(&mut self.scanner);
    scanner.feed(data, |seq| self.apply(seq));
    self.scanner = scanner;
    // The output may pause on a line that is never ended.
    self.grid.log_check_open();
    self.settle_views();
  }

  /// The unfinished escape sequence the parser holds, for snapshots.
  pub fn pending_input(&self) -> Vec<u8> {
    self.scanner.pending()
  }

  fn apply(&mut self, seq: Seq) {
    match seq {
      Seq::Text(text) => self.text_run(text),
      Seq::Ctl(b) => self.control(b),
      Seq::Esc { inter: 0, final_ } => self.escape(final_),
      Seq::Esc { inter, final_ } => self.escape_nf(inter, final_),
      Seq::Csi(p) => self.csi(p),
      Seq::Osc(data) => self.osc(data),
      Seq::Dcs(data) => self.dcs(data),
      Seq::Apc(data) => self.apc(data),
      // Input-only items; an output scanner never produces them.
      Seq::EscChar(_) | Seq::Ss3(_) | Seq::X10Mouse(..) => (),
    }
  }

  fn control(&mut self, b: u8) {
    match b {
      0x07 => self.events.push(VtEvent::Bell),
      0x08 => self.grid_mut().col_dec(1),
      0x09 => self.grid_mut().col_tab(),
      0x0A..=0x0C => self.linefeed(),
      0x0D => {
        let grid = self.grid_mut();
        grid.log_check_open();
        let (left, _) = grid.margins();
        let col = if grid.pos().col >= left { left } else { 0 };
        grid.col_set(col);
      }
      0x0E => self.gl = 1,
      0x0F => self.gl = 0,
      // Legacy C1 RI byte.
      0x8D => self.reverse_index(),
      _ => (),
    }
  }

  fn linefeed(&mut self) {
    self.grid_mut().log_newline();
    let style = self.style_id();
    self.grid_mut().row_inc_scroll(1, style);
    if self.modes.newline {
      let (left, _) = self.active_grid().margins();
      self.grid_mut().col_set(left);
    }
  }

  fn reverse_index(&mut self) {
    let style = self.style_id();
    self.grid_mut().row_dec_scroll(1, style);
  }

  fn escape(&mut self, final_: u8) {
    match final_ {
      // DECSC / DECRC
      b'7' => self.save_cursor(),
      b'8' => self.restore_cursor(),
      // DECKPAM / DECKPNM
      b'=' => self.modes.app_keypad = true,
      b'>' => self.modes.app_keypad = false,
      // IND
      b'D' => {
        self.grid_mut().log_newline();
        let style = self.style_id();
        self.grid_mut().row_inc_scroll(1, style);
      }
      // NEL
      b'E' => {
        self.grid_mut().log_newline();
        let style = self.style_id();
        let (left, _) = self.active_grid().margins();
        self.grid_mut().row_inc_scroll(1, style);
        self.grid_mut().col_set(left);
      }
      // HTS
      b'H' => self.grid_mut().set_tab(),
      // RI
      b'M' => self.reverse_index(),
      // SS2 / SS3
      b'N' => self.single_shift = Some(2),
      b'O' => self.single_shift = Some(3),
      // LS2 / LS3
      b'n' => self.gl = 2,
      b'o' => self.gl = 3,
      // LS1R / LS2R / LS3R: GR sets, which we do not map.
      b'~' | b'}' | b'|' => (),
      // RIS
      b'c' => self.reset(),
      // PAD / nothing
      b'@' => (),
      c => log::debug!("Unhandled ESC {} ({:#x})", c as char, c),
    }
  }

  fn escape_nf(&mut self, inter: u8, final_: u8) {
    match inter {
      // Designate G0..G3 with 94-character sets.
      b'(' | b')' | b'*' | b'+' => {
        let set = CharSet::from_final(final_).unwrap_or(CharSet::Ascii);
        let index = match inter {
          b'(' => 0,
          b')' => 1,
          b'*' => 2,
          _ => 3,
        };
        self.charsets[index] = set;
      }
      // 96-character sets: treated as ASCII.
      b'-' | b'.' | b'/' => {
        let index = match inter {
          b'-' => 1,
          b'.' => 2,
          _ => 3,
        };
        self.charsets[index] = CharSet::Ascii;
      }
      // DECALN fills the screen with E; double-width/height lines are
      // not supported.
      b'#' => {
        if final_ == b'8' {
          self.screen_alignment();
        }
      }
      // ESC SP F / G: 7-bit / 8-bit controls.
      b' ' => (),
      // ESC % G / @: UTF-8 is all we speak.
      b'%' => (),
      _ => log::debug!("Ignored nF: ESC {} {}", inter as char, final_ as char),
    }
  }

  fn screen_alignment(&mut self) {
    let size = self.active_grid().size();
    let style = self.grid_mut().intern_style(Style::DEFAULT);
    let grid = self.grid_mut();
    grid.set_scroll_region(0, size.height - 1);
    grid.set_margins(0, size.width - 1);
    for row in 0..size.height {
      for col in 0..size.width {
        grid.write_cell(
          Pos { row, col },
          super::cell::CellText::scalar('E', 1),
          style,
        );
      }
    }
    grid.set_pos(Pos::default());
  }

  fn apc(&mut self, data: &[u8]) {
    // Kitty graphics; other application program commands are ignored.
    if let Some(body) = data.strip_prefix(b"G") {
      self.graphics_command(body);
    }
  }
}

#[cfg(test)]
mod graphics_tests;
#[cfg(test)]
mod tests;
