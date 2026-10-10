//! A diagonal sweep that paints a screen over whatever a terminal shows,
//! band by band, instead of all at once. Played after a live upgrade so
//! the new kernel's first paint is seen arriving rather than snapping in.
//!
//! The band is a blue tint moving across the screen: it paints the new
//! screen tinted, strongest at its leading edge, and behind it the new
//! screen is painted as is. Ahead of it the terminal keeps whatever it
//! showed.

use unicode_width::UnicodeWidthStr;

use super::{
  Cell, Color, Palette, Rgb, attrs::Attrs, grid::Pos,
  screen_differ::BufferView, vt::emit,
};

/// Frames of terminal bytes; write each, then wait `FRAME_INTERVAL`.
/// Once the last frame is written every cell of `new` is on the
/// terminal, the cursor hidden and the attributes reset. The caller
/// paints again as usual afterwards, after telling its differ the cursor
/// is hidden, to place the cursor (and any change made meanwhile).
pub fn frames<N: BufferView>(new: &N) -> Vec<Vec<u8>> {
  let size = new.size();
  if size.width == 0 || size.height == 0 {
    return Vec::new();
  }
  let steps = FRAMES.max(1);
  let last = depth(size.width - 1, size.height - 1) + BAND;
  let mut out = Vec::with_capacity(steps as usize);
  let mut painted: Vec<Paint> =
    vec![Paint::Old; size.width as usize * size.height as usize];
  let mut brush = Attrs::default();
  let mut pos = None;
  let default_cell = Cell::default();
  let cell_of = |x: u16, y: u16| -> &Cell {
    new
      .get_cell(Pos { col: x, row: y })
      .unwrap_or(&default_cell)
  };
  for step in 1..=steps {
    let front = (last as u32 * step as u32).div_ceil(steps as u32) as u16;
    let mut frame = Vec::new();
    if step == 1 {
      emit::dec_reset(&mut frame, emit::DecMode::ShowCursor);
    }
    for y in 0..size.height {
      for x in 0..size.width {
        let paint = match depth(x, y) {
          d if d + BAND <= front => Paint::New,
          d if d < front => Paint::Band(level(front - d)),
          _ => Paint::Old,
        };
        let index = y as usize * size.width as usize + x as usize;
        if painted[index] == paint {
          continue;
        }
        painted[index] = paint;
        let tint = match paint {
          Paint::Old => unreachable!("an untouched cell never changes"),
          Paint::New => None,
          Paint::Band(level) => Some(TINTS[level as usize]),
        };
        // The right half of a wide glyph is never drawn on its own; it
        // follows the glyph.
        if x > 0 && cell_of(x - 1, y).is_wide() {
          continue;
        }
        let cell = cell_of(x, y);
        let attrs = match tint {
          Some(tint) => tinted(*cell.attrs(), tint),
          None => *cell.attrs(),
        };
        emit::sgr(&mut frame, brush, attrs);
        brush = attrs;
        if pos != Some((x, y)) {
          emit::cup(&mut frame, y, x);
        }
        let text = if cell.width() > 0 {
          cell.contents()
        } else {
          " "
        };
        frame.extend_from_slice(text.as_bytes());
        // Past the last column the cursor is pending a wrap; the next
        // write there must be addressed, or it scrolls the terminal.
        let end = x + text.width() as u16;
        pos = (end < size.width).then_some((end, y));
      }
    }
    if step == steps {
      frame.extend_from_slice(emit::SGR_RESET.as_bytes());
    }
    out.push(frame);
  }
  out
}

/// Time between frames: 120 a second, so the sweep lasts a second.
pub const FRAME_INTERVAL: std::time::Duration =
  std::time::Duration::from_micros(8_333);
const FRAMES: u16 = 120;
/// Depth of the band, in `depth` units.
const BAND: u16 = 16;
/// How much of the wave color (out of 255) each level of the band takes,
/// from its leading edge back.
const TINTS: [u8; 4] = [150, 110, 80, 40];

