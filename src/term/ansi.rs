use super::{Color, attrs::Attrs, screen::Screen, vt::emit};

/// Render the screen contents as ANSI-styled text, one line per row,
/// trailing blanks trimmed; a blank with a background stays.
pub fn render_screen_ansi(screen: &Screen) -> String {
  let size = screen.size();
  let mut out: Vec<u8> = Vec::new();
  let mut brush = Attrs::default();

  for row in 0..size.height {
    if row > 0 {
      out.extend_from_slice(b"\r\n");
    }
    let end = (0..size.width)
      .rev()
      .find(|&col| {
        screen.cell(row, col).is_some_and(|cell| {
          let attrs = cell.attrs();
          !cell.contents().trim().is_empty()
            || attrs.bgcolor != Color::Default
            || attrs.inverse()
            || attrs.underline()
        })
      })
      .map_or(0, |col| col + 1);
    let mut line_brush = brush;

    for col in 0..end {
      let cell = match screen.cell(row, col) {
        Some(c) => c,
        None => continue,
      };
      let attrs = *cell.attrs();
      emit::sgr(&mut out, line_brush, attrs);
      line_brush = attrs;

      let c = if cell.width() > 0 {
        cell.contents()
      } else {
        " "
      };
      out.extend_from_slice(c.as_bytes());
    }
    brush = line_brush;
  }

  // Reset attributes at the end
  if brush != Attrs::default() {
    out.extend_from_slice(emit::SGR_RESET.as_bytes());
  }

  String::from_utf8(out).expect("emitted ANSI is valid utf-8")
}
