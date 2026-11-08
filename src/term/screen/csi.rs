//! CSI sequences.

use crate::term::{
  common::CursorStyle,
  grid::Pos,
  vt::{Params, emit},
};

use super::{Screen, VtEvent};

impl Screen {
  pub(super) fn csi(&mut self, p: &Params) {
    if p.invalid() {
      csi_todo(p);
      return;
    }
    let style = self.style_id();
    match (p.prefix, p.inter, p.final_) {
      // ICH
      (0, 0, b'@') => self.grid_mut().insert_cells(p.get16(0, 1), style),
      // SL - shift left
      (0, b' ', b'@') => {
        let n = p.get16(0, 1).max(1);
        let grid = self.grid_mut();
        let pos = grid.pos();
        let (left, _) = grid.margins();
        let (top, _) = grid.scroll_region();
        grid.set_pos_absolute(Pos {
          row: top,
          col: left,
        });
        grid.delete_columns(n, style);
        grid.set_pos_absolute(pos);
      }
      // SR - shift right
      (0, b' ', b'A') => {
        let n = p.get16(0, 1).max(1);
        let grid = self.grid_mut();
        let pos = grid.pos();
        let (left, _) = grid.margins();
        let (top, _) = grid.scroll_region();
        grid.set_pos_absolute(Pos {
          row: top,
          col: left,
        });
        grid.insert_columns(n, style);
        grid.set_pos_absolute(pos);
      }
      // CUU
      (0, 0, b'A') => self.grid_mut().row_dec_clamp(p.get16(0, 1).max(1)),
      // CUD
      (0, 0, b'B') => self.grid_mut().row_inc_clamp(p.get16(0, 1).max(1)),
      // CUF
      (0, 0, b'C') => self.grid_mut().col_inc_clamp(p.get16(0, 1).max(1)),
      // CUB
      (0, 0, b'D') => self.grid_mut().col_dec(p.get16(0, 1).max(1)),
      // CNL
      (0, 0, b'E') => {
        let count = p.get16(0, 1).max(1);
        let grid = self.grid_mut();
        grid.row_inc_clamp(count);
        grid.col_set(0);
      }
      // CPL
      (0, 0, b'F') => {
        let count = p.get16(0, 1).max(1);
        let grid = self.grid_mut();
        grid.row_dec_clamp(count);
        grid.col_set(0);
      }
      // CHA
      (0, 0, b'G') | (0, 0, b'`') => {
        self.grid_mut().col_set(p.get16(0, 1).max(1) - 1)
      }
      // CUP / HVP
      (0, 0, b'H') | (0, 0, b'f') => {
        let row = p.get16(0, 1).max(1) - 1;
        let col = p.get16(1, 1).max(1) - 1;
        self.grid_mut().set_pos(Pos { row, col });
      }
      // CHT
      (0, 0, b'I') => self.grid_mut().tab_forward(p.get16(0, 1).max(1)),
      // ED / DECSED
      (0 | b'?', 0, b'J') => self.erase_display(p.get16(0, 0)),
      // EL / DECSEL
      (0 | b'?', 0, b'K') => self.erase_line(p.get16(0, 0)),
      // IL
      (0, 0, b'L') => self.grid_mut().insert_lines(p.get16(0, 1).max(1), style),
      // DL
      (0, 0, b'M') => self.grid_mut().delete_lines(p.get16(0, 1).max(1), style),
      // DCH
      (0, 0, b'P') => self.grid_mut().delete_cells(p.get16(0, 1).max(1), style),
      // SU
      (0, 0, b'S') => self.grid_mut().scroll_up(p.get16(0, 1).max(1), style),
      // XTSMGRAPHICS: graphics attributes query
      (b'?', 0, b'S') => self.graphics_attributes(p),
      // SD (one param; the five-param form is mouse tracking)
      (0, 0, b'T') => {
        if p.len() <= 1 {
          self.grid_mut().scroll_down(p.get16(0, 1).max(1), style);
        }
      }
      // ECH
      (0, 0, b'X') => self.grid_mut().erase_cells(p.get16(0, 1).max(1), style),
      // CBT
      (0, 0, b'Z') => self.grid_mut().tab_back(p.get16(0, 1).max(1)),
      // REP
      (0, 0, b'b') => self.repeat_last(p.get16(0, 1).max(1)),
      // DA1
      (0, 0, b'c') => self.reply(emit::da1_reply),
      // DA2
      (b'>', 0, b'c') => self.reply(emit::da2_reply),
      // DA3
      (b'=', 0, b'c') => self.reply(emit::da3_reply),
      // VPA
      (0, 0, b'd') => self.grid_mut().row_set(p.get16(0, 1).max(1) - 1),
      // VPR
      (0, 0, b'e') => self.grid_mut().row_inc_clamp(p.get16(0, 1).max(1)),
      // TBC
      (0, 0, b'g') => match p.get(0, 0) {
        0 => self.grid_mut().clear_tab(),
        3 => self.grid_mut().clear_all_tabs(),
        _ => (),
      },
      // SM / RM, DECSET / DECRST
      (0 | b'?', 0, b'h' | b'l') => self.set_modes(p),
      // SGR
      (0, 0, b'm') => self.sgr(p),
      // XTQMODKEYS and modifyOtherKeys: the client encodes keys.
      (b'>', 0, b'm') => (),
      (b'?', 0, b'm') => (),
      // DSR
      (0, 0, b'n') => match p.get(0, 0) {
        5 => self.reply(|out| out.extend_from_slice(b"\x1b[0n")),
        6 => {
          let pos = self.cursor_report_pos();
          self.reply(|out| emit::cpr(out, pos.row, pos.col));
        }
        _ => (),
      },
      // DECDSR
      (b'?', 0, b'n') => match p.get(0, 0) {
        6 => {
          let pos = self.cursor_report_pos();
          self.reply(|out| emit::decxcpr(out, pos.row, pos.col));
        }
        // Printer, UDK, keyboard, locator: nothing to report.
        15 => self.reply(|out| out.extend_from_slice(b"\x1b[?13n")),
        25 => self.reply(|out| out.extend_from_slice(b"\x1b[?21n")),
        26 => self.reply(|out| out.extend_from_slice(b"\x1b[?27;1;0;0n")),
        53 | 55 => self.reply(|out| out.extend_from_slice(b"\x1b[?53n")),
        _ => (),
      },
      // DECSTR
      (0, b'!', b'p') => self.soft_reset(),
      // DECRQM
      (0 | b'?', b'$', b'p') => self.request_mode(p),
      // DECSCL: set conformance level, nothing to do.
      (0, b'"', b'p') => (),
      // XTVERSION
      (b'>', 0, b'q') => self.reply(emit::xtversion_reply),
      // DECSCUSR
      (0, b' ', b'q') => {
        self.cursor_style = CursorStyle::from_u16(p.get16(0, 0));
      }
      // DECSCA: nothing is protected.
      (0, b'"', b'q') => (),
      // DECSTBM
      (0, 0, b'r') => {
        let top = p.get16(0, 1).max(1);
        let bottom = match p.get_opt(1) {
          Some(_) => p.get16(1, 1).max(1),
          None => self.active_grid().size().height,
        };
        self.grid_mut().set_scroll_region(top - 1, bottom - 1);
      }
      // DECSLRM, or SCOSC when margins are off
      (0, 0, b's') => {
        if self.modes.margins {
          let left = p.get16(0, 1).max(1);
          let right = match p.get_opt(1) {
            Some(_) => p.get16(1, 1).max(1),
            None => self.active_grid().size().width,
          };
          self.grid_mut().set_margins(left - 1, right - 1);
        } else {
          self.save_cursor();
        }
      }
      // SCORC
      (0, 0, b'u') if p.is_empty() => self.restore_cursor(),
      // Kitty keyboard protocol
      (b'?' | b'>' | b'<' | b'=', 0, b'u') => self.kitty_keyboard(p),
      // XTWINOPS
      (0, 0, b't') => self.window_op(p),
      // DECIC / DECDC
      (0, b'\'', b'}') => {
        self.grid_mut().insert_columns(p.get16(0, 1).max(1), style)
      }
      (0, b'\'', b'~') => {
        self.grid_mut().delete_columns(p.get16(0, 1).max(1), style)
      }
      // XTPUSHSGR / XTPOPSGR
      (0, b'#', b'{') | (0, b'#', b'p') => {
        if self.sgr_stack.len() < 16 {
          self.sgr_stack.push(self.style);
        }
      }
      (0, b'#', b'}') | (0, b'#', b'q') => {
        if let Some(style) = self.sgr_stack.pop() {
          let link = self.style.link;
          self.style = style.with_link(link);
        }
      }
      // DECSACE / DECCARA / DECRARA / DECCRA / DECFRA / DECERA / DECSERA:
      // rectangle operations nobody uses through a multiplexer.
      _ => csi_todo(p),
    }
  }