/// Cells are about twice as tall as wide: `x + 2y` sweeps at 45°.
fn depth(x: u16, y: u16) -> u16 {
  x + 2 * y
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Paint {
  /// Still whatever the terminal showed before.
  Old,
  /// In the band, at one of its `level`s.
  Band(u8),
  /// The cell as `new` has it.
  New,
}

/// The band in `TINTS.len()` levels by `age`, the steps a cell is behind
/// the leading edge (1 = the edge): a cell is repainted once per level
/// while the band passes rather than every step.
fn level(age: u16) -> u8 {
  let quarter = BAND / 4;
  match age {
    a if a <= quarter => 0,
    a if a <= 2 * quarter => 1,
    a if a <= 3 * quarter => 2,
    _ => 3,
  }
}

const WAVE: Rgb = Rgb(0x2f, 0x81, 0xf7);
const WAVE_TEXT: Rgb = Rgb(0xea, 0xf3, 0xff);

/// Colors a terminal is assumed to use for the default and indexed ones,
/// which have no value of their own to tint.
fn assumed() -> Palette {
  Palette {
    fg: Rgb(0xd1, 0xd7, 0xe0),
    bg: Rgb(0x16, 0x1b, 0x22),
    ansi: [
      Rgb(0x48, 0x4f, 0x58),
      Rgb(0xff, 0x7b, 0x72),
      Rgb(0x3f, 0xb9, 0x50),
      Rgb(0xd2, 0x99, 0x22),
      Rgb(0x58, 0xa6, 0xff),
      Rgb(0xbc, 0x8c, 0xff),
      Rgb(0x39, 0xc5, 0xcf),
      Rgb(0xb1, 0xba, 0xc4),
      Rgb(0x6e, 0x76, 0x81),
      Rgb(0xff, 0xa1, 0x98),
      Rgb(0x56, 0xd3, 0x64),
      Rgb(0xe3, 0xb3, 0x41),
      Rgb(0x79, 0xc0, 0xff),
      Rgb(0xd2, 0xa8, 0xff),
      Rgb(0x56, 0xd4, 0xdd),
      Rgb(0xff, 0xff, 0xff),
    ],
  }
}

fn tinted(attrs: Attrs, amount: u8) -> Attrs {
  let palette = assumed();
  let (fg, bg) = if attrs.inverse() {
    (palette.bg(attrs.bgcolor), palette.fg(attrs.fgcolor))
  } else {
    (palette.fg(attrs.fgcolor), palette.bg(attrs.bgcolor))
  };
  Attrs {
    fgcolor: Color::from(fg.blend(WAVE_TEXT, amount)),
    bgcolor: Color::from(bg.blend(WAVE, amount)),
    ..attrs
  }
  .set_inverse(false)
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::term::{Screen, Size};

  fn screen_text(screen: &Screen) -> Vec<String> {
    (0..screen.size().height)
      .map(|row| {
        (0..screen.size().width)
          .map(|col| match screen.cell(row, col) {
            Some(cell) if cell.has_contents() => cell.contents().to_string(),
            _ => " ".to_string(),
          })
          .collect()
      })
      .collect()
  }

  #[test]
  fn every_cell_arrives_by_the_last_frame() {
    let size = Size {
      width: 12,
      height: 4,
    };
    let mut wanted = Screen::new(size, 0);
    let mut events = Vec::new();
    wanted.process(
      b"\x1b[1;1Hhello world!\x1b[2;3H\x1b[31mred\x1b[0m\x1b[4;12H#",
      &mut events,
    );
    let mut shown = Screen::new(size, 0);
    shown.process(b"\x1b[1;1Hold old old old old old old", &mut events);

    let frames = frames(&wanted);
    assert_eq!(frames.len(), 120);
    assert!(frames[0].starts_with(b"\x1b[?25l"));
    assert!(frames.last().unwrap().ends_with(b"\x1b[0m"));

    // The band starts at the top left; the far corner is still old.
    let early: Vec<u8> = frames[..8].concat();
    let early = String::from_utf8_lossy(&early);
    assert!(early.contains('h'), "{early:?}");
    assert!(!early.contains('#'), "{early:?}");

    let mut seen_tinted = false;
    for (i, frame) in frames.iter().enumerate() {
      shown.process(frame, &mut events);
      let top_left = shown.cell(0, 0).unwrap();
      // Tinted in the band, then as is.
      if i >= 1 && *top_left.attrs() != Attrs::default() {
        assert_eq!(top_left.contents(), "h");
        seen_tinted = true;
      }
    }
    assert!(seen_tinted);
    assert_eq!(screen_text(&shown), screen_text(&wanted));
    assert_eq!(
      shown.cell(1, 2).unwrap().attrs().fgcolor,
      wanted.cell(1, 2).unwrap().attrs().fgcolor
    );
    assert_eq!(*shown.cell(0, 0).unwrap().attrs(), Attrs::default());
    assert!(shown.hide_cursor());
  }

  /// Letters that never occur in an escape sequence, so counting them
  /// across the frames counts the cell's repaints.
  #[test]
  fn a_cell_is_repainted_once_per_level() {
    let size = Size {
      width: 6,
      height: 2,
    };
    let mut wanted = Screen::new(size, 0);
    let mut events = Vec::new();
    wanted.process(b"abcdef\r\nwxyzab", &mut events);
    let all = frames(&wanted).concat();
    let count = |c: u8| all.iter().filter(|b| **b == c).count();
    // Once per level of the band, then as is.
    for c in b"cdefwxyz" {
      assert_eq!(count(*c), TINTS.len() + 1, "{}", *c as char);
    }
  }

  #[test]
  fn an_empty_screen_has_no_frames() {
    let empty: Vec<Vec<Cell>> = Vec::new();
    assert!(frames(&empty).is_empty());
  }
}
