pub const MAX_PARAMS: usize = 32;

/// A parsed CSI sequence: prefix / intermediate / final bytes plus numeric
/// parameters. Parameters are parsed into integers during scanning; colon
/// subparameters are stored flat with a marker bit.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Params {
  pub prefix: u8,
  pub inter: u8,
  pub final_: u8,
  values: [u32; MAX_PARAMS],
  given: u32,
  subs: u32,
  len: u8,
  invalid: bool,
}

impl Params {
  pub(crate) fn reset(&mut self) {
    *self = Params::default();
  }

  pub(crate) fn push_digit(&mut self, digit: u8) {
    if self.len == 0 {
      self.len = 1;
    }
    let i = self.len as usize - 1;
    self.values[i] = self.values[i]
      .saturating_mul(10)
      .saturating_add(digit as u32);
    self.given |= 1 << i;
  }

  pub(crate) fn next_param(&mut self, sub: bool) {
    if self.len == 0 {
      self.len = 1;
    }
    if (self.len as usize) < MAX_PARAMS {
      if sub {
        self.subs |= 1 << self.len;
      }
      self.len += 1;
    } else {
      self.invalid = true;
    }
  }

  pub(crate) fn set_prefix(&mut self, prefix: u8) {
    if self.len == 0 && self.prefix == 0 {
      self.prefix = prefix;
    } else {
      self.invalid = true;
    }
  }

  pub(crate) fn set_inter(&mut self, inter: u8) {
    if self.inter == 0 {
      self.inter = inter;
    } else {
      self.inter = 0xFF;
    }
  }

  pub fn len(&self) -> usize {
    self.len as usize
  }

  /// True when the sequence had no params, prefix, or intermediates.
  pub fn is_bare(&self) -> bool {
    self.len == 0 && self.prefix == 0 && self.inter == 0 && !self.invalid
  }

  pub fn invalid(&self) -> bool {
    self.invalid
  }

  /// The value of param `i`, or `None` when it is missing or empty.
  pub fn get_opt(&self, i: usize) -> Option<u32> {
    if i < self.len as usize && self.given & (1 << i) != 0 {
      Some(self.values[i])
    } else {
      None
    }
  }

  pub fn get(&self, i: usize, default: u32) -> u32 {
    self.get_opt(i).unwrap_or(default)
  }

  pub fn get16(&self, i: usize, default: u16) -> u16 {
    self.get(i, default as u32).min(u16::MAX as u32) as u16
  }

  /// True when param `i` was attached to the previous one with a colon.
  pub fn is_sub(&self, i: usize) -> bool {
    i < self.len as usize && self.subs & (1 << i) != 0
  }

  /// Writes the bytes that, scanned after `ESC [`, rebuild these params
  /// for a CSI still waiting for its final byte.
  pub(crate) fn write_unfinished(&self, out: &mut Vec<u8>) {
    if self.prefix != 0 {
      out.push(self.prefix);
    }
    for i in 0..self.len as usize {
      if i > 0 {
        out.push(if self.subs & (1 << i) != 0 {
          b':'
        } else {
          b';'
        });
      }
      if self.given & (1 << i) != 0 {
        out.extend_from_slice(self.values[i].to_string().as_bytes());
      }
    }
    // Invalid params always have a param or a prefix, so one more prefix
    // byte marks them invalid and changes nothing else.
    if self.invalid {
      out.push(b'?');
    }
    match self.inter {
      0 => (),
      // More than one intermediate.
      0xFF => out.extend_from_slice(b"  "),
      inter => out.push(inter),
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  fn build(bytes: &[u8]) -> Params {
    let mut p = Params::default();
    for b in bytes {
      match b {
        b'0'..=b'9' => p.push_digit(b - b'0'),
        b';' => p.next_param(false),
        b':' => p.next_param(true),
        b'<' | b'=' | b'>' | b'?' => p.set_prefix(*b),
        _ => panic!("bad test input"),
      }
    }
    p
  }

  #[test]
  fn basics() {
    let p = build(b"");
    assert_eq!(p.len(), 0);
    assert_eq!(p.get(0, 7), 7);
    assert!(p.is_bare());

    let p = build(b"5;;38");
    assert_eq!(p.len(), 3);
    assert_eq!(p.get(0, 0), 5);
    assert_eq!(p.get_opt(1), None);
    assert_eq!(p.get(1, 1), 1);
    assert_eq!(p.get(2, 0), 38);

    let p = build(b"?1049");
    assert_eq!(p.prefix, b'?');
    assert_eq!(p.get(0, 0), 1049);

    let p = build(b"38:5:196");
    assert!(!p.is_sub(0));
    assert!(p.is_sub(1));
    assert!(p.is_sub(2));
    assert_eq!(p.get(2, 0), 196);

    let p = build(b"99999999999999");
    assert_eq!(p.get(0, 0), u32::MAX);
  }
}