  fn erase_display(&mut self, mode: u16) {
    let style = self.style_id();
    // A clear of the whole screen starts the log afresh; a partial one
    // is a program redrawing what it showed.
    match mode {
      0 => {
        let grid = self.grid_mut();
        let home = grid.pos() == Pos::default();
        grid.erase_all_forward(style);
        if home {
          grid.log_restart();
        }
      }
      1 => self.grid_mut().erase_all_backward(style),
      2 => {
        self.grid_mut().erase_all(style);
        self.grid_mut().log_restart();
      }
      3 => self.grid_mut().clear_history(),
      n => log::debug!("Unhandled ED mode: {n}"),
    }
  }

  fn erase_line(&mut self, mode: u16) {
    let style = self.style_id();
    match mode {
      0 => self.grid_mut().erase_row_forward(style),
      1 => self.grid_mut().erase_row_backward(style),
      2 => self.grid_mut().erase_row(style),
      n => log::debug!("Unhandled EL mode: {n}"),
    }
  }

  /// The cursor position a report says: within the margins when origin
  /// mode is on, never past the last column.
  fn cursor_report_pos(&self) -> Pos {
    let grid = self.active_grid();
    let mut pos = grid.pos();
    pos.col = pos.col.min(grid.size().width - 1);
    if grid.origin_mode() {
      let (top, _) = grid.scroll_region();
      let (left, _) = grid.margins();
      pos.row = pos.row.saturating_sub(top);
      pos.col = pos.col.saturating_sub(left);
    }
    pos
  }

