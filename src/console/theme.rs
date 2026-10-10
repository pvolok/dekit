use crate::term::{
  Grid, Palette, Rgb,
  attrs::Attrs,
  grid::{Block, BorderType, Rect},
};

/// The console paints every color itself, so it reads the same on light and
/// dark terminals.
pub struct Theme {
  pub dark: bool,
  /// For the task screens; its default colors are the blocks' `text` and
  /// `bg`.
  pub palette: Palette,
  pub block: BlockTheme,
  pub tasks: TaskListTheme,
}

pub struct BlockTheme {
  pub bg: Rgb,
  pub modal_bg: Rgb,
  /// The header bar across the top: lighter than the blocks under it.
  pub header_bg: Rgb,
  /// The `dekit` badge at the bar's left end.
  pub logo_bg: Rgb,
  pub logo_fg: Rgb,
  /// The runner's name in the bar.
  pub header_project: Rgb,
  /// Everything under an open modal moves `shade_amount`/255 toward
  /// `shade`.
  pub shade: Rgb,
  pub shade_amount: u8,
  /// The selected row of a list.
  pub selected_bg: Rgb,
  pub text: Rgb,
  /// Secondary text, like the terminal's own title.
  pub dim: Rgb,
  /// Key names in help and menus.
  pub key: Rgb,
  pub border: Rgb,
  pub border_focused: Rgb,
  pub title: Rgb,
  pub title_focused: Rgb,
  /// A title badge that needs attention: QUITTING.
  pub alert: Rgb,
  pub alert_bg: Rgb,
}

/// Group counts: `failed` if any failed, `up` if all are up, else `down`.
pub struct TaskListTheme {
  /// The `/` or `/…` after a group name.
  pub mark: Rgb,
  pub up: Rgb,
  pub down: Rgb,
  /// Ended with code 0: DONE, DOWN (0).
  pub done: Rgb,
  /// FAILED, DOWN (code), NOT READY.
  pub failed: Rgb,
}

impl Theme {
  pub fn dark() -> Self {
    let bg = Rgb(0x16, 0x1b, 0x22);
    let text = Rgb(0xd1, 0xd7, 0xe0);
    Theme {
      dark: true,
      palette: Palette {
        fg: text,
        bg,
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
      },
      block: BlockTheme {
        bg,
        modal_bg: Rgb(0x21, 0x28, 0x30),
        header_bg: Rgb(0x2b, 0x33, 0x3d),
        logo_bg: Rgb(0x27, 0x51, 0x93),
        logo_fg: Rgb(0xff, 0xff, 0xff),
        header_project: Rgb(0x56, 0xd4, 0xdd),
        shade: Rgb(0x00, 0x00, 0x00),
        shade_amount: 128,
        selected_bg: Rgb(0x42, 0x4a, 0x57),
        text,
        dim: Rgb(0x91, 0x98, 0xa1),
        key: Rgb(0xe3, 0xb3, 0x41),
        border: Rgb(0x3d, 0x44, 0x4d),
        border_focused: Rgb(0x6c, 0xae, 0xff),
        title: Rgb(0x91, 0x98, 0xa1),
        title_focused: Rgb(0xf0, 0xf6, 0xfc),
        alert: Rgb(0xff, 0xff, 0xff),
        alert_bg: Rgb(0xda, 0x36, 0x33),
      },
      tasks: TaskListTheme {
        mark: Rgb(0x91, 0x98, 0xa1),
        up: Rgb(0x56, 0xd3, 0x64),
        down: Rgb(0xa3, 0xac, 0xb8),
        done: Rgb(0x79, 0xc0, 0xff),
        failed: Rgb(0xff, 0xa1, 0x98),
      },
    }
  }

