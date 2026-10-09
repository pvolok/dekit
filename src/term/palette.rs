use super::color::{Color, Rgb};

/// How a screen's colors become RGB: the default colors and the 16 ANSI
/// ones; 16-231 are the standard cube and 232-255 the grays.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Palette {
  pub fg: Rgb,
  pub bg: Rgb,
  pub ansi: [Rgb; 16],
}

impl Palette {
  pub fn fg(&self, color: Color) -> Rgb {
    self.resolve(color, self.fg)
  }

  pub fn bg(&self, color: Color) -> Rgb {
    self.resolve(color, self.bg)
  }

  fn resolve(&self, color: Color, default: Rgb) -> Rgb {
    match color {
      Color::Default => default,
      Color::Idx(i) => self.idx(i),
      Color::Rgb(r, g, b) => Rgb(r, g, b),
    }
  }

  pub fn idx(&self, i: u8) -> Rgb {
    const LEVELS: [u8; 6] = [0, 95, 135, 175, 215, 255];
    match i {
      0..=15 => self.ansi[i as usize],
      16..=231 => {
        let i = (i - 16) as usize;
        Rgb(LEVELS[i / 36], LEVELS[i / 6 % 6], LEVELS[i % 6])
      }
      232..=255 => {
        let v = 8 + (i - 232) * 10;
        Rgb(v, v, v)
      }
    }
  }
}
