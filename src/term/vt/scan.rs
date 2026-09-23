use super::params::Params;

/// A body of an unterminated OSC/DCS longer than this aborts the sequence.
const MAX_BODY_BYTES: usize = 64 * 1024;

/// One scanned item. Borrowed slices point into the fed chunk or the
/// scanner's internal buffers.
#[derive(Debug, PartialEq, Eq)]
pub enum Seq<'a> {
  /// Printable text (valid UTF-8).
  Text(&'a str),
  /// A C0 control byte (also DEL, and 0x8D for legacy C1 RI).
  Ctl(u8),
  /// `ESC final` or an nF sequence `ESC inter final`. `inter` is 0 when
  /// absent and 0xFF when there was more than one intermediate byte.
  Esc {
    inter: u8,
    final_: u8,
  },
  Csi(&'a Params),
  Osc(&'a [u8]),
  Dcs(&'a [u8]),
  /// Input mode: ESC followed by a plain character (Alt chord).
  EscChar(char),
  /// Input mode: `ESC O final`.
  Ss3(u8),
  /// Input mode: X10 mouse report `ESC [ M b x y` (raw bytes).
  X10Mouse(u8, u8, u8),
}

/// Which direction of the protocol is being scanned. Output is what programs
/// write to a terminal; Input is what a terminal sends for keys and mouse.
/// The framing differs after ESC: input has SS3 and Alt chords, and a bare
/// `ESC [ M` starts an X10 mouse report instead of a CSI final.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScanMode {
  Output,
  Input,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum State {
  Ground,
  Utf8,
  Esc,
  EscInter,
  EscUtf8,
  Csi,
  Osc,
  OscEsc,
  Dcs,
  DcsEsc,
  Skip,
  SkipEsc,
  Ss3,
  X10Mouse,
}

/// Incremental scanner for terminal byte streams. State carries across
/// `feed` calls, so chunk boundaries never require re-scanning.
#[derive(Clone, Debug)]
pub struct Scanner {
  mode: ScanMode,
  state: State,
  params: Params,
  esc_inter: u8,
  body: Vec<u8>,
  utf8: [u8; 4],
  utf8_len: u8,
  utf8_need: u8,
  x10: [u8; 2],
  x10_len: u8,
}

impl Default for Scanner {
  fn default() -> Self {
    Scanner::new(ScanMode::Output)
  }
}

fn utf8_need(first_byte: u8) -> usize {
  match first_byte {
    0xC2..=0xDF => 2,
    0xE0..=0xEF => 3,
    0xF0..=0xF4 => 4,
    _ => 0,
  }
}

impl Scanner {
  pub fn new(mode: ScanMode) -> Self {
    Scanner {
      mode,
      state: State::Ground,
      params: Params::default(),
      esc_inter: 0,
      body: Vec::new(),
      utf8: [0; 4],
      utf8_len: 0,
      utf8_need: 0,
      x10: [0; 2],
      x10_len: 0,
    }
  }

  /// Bytes that put a fresh scanner into this scanner's state: the
  /// unfinished sequence, rebuilt from what was parsed of it so far.
  /// Empty at rest.
  pub fn pending(&self) -> Vec<u8> {
    let mut out = Vec::new();
    match self.state {
      State::Ground => (),
      State::Utf8 => {
        out.extend_from_slice(&self.utf8[..self.utf8_len as usize]);
      }
      State::Esc => out.push(0x1B),
      State::EscInter => {
        out.push(0x1B);
        match self.esc_inter {
          // More than one intermediate.
          0xFF => out.extend_from_slice(b"  "),
          inter => out.push(inter),
        }
      }
      State::EscUtf8 => {
        out.push(0x1B);
        out.extend_from_slice(&self.utf8[..self.utf8_len as usize]);
      }
      State::Csi => {
        out.extend_from_slice(b"\x1b[");
        self.params.write_unfinished(&mut out);
      }
      State::Osc => {
        out.extend_from_slice(b"\x1b]");
        out.extend_from_slice(&self.body);
      }
      State::OscEsc => {
        out.extend_from_slice(b"\x1b]");
        out.extend_from_slice(&self.body);
        out.push(0x1B);
      }
      State::Dcs => {
        out.extend_from_slice(b"\x1bP");
        out.extend_from_slice(&self.body);
      }
      State::DcsEsc => {
        out.extend_from_slice(b"\x1bP");
        out.extend_from_slice(&self.body);
        out.push(0x1B);
      }
      State::Skip => {
        out.extend_from_slice(b"\x1bX");
        out.extend_from_slice(&self.body);
      }
      State::SkipEsc => {
        out.extend_from_slice(b"\x1bX");
        out.extend_from_slice(&self.body);
        out.push(0x1B);
      }
      State::Ss3 => out.extend_from_slice(b"\x1bO"),
      State::X10Mouse => {
        out.extend_from_slice(b"\x1b[M");
        out.extend_from_slice(&self.x10[..self.x10_len as usize]);
      }
    }
    out
  }

  /// Input mode: resolve a trailing lone ESC into an Esc key press. Called
  /// when a read chunk ends and no more input is immediately available.
  pub fn flush<F: for<'a> FnMut(Seq<'a>)>(&mut self, mut f: F) {
    if self.state == State::Esc {
      self.state = State::Ground;
      f(Seq::Ctl(0x1B));
    }
  }

  /// True while a lone ESC is waiting to see whether more bytes follow.
  pub fn esc_pending(&self) -> bool {
    self.state == State::Esc
  }

  pub fn feed<F: for<'a> FnMut(Seq<'a>)>(&mut self, input: &[u8], mut f: F) {
    let mut i = 0;
    while i < input.len() {
      let b = input[i];
      match self.state {
        State::Ground => {
          if b == 0x1B {
            self.state = State::Esc;
            i += 1;
            continue;
          }
          if (0x20..=0x7E).contains(&b) || b >= 0xC2 {
            let start = i;
            let mut end = i;
            while end < input.len() {
              let b = input[end];
              if (0x20..=0x7E).contains(&b) {
                end += 1;
                continue;
              }
              if b >= 0xC2 {
                let need = utf8_need(b);
                if need == 0 || end + need > input.len() {
                  break;
                }
                if std::str::from_utf8(&input[end..end + need]).is_err() {
                  break;
                }
                end += need;
                continue;
              }
              break;
            }
            if end > start {
              // Safety: the range is printable ASCII plus individually
              // validated UTF-8 sequences.
              let text =
                unsafe { std::str::from_utf8_unchecked(&input[start..end]) };
              f(Seq::Text(text));
              i = end;
              continue;
            }
            // A lone byte >= 0xC2 that did not form a run: either a
            // partial char at the chunk end or invalid UTF-8.
            let need = utf8_need(b);
            if need > 0
              && i + need > input.len()
              && input[i + 1..].iter().all(|b| (0x80..=0xBF).contains(b))
            {
              let avail = input.len() - i;
              self.utf8[..avail].copy_from_slice(&input[i..]);
              self.utf8_len = avail as u8;
              self.utf8_need = need as u8;
              self.state = State::Utf8;
              i = input.len();
            } else {
              i += 1;
            }
            continue;
          }
          if b < 0x20 || b == 0x7F || b == 0x8D {
            f(Seq::Ctl(b));
          }
          i += 1;
        }
        State::Utf8 | State::EscUtf8 => {
          if !(0x80..=0xBF).contains(&b) {
            // Not a continuation byte: the char is invalid. Only its
            // lead byte is dropped; the stashed continuation bytes are
            // reprocessed like standalone bytes (0x8D is legacy RI),
            // and the current byte restarts from the ground state.
            let len = self.utf8_len as usize;
            let stash = self.utf8;
            self.utf8_len = 0;
            self.state = State::Ground;
            for sb in &stash[1..len] {
              if *sb == 0x8D {
                f(Seq::Ctl(0x8D));
              }
            }
            continue;
          }
          self.utf8[self.utf8_len as usize] = b;
          self.utf8_len += 1;
          i += 1;
          if self.utf8_len == self.utf8_need {
            let alt = self.state == State::EscUtf8;
            let len = self.utf8_len as usize;
            self.utf8_len = 0;
            self.state = State::Ground;
            match std::str::from_utf8(&self.utf8[..len]) {
              Ok(s) => {
                if alt {
                  let c = s.chars().next().unwrap();
                  f(Seq::EscChar(c));
                } else {
                  f(Seq::Text(s));
                }
              }
              Err(_) => {
                // Too long or otherwise invalid: drop the lead byte,
                // reprocess the continuations as standalone bytes.
                let stash = self.utf8;
                for sb in &stash[1..len] {
                  if *sb == 0x8D {
                    f(Seq::Ctl(0x8D));
                  }
                }
              }
            }
          }
        }
        State::Esc => {
          i += 1;
          match b {
            b'[' => {
              self.params.reset();
              self.state = State::Csi;
            }
            0x1B => match self.mode {
              // Output: a second ESC restarts the sequence.
              ScanMode::Output => (),
              // Input: ESC ESC is one Esc key press.
              ScanMode::Input => {
                self.state = State::Ground;
                f(Seq::Ctl(0x1B));
              }
            },
            b'O' if self.mode == ScanMode::Input => {
              self.state = State::Ss3;
            }
            _ => match self.mode {
              ScanMode::Output => match b {
                0x20..=0x2F => {
                  self.esc_inter = b;
                  self.state = State::EscInter;
                }
                b']' => {
                  self.body.clear();
                  self.state = State::Osc;
                }
                b'P' => {
                  self.body.clear();
                  self.state = State::Dcs;
                }
                b'X' | b'^' | b'_' => {
                  self.body.clear();
                  self.state = State::Skip;
                }
                0x30..=0x7E => {
                  self.state = State::Ground;
                  f(Seq::Esc {
                    inter: 0,
                    final_: b,
                  });
                }
                _ => {
                  self.state = State::Ground;
                  if b < 0x20 || b == 0x7F {
                    f(Seq::Ctl(b));
                  }
                }
              },
              ScanMode::Input => {
                if b < 0x80 {
                  self.state = State::Ground;
                  f(Seq::EscChar(b as char));
                } else {
                  let need = utf8_need(b);
                  if need > 0 {
                    self.utf8[0] = b;
                    self.utf8_len = 1;
                    self.utf8_need = need as u8;
                    self.state = State::EscUtf8;
                  } else {
                    self.state = State::Ground;
                  }
                }
              }
            },
          }
        }
        State::EscInter => {
          i += 1;
          match b {
            0x20..=0x2F => self.params_inter_extra(),
            0x30..=0x7E => {
              let inter = self.esc_inter;
              self.state = State::Ground;
              f(Seq::Esc { inter, final_: b });
            }
            _ => {
              self.state = State::Ground;
              if b < 0x20 || b == 0x7F {
                f(Seq::Ctl(b));
              }
            }
          }
        }
        State::Csi => {
          i += 1;
          match b {
            b'0'..=b'9' => self.params.push_digit(b - b'0'),
            b';' => self.params.next_param(false),
            b':' => self.params.next_param(true),
            b'<' | b'=' | b'>' | b'?' => self.params.set_prefix(b),
            0x20..=0x2F => self.params.set_inter(b),
            b'M' if self.mode == ScanMode::Input && self.params.is_bare() => {
              self.x10_len = 0;
              self.state = State::X10Mouse;
            }
            0x40..=0x7E => {
              self.params.final_ = b;
              self.state = State::Ground;
              f(Seq::Csi(&self.params));
            }
            0x1B => self.state = State::Esc,
            0x00..=0x1A | 0x1C..=0x1F => f(Seq::Ctl(b)),
            _ => {
              // 0x7F is ignored inside a sequence; anything else
              // aborts it.
              if b != 0x7F {
                self.state = State::Ground;
              }
            }
          }
        }
        State::Osc => {
          if b == 0x07 || b == 0x9C {
            self.state = State::Ground;
            f(Seq::Osc(&self.body));
          } else if b == 0x1B {
            self.state = State::OscEsc;
          } else {
            self.push_body(b);
          }
          i += 1;
        }
        State::OscEsc => {
          if b == b'\\' {
            self.state = State::Ground;
            f(Seq::Osc(&self.body));
            i += 1;
          } else {
            // The ESC was part of the body; reprocess this byte.
            self.state = State::Osc;
            self.push_body(0x1B);
          }
        }
        State::Dcs => {
          if b == 0x1B {
            self.state = State::DcsEsc;
          } else {
            self.push_body(b);
          }
          i += 1;
        }
        State::DcsEsc => {
          if b == b'\\' {
            self.state = State::Ground;
            f(Seq::Dcs(&self.body));
            i += 1;
          } else {
            self.state = State::Dcs;
            self.push_body(0x1B);
          }
        }
        State::Skip => {
          if b == 0x1B {
            self.state = State::SkipEsc;
          } else {
            self.push_body(b);
          }
          i += 1;
        }
        State::SkipEsc => {
          if b == b'\\' {
            self.state = State::Ground;
            self.body.clear();
            i += 1;
          } else {
            self.state = State::Skip;
            self.push_body(0x1B);
          }
        }
        State::Ss3 => {
          i += 1;
          self.state = State::Ground;
          f(Seq::Ss3(b));
        }
        State::X10Mouse => {
          i += 1;
          if self.x10_len < 2 {
            self.x10[self.x10_len as usize] = b;
            self.x10_len += 1;
          } else {
            self.state = State::Ground;
            f(Seq::X10Mouse(self.x10[0], self.x10[1], b));
          }
        }
      }
    }
  }

  fn params_inter_extra(&mut self) {
    if self.esc_inter != 0 {
      self.esc_inter = 0xFF;
    }
  }

  fn push_body(&mut self, b: u8) {
    if self.body.len() >= MAX_BODY_BYTES {
      self.body.clear();
      self.state = State::Ground;
    } else {
      self.body.push(b);
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[derive(Debug, PartialEq, Eq)]
  enum Item {
    Text(String),
    Ctl(u8),
    Esc(u8, u8),
    Csi(String),
    Osc(Vec<u8>),
    Dcs(Vec<u8>),
    EscChar(char),
    Ss3(u8),
    X10(u8, u8, u8),
  }

  fn csi_repr(p: &Params) -> String {
    let mut s = String::new();
    if p.invalid() {
      s.push('!');
    }
    if p.prefix != 0 {
      s.push(p.prefix as char);
    }
    for i in 0..p.len() {
      if i > 0 {
        s.push(if p.is_sub(i) { ':' } else { ';' });
      }
      match p.get_opt(i) {
        Some(v) => s.push_str(&v.to_string()),
        None => (),
      }
    }
    if p.inter != 0 {
      s.push(p.inter as char);
    }
    s.push(p.final_ as char);
    s
  }

  fn collect(scanner: &mut Scanner, chunks: &[&[u8]]) -> Vec<Item> {
    let mut items = Vec::new();
    for chunk in chunks {
      scanner.feed(chunk, |seq| {
        items.push(match seq {
          Seq::Text(t) => Item::Text(t.to_string()),
          Seq::Ctl(b) => Item::Ctl(b),
          Seq::Esc { inter, final_ } => Item::Esc(inter, final_),
          Seq::Csi(p) => Item::Csi(csi_repr(p)),
          Seq::Osc(d) => Item::Osc(d.to_vec()),
          Seq::Dcs(d) => Item::Dcs(d.to_vec()),
          Seq::EscChar(c) => Item::EscChar(c),
          Seq::Ss3(b) => Item::Ss3(b),
          Seq::X10Mouse(a, b, c) => Item::X10(a, b, c),
        });
      });
    }
    items
  }

  fn scan_output(chunks: &[&[u8]]) -> Vec<Item> {
    collect(&mut Scanner::new(ScanMode::Output), chunks)
  }

  fn scan_input(chunks: &[&[u8]]) -> Vec<Item> {
    collect(&mut Scanner::new(ScanMode::Input), chunks)
  }

  #[test]
  fn text_and_controls() {
    assert_eq!(
      scan_output(&[b"ab\ncd"]),
      vec![
        Item::Text("ab".into()),
        Item::Ctl(b'\n'),
        Item::Text("cd".into()),
      ]
    );
    assert_eq!(
      scan_output(&["ab测c".as_bytes()]),
      vec![Item::Text("ab测c".into())]
    );
  }

  #[test]
  fn utf8_split_across_chunks() {
    let bytes = "测".as_bytes();
    assert_eq!(
      scan_output(&[&bytes[..1], &bytes[1..]]),
      vec![Item::Text("测".into())]
    );
    assert_eq!(
      scan_output(&[&bytes[..2], &bytes[2..]]),
      vec![Item::Text("测".into())]
    );
    // Aborted partial char: the next byte is not a continuation.
    assert_eq!(
      scan_output(&[&bytes[..1], b"x"]),
      vec![Item::Text("x".into())]
    );
  }

  #[test]
  fn invalid_bytes_skipped() {
    assert_eq!(
      scan_output(&[b"a\x80\xffb"]),
      vec![Item::Text("a".into()), Item::Text("b".into())]
    );
    // 0x8D is legacy C1 RI.
    assert_eq!(
      scan_output(&[b"a\x8db"]),
      vec![
        Item::Text("a".into()),
        Item::Ctl(0x8D),
        Item::Text("b".into()),
      ]
    );
  }

  #[test]
  fn csi() {
    assert_eq!(scan_output(&[b"\x1b[m"]), vec![Item::Csi("m".into())]);
    assert_eq!(
      scan_output(&[b"\x1b[1;31m"]),
      vec![Item::Csi("1;31m".into())]
    );
    assert_eq!(
      scan_output(&[b"\x1b[?1049h"]),
      vec![Item::Csi("?1049h".into())]
    );
    assert_eq!(
      scan_output(&[b"\x1b[38:5:196m"]),
      vec![Item::Csi("38:5:196m".into())]
    );
    assert_eq!(scan_output(&[b"\x1b[ q"]), vec![Item::Csi(" q".into())]);
    assert_eq!(scan_output(&[b"\x1b[;5H"]), vec![Item::Csi(";5H".into())]);
    // Split anywhere.
    assert_eq!(
      scan_output(&[b"\x1b", b"[", b"3", b"8;5;1", b"m"]),
      vec![Item::Csi("38;5;1m".into())]
    );
    // C0 inside a CSI executes without aborting it.
    assert_eq!(
      scan_output(&[b"\x1b[3\n1m"]),
      vec![Item::Ctl(b'\n'), Item::Csi("31m".into())]
    );
    // ESC inside a CSI restarts the sequence.
    assert_eq!(
      scan_output(&[b"\x1b[3\x1b[31m"]),
      vec![Item::Csi("31m".into())]
    );
  }

  #[test]
  fn esc_and_nf() {
    assert_eq!(scan_output(&[b"\x1b7"]), vec![Item::Esc(0, b'7')]);
    assert_eq!(scan_output(&[b"\x1b(B"]), vec![Item::Esc(b'(', b'B')]);
    assert_eq!(scan_output(&[b"\x1b(", b"0"]), vec![Item::Esc(b'(', b'0')]);
    // Double ESC restarts.
    assert_eq!(scan_output(&[b"\x1b\x1b7"]), vec![Item::Esc(0, b'7')]);
  }

  #[test]
  fn osc() {
    assert_eq!(
      scan_output(&[b"\x1b]0;title\x07"]),
      vec![Item::Osc(b"0;title".to_vec())]
    );
    assert_eq!(
      scan_output(&[b"\x1b]0;title\x1b\\"]),
      vec![Item::Osc(b"0;title".to_vec())]
    );
    // ESC inside the body that is not a terminator stays in the body.
    assert_eq!(
      scan_output(&[b"\x1b]0;a\x1bb\x07"]),
      vec![Item::Osc(b"0;a\x1bb".to_vec())]
    );
    // Split across chunks.
    assert_eq!(
      scan_output(&[b"\x1b]0;ti", b"tle\x1b", b"\\x"]),
      vec![Item::Osc(b"0;title".to_vec()), Item::Text("x".into())]
    );
  }

  #[test]
  fn dcs_and_skip() {
    assert_eq!(
      scan_output(&[b"\x1bPdata\x1b\\x"]),
      vec![Item::Dcs(b"data".to_vec()), Item::Text("x".into())]
    );
    // SOS/PM/APC bodies are discarded.
    assert_eq!(
      scan_output(&[b"\x1b_hidden\x1b\\x"]),
      vec![Item::Text("x".into())]
    );
  }

  #[test]
  fn body_flood_aborts() {
    let mut scanner = Scanner::new(ScanMode::Output);
    let mut items = Vec::new();
    scanner.feed(b"\x1b]0;", |_| panic!("no items yet"));
    let flood = vec![b'x'; MAX_BODY_BYTES + 10];
    scanner.feed(&flood, |seq| items.push(matches!(seq, Seq::Text(_))));
    // After the cap the scanner returns to ground; the tail renders as text.
    assert!(!items.is_empty());
    assert!(items.iter().all(|t| *t));
  }

  #[test]
  fn input_mode() {
    assert_eq!(scan_input(&[b"\x1bOA"]), vec![Item::Ss3(b'A')]);
    assert_eq!(scan_input(&[b"\x1bx"]), vec![Item::EscChar('x')]);
    assert_eq!(scan_input(&[b"\x1b\r"]), vec![Item::EscChar('\r')]);
    assert_eq!(scan_input(&["\x1bé".as_bytes()]), vec![Item::EscChar('é')]);
    assert_eq!(scan_input(&[b"\x1b\x1b"]), vec![Item::Ctl(0x1B)]);
    assert_eq!(
      scan_input(&[b"\x1b[M !\""]),
      vec![Item::X10(b' ', b'!', b'"')]
    );
    // SGR mouse goes through the params path.
    assert_eq!(
      scan_input(&[b"\x1b[<0;5;10M"]),
      vec![Item::Csi("<0;5;10M".into())]
    );
    // Rxvt mouse: params then final M.
    assert_eq!(
      scan_input(&[b"\x1b[32;5;10M"]),
      vec![Item::Csi("32;5;10M".into())]
    );

    // A lone ESC resolves on flush.
    let mut scanner = Scanner::new(ScanMode::Input);
    let mut items = Vec::new();
    scanner.feed(b"\x1b", |_| panic!("pending"));
    scanner.flush(|seq| items.push(matches!(seq, Seq::Ctl(0x1B))));
    assert_eq!(items, vec![true]);
    // Flush with no pending ESC emits nothing.
    scanner.flush(|_| panic!("nothing pending"));
  }

  /// Text runs split wherever the chunks do; only their content matters.
  fn merge_text(items: Vec<Item>) -> Vec<Item> {
    let mut merged: Vec<Item> = Vec::new();
    for item in items {
      match (merged.last_mut(), item) {
        (Some(Item::Text(prev)), Item::Text(text)) => prev.push_str(&text),
        (_, item) => merged.push(item),
      }
    }
    merged
  }

  /// At every split point, a fresh scanner fed `pending()` of the head
  /// continues the tail exactly like the original scanner would.
  fn resumes_anywhere(mode: ScanMode, input: &[u8]) {
    let whole = merge_text(collect(&mut Scanner::new(mode), &[input]));
    for split in 0..=input.len() {
      let mut head = Scanner::new(mode);
      let mut items = collect(&mut head, &[&input[..split]]);
      let mut resumed = Scanner::new(mode);
      let replayed = collect(&mut resumed, &[&head.pending()]);
      assert_eq!(replayed, vec![], "pending emitted at {split} of {input:?}");
      items.extend(collect(&mut resumed, &[&input[split..]]));
      assert_eq!(merge_text(items), whole, "split at {split} of {input:?}");
    }
  }

  #[test]
  fn pending_rebuilds_the_state() {
    let too_many = format!(
      "\x1b[{}m",
      (1..=40)
        .map(|n| n.to_string())
        .collect::<Vec<_>>()
        .join(";")
    );
    for input in [
      &b"\x1b[?25;38:5:196;;7 q"[..],
      too_many.as_bytes(),
      b"\x1b[1?2m",
      b"\x1b[?>1m",
      b"\x1b[3\n1m",
      b"\x1b[99999999999m",
      b"\x1b[1 !m",
      b"\x1b(B\x1b !B",
      b"\x1b\x1b7",
      b"\x1b]0;a\x1bb\x1b\x1bc\x07x",
      b"\x1b]0;title\x1b\\",
      b"\x1bPdata\x1bx\x1b\\",
      b"\x1b_hid\x1bden\x1b\\x",
      "a测b\x1b[1m".as_bytes(),
    ] {
      resumes_anywhere(ScanMode::Output, input);
    }
    for input in [
      &b"\x1bOA"[..],
      b"\x1b[M !\"",
      "\x1bé".as_bytes(),
      b"\x1b[<0;5;10M",
    ] {
      resumes_anywhere(ScanMode::Input, input);
    }
  }

  #[test]
  fn pending_stays_small_in_an_endless_sequence() {
    let mut scanner = Scanner::new(ScanMode::Output);
    scanner.feed(b"\x1b[", |_| ());
    for _ in 0..10_000 {
      scanner.feed(b"12345\n", |_| ());
    }
    assert!(scanner.pending().len() < 16, "{:?}", scanner.pending());
  }
}