  /// `CSI Ps ; ... t`: reports sizes and the title stack; never resizes
  /// or moves anything, the client owns its window.
  fn window_op(&mut self, p: &Params) {
    let size = self.active_grid().size();
    let (cell_w, cell_h) = self.cell_pixels;
    match p.get(0, 0) {
      // Report text area size in pixels.
      14 => {
        let (w, h) = (
          size.width as u32 * cell_w as u32,
          size.height as u32 * cell_h as u32,
        );
        self.reply(|out| emit::window_report(out, 4, h, w));
      }
      // Report screen size in pixels: the same, we have no screen.
      15 => {
        let (w, h) = (
          size.width as u32 * cell_w as u32,
          size.height as u32 * cell_h as u32,
        );
        self.reply(|out| emit::window_report(out, 5, h, w));
      }
      // Report cell size in pixels.
      16 => self
        .reply(|out| emit::window_report(out, 6, cell_h as u32, cell_w as u32)),
      // Report text area size in characters.
      18 => self.reply(|out| {
        emit::window_report(out, 8, size.height as u32, size.width as u32)
      }),
      // Report screen size in characters.
      19 => self.reply(|out| {
        emit::window_report(out, 9, size.height as u32, size.width as u32)
      }),
      // Report icon label / title.
      20 | 21 => {
        let title = self.title.clone();
        let code = if p.get(0, 0) == 20 { b'L' } else { b'l' };
        self.reply(|out| {
          out.extend_from_slice(b"\x1b]");
          out.push(code);
          out.extend_from_slice(title.as_bytes());
          out.extend_from_slice(b"\x1b\\");
        });
      }
      // Push title.
      22 => {
        if self.title_stack.len() < 16 {
          self.title_stack.push(self.title.clone());
        }
      }
      // Pop title.
      23 => {
        if let Some(title) = self.title_stack.pop() {
          self.title = title;
        }
      }
      // Resize, move, iconify, raise: the client's window is not ours.
      _ => (),
    }
  }

  /// XTSMGRAPHICS queries: `CSI ? Pi ; Pa ; Pv S`. Reports how many
  /// sixel colors and how large an image we take.
  fn graphics_attributes(&mut self, p: &Params) {
    let item = p.get(0, 0);
    let action = p.get(1, 0);
    let reply: Option<String> = match (item, action) {
      // Color registers: 256, read or max.
      (1, 1) | (1, 4) => Some("\x1b[?1;0;256S".to_string()),
      // Sixel geometry.
      (2, 1) | (2, 4) => Some(format!("\x1b[?2;0;{};{}S", 4096, 4096)),
      // ReGIS: none.
      (3, _) => Some("\x1b[?3;3;0S".to_string()),
      _ => None,
    };
    if let Some(reply) = reply {
      self.events.push(VtEvent::Reply(reply.into_bytes()));
    }
  }

  fn reply(&mut self, write: impl FnOnce(&mut Vec<u8>)) {
    let mut out = Vec::new();
    write(&mut out);
    self.events.push(VtEvent::Reply(out));
  }
}

fn csi_todo(p: &Params) {
  log::debug!("CSI not implemented: {:?}", p);
}
