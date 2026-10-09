#[macro_export]
macro_rules! color {
  ($hex:literal) => {{
    const COLOR: $crate::term::Color = {
      let bytes = $hex.as_bytes();
      assert!(bytes.len() == 7, "expected #RRGGBB format");
      assert!(bytes[0] == b'#', "expected #RRGGBB format");

      const fn hex_digit(b: u8) -> u8 {
        match b {
          b'0'..=b'9' => b - b'0',
          b'a'..=b'f' => b - b'a' + 10,
          b'A'..=b'F' => b - b'A' + 10,
          _ => panic!("invalid hex digit"),
        }
      }

      let r = hex_digit(bytes[1]) * 16 + hex_digit(bytes[2]);
      let g = hex_digit(bytes[3]) * 16 + hex_digit(bytes[4]);
      let b = hex_digit(bytes[5]) * 16 + hex_digit(bytes[6]);
      $crate::term::Color::Rgb(r, g, b)
    };
    COLOR
  }};
}

/// Represents a foreground or background color for cells.
#[derive(Eq, PartialEq, Debug, Copy, Clone)]
pub enum Color {
  /// The default terminal color.
  Default,

  /// An indexed terminal color.
  Idx(u8),

  /// An RGB terminal color. The parameters are (red, green, blue).
  Rgb(u8, u8, u8),
}

#[allow(dead_code)]
impl Color {
  pub const BLACK: Self = Color::Idx(0);
  pub const RED: Self = Color::Idx(1);
  pub const GREEN: Self = Color::Idx(2);
  pub const YELLOW: Self = Color::Idx(3);
  pub const BLUE: Self = Color::Idx(4);
  pub const MAGENTA: Self = Color::Idx(5);
  pub const CYAN: Self = Color::Idx(6);
  pub const WHITE: Self = Color::Idx(7);

  pub const BRIGHT_BLACK: Self = Color::Idx(8);
  pub const BRIGHT_RED: Self = Color::Idx(9);
  pub const BRIGHT_GREEN: Self = Color::Idx(10);
  pub const BRIGHT_YELLOW: Self = Color::Idx(11);
  pub const BRIGHT_BLUE: Self = Color::Idx(12);
  pub const BRIGHT_MAGENTA: Self = Color::Idx(13);
  pub const BRIGHT_CYAN: Self = Color::Idx(14);
  pub const BRIGHT_WHITE: Self = Color::Idx(15);
}

impl Color {
  /// Only RGB colors move; the others have no value to move from.
  pub fn blend(self, to: Rgb, amount: u8) -> Self {
    match self {
      Color::Rgb(r, g, b) => Rgb(r, g, b).blend(to, amount).into(),
      Color::Default | Color::Idx(_) => self,
    }
  }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Rgb(pub u8, pub u8, pub u8);

impl Rgb {
  /// Moves `amount`/255 of the way to `to`.
  pub fn blend(self, to: Rgb, amount: u8) -> Rgb {
    let amount = amount as u16;
    let mix = |a: u8, b: u8| {
      ((a as u16 * (255 - amount) + b as u16 * amount) / 255) as u8
    };
    Rgb(mix(self.0, to.0), mix(self.1, to.1), mix(self.2, to.2))
  }
}

impl From<Rgb> for Color {
  fn from(Rgb(r, g, b): Rgb) -> Self {
    Color::Rgb(r, g, b)
  }
}

impl Default for Color {
  fn default() -> Self {
    Self::Default
  }
}