  pub fn light() -> Self {
    let bg = Rgb(0xf6, 0xf8, 0xfa);
    let text = Rgb(0x1f, 0x23, 0x28);
    Theme {
      dark: false,
      palette: Palette {
        fg: text,
        bg,
        ansi: [
          Rgb(0x24, 0x29, 0x2f),
          Rgb(0xcf, 0x22, 0x2e),
          Rgb(0x11, 0x63, 0x29),
          Rgb(0x9a, 0x67, 0x00),
          Rgb(0x09, 0x69, 0xda),
          Rgb(0x82, 0x50, 0xdf),
          Rgb(0x1b, 0x7c, 0x83),
          Rgb(0x6e, 0x77, 0x81),
          Rgb(0x57, 0x60, 0x6a),
          Rgb(0xa4, 0x0e, 0x26),
          Rgb(0x1a, 0x7f, 0x37),
          Rgb(0xbf, 0x87, 0x00),
          Rgb(0x21, 0x8b, 0xff),
          Rgb(0xa4, 0x75, 0xf9),
          Rgb(0x31, 0x92, 0xaa),
          Rgb(0x8c, 0x95, 0x9f),
        ],
      },
      block: BlockTheme {
        bg,
        modal_bg: Rgb(0xff, 0xff, 0xff),
        header_bg: Rgb(0xe1, 0xe7, 0xee),
        logo_bg: Rgb(0x4a, 0x80, 0xc8),
        logo_fg: Rgb(0xff, 0xff, 0xff),
        header_project: Rgb(0x1b, 0x7c, 0x83),
        shade: Rgb(0x1f, 0x23, 0x28),
        shade_amount: 77,
        selected_bg: Rgb(0xdd, 0xe4, 0xec),
        text,
        dim: Rgb(0x59, 0x63, 0x6e),
        key: Rgb(0x9a, 0x67, 0x00),
        border: Rgb(0xb8, 0xc2, 0xcc),
        border_focused: Rgb(0x05, 0x50, 0xae),
        title: Rgb(0x59, 0x63, 0x6e),
        title_focused: Rgb(0x1f, 0x23, 0x28),
        alert: Rgb(0xff, 0xff, 0xff),
        alert_bg: Rgb(0xcf, 0x22, 0x2e),
      },
      tasks: TaskListTheme {
        mark: Rgb(0x59, 0x63, 0x6e),
        up: Rgb(0x1a, 0x7f, 0x37),
        down: Rgb(0x59, 0x63, 0x6e),
        done: Rgb(0x09, 0x69, 0xda),
        failed: Rgb(0xd1, 0x24, 0x2f),
      },
    }
  }
}

/// The attrs of one drawn block, resolved from the theme.
#[derive(Clone, Copy)]
pub struct BlockStyle {
  pub border: Attrs,
  pub title: Attrs,
  pub text: Attrs,
  pub dim: Attrs,
  pub key: Attrs,
  pub alert: Attrs,
}

impl BlockTheme {
  pub fn panel(&self, focused: bool) -> BlockStyle {
    self.style(self.bg, focused)
  }

  /// A modal has the focus while it is open.
  pub fn modal(&self) -> BlockStyle {
    self.style(self.modal_bg, true)
  }

  fn style(&self, bg: Rgb, focused: bool) -> BlockStyle {
    let on_bg = |fg: Rgb| Attrs::default().bg(bg.into()).fg(fg.into());
    let (border, title) = if focused {
      (self.border_focused, self.title_focused)
    } else {
      (self.border, self.title)
    };
    BlockStyle {
      border: on_bg(border),
      title: on_bg(title).set_bold(focused),
      text: on_bg(self.text),
      dim: on_bg(self.dim),
      key: on_bg(self.key),
      alert: Attrs::default()
        .bg(self.alert_bg.into())
        .fg(self.alert.into())
        .set_bold(true),
    }
  }
}

impl BlockStyle {
  /// Draws the border and clears the inside.
  pub fn draw<'a>(&self, grid: &'a mut Grid, area: Rect) -> Block<'a> {
    grid.fill_area(area.inner(1), ' ', self.text);
    grid.block(area, BorderType::Plain, self.border)
  }
}
