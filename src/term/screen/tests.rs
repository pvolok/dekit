use super::*;
use crate::term::{
  Color,
  modes::{MouseProtocolEncoding, MouseProtocolMode},
  style::Underline,
};

fn screen(width: u16, height: u16) -> Screen {
  Screen::new(Size { height, width }, 100)
}

fn feed(screen: &mut Screen, bytes: &[u8]) -> Vec<VtEvent> {
  screen.process(bytes);
  let mut events = Vec::new();
  screen.take_events(&mut events);
  events
}

fn row_text(screen: &Screen, row: u16) -> String {
  let frame = screen.frame();
  let text = frame.row_text(row, 0, frame.size.width);
  text.trim_end().to_string()
}

fn style_at(screen: &Screen, row: u16, col: u16) -> Style {
  let frame = screen.frame();
  frame.style(frame.cell(row, col).unwrap())
}

fn reply_bytes(events: &[VtEvent]) -> Vec<Vec<u8>> {
  events
    .iter()
    .filter_map(|e| match e {
      VtEvent::Reply(bytes) => Some(bytes.clone()),
      _ => None,
    })
    .collect()
}

#[test]
fn text_wrap_and_controls() {
  let mut s = screen(5, 3);
  feed(&mut s, b"hello, world");
  assert_eq!(row_text(&s, 0), "hello");
  assert_eq!(row_text(&s, 1), ", wor");
  assert_eq!(row_text(&s, 2), "ld");
  assert_eq!(s.cursor_position(), (2, 2));

  let mut s = screen(11, 3);
  feed(&mut s, b"ab\r\ncd\x08X\ttab");
  assert_eq!(row_text(&s, 0), "ab");
  assert_eq!(row_text(&s, 1), "cX      tab");
}

#[test]
fn wide_chars() {
  let mut s = screen(10, 2);
  feed(&mut s, "a\u{6d4b}b".as_bytes());
  assert_eq!(row_text(&s, 0), "a\u{6d4b}b");
  assert_eq!(s.cursor_position(), (0, 4));
  // Overwrite the left half of the wide char.
  feed(&mut s, b"\x1b[1;2HX");
  assert_eq!(row_text(&s, 0), "aX b");
  // A wide char that does not fit at the end wraps whole, and the line
  // goes on: its text joins, and a wider screen rejoins it.
  let mut s = screen(4, 2);
  feed(&mut s, "abc\u{6d4b}".as_bytes());
  assert_eq!(row_text(&s, 0), "abc");
  assert_eq!(row_text(&s, 1), "\u{6d4b}");
  assert_eq!(s.open_log_line().as_deref(), Some("abc\u{6d4b}"));
  s.set_size(2, 8);
  assert_eq!(row_text(&s, 0), "abc\u{6d4b}");
  assert_eq!(s.cursor_position(), (0, 5));
  // A glyph on the last column stays when a wide char wraps from it.
  let mut s = screen(4, 2);
  feed(&mut s, "abcd\x1b[1;4H\u{6d4b}".as_bytes());
  assert_eq!(row_text(&s, 0), "abcd");
  assert_eq!(row_text(&s, 1), "\u{6d4b}");
  assert_eq!(s.open_log_line().as_deref(), Some("abcd\u{6d4b}"));
}

#[test]
fn grapheme_clusters() {
  let mut s = screen(10, 2);
  // Combining mark joins; VS16 widens; a flag is one cell; ZWJ joins.
  feed(&mut s, "e\u{301}x".as_bytes());
  assert_eq!(s.cursor_position(), (0, 2));
  let frame = s.frame();
  let mut buf = [0u8; 4];
  assert_eq!(frame.text(frame.cell(0, 0).unwrap(), &mut buf), "e\u{301}");
  feed(&mut s, "\r\n\u{2764}\u{FE0F}y".as_bytes());
  assert_eq!(s.cursor_position(), (1, 3));
  let frame = s.frame();
  assert!(frame.cell(1, 0).unwrap().is_wide());
  assert!(frame.cell(1, 1).unwrap().is_continuation());
  assert_eq!(frame.text(frame.cell(1, 2).unwrap(), &mut buf), "y");

  let mut s = screen(10, 1);
  feed(&mut s, "\u{1F1FA}\u{1F1F8}!".as_bytes());
  assert_eq!(s.cursor_position(), (0, 3));
  let mut s = screen(10, 1);
  feed(
    &mut s,
    "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}!".as_bytes(),
  );
  assert_eq!(s.cursor_position(), (0, 3));
  let frame = s.frame();
  assert_eq!(
    frame.text(frame.cell(0, 0).unwrap(), &mut buf),
    "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}"
  );
}

// Feeding byte-at-a-time never enters the ASCII blit in text_run (every
// Text item is a single char), while a one-shot feed blits maximally.
// Both must produce identical screens, including at wide-char overwrite
// boundaries and across wraps.
#[test]
fn blit_matches_per_char() {
  let width = 10;
  for start_col in 0..width {
    for run in ["XY", "XYZXYZXYZXYZXYZ", "x"] {
      let paint = "\u{6d4b}\u{8bd5}\u{5bbd}".as_bytes();
      let mut blit = screen(width, 3);
      let mut per_char = screen(width, 3);
      for s in [&mut blit, &mut per_char] {
        feed(s, paint);
        feed(s, format!("\x1b[1;{}H", start_col + 1).as_bytes());
      }

      feed(&mut blit, run.as_bytes());
      for b in run.as_bytes() {
        feed(&mut per_char, &[*b]);
      }

      for row in 0..3 {
        assert_eq!(
          row_text(&blit, row),
          row_text(&per_char, row),
          "row {row}, start {start_col}, run {run:?}"
        );
      }
      assert_eq!(
        blit.cursor_position(),
        per_char.cursor_position(),
        "cursor, start {start_col}, run {run:?}"
      );
      assert_eq!(
        crate::term::ansi::render_frame_ansi(&blit.frame()),
        crate::term::ansi::render_frame_ansi(&per_char.frame()),
        "attrs, start {start_col}, run {run:?}"
      );
    }
  }
}

#[test]
fn sgr_colors_and_attributes() {
  let mut s = screen(20, 2);
  feed(
    &mut s,
    b"\x1b[1;31mR\x1b[0m\x1b[38;5;100mI\x1b[38;2;1;2;3mT\x1b[38:5:200mC\x1b[38:2::4:5:6;1mX",
  );
  let cell = style_at(&s, 0, 0);
  assert!(cell.bold());
  assert_eq!(cell.fg, Color::Idx(1));
  assert_eq!(style_at(&s, 0, 1).fg, Color::Idx(100));
  assert_eq!(style_at(&s, 0, 2).fg, Color::Rgb(1, 2, 3));
  assert_eq!(style_at(&s, 0, 3).fg, Color::Idx(200));
  let cell = style_at(&s, 0, 4);
  assert_eq!(cell.fg, Color::Rgb(4, 5, 6));
  assert!(cell.bold(), "params after the colour still apply");

  let mut s = screen(20, 1);
  feed(
    &mut s,
    b"\x1b[2;3;4:3;5;7;8;9;53;58:2::9:8:7ma\x1b[22;24;59mb\x1b[21mc",
  );
  let a = style_at(&s, 0, 0);
  assert!(a.dim() && a.italic() && a.blink() && a.inverse());
  assert!(a.hidden() && a.strike() && a.overline());
  assert!(!a.bold());
  assert_eq!(a.underline(), Underline::Curly);
  assert_eq!(a.ul, Color::Rgb(9, 8, 7));
  let b = style_at(&s, 0, 1);
  assert!(!b.dim() && b.italic());
  assert_eq!(b.underline(), Underline::None);
  assert_eq!(b.ul, Color::Default);
  assert_eq!(style_at(&s, 0, 2).underline(), Underline::Double);
}

#[test]
fn charsets() {
  let mut s = screen(10, 1);
  feed(&mut s, b"\x1b(0qqx\x1b(Bq");
  assert_eq!(row_text(&s, 0), "──│q");
  let mut s = screen(10, 1);
  feed(&mut s, b"\x1b)0a\x0eq\x0fq");
  assert_eq!(row_text(&s, 0), "a─q");
  // Single shift applies to one character.
  let mut s = screen(10, 1);
  feed(&mut s, b"\x1b*0\x1bNqq");
  assert_eq!(row_text(&s, 0), "─q");
}

#[test]
fn cursor_and_erase() {
  let mut s = screen(10, 4);
  feed(&mut s, b"aaaa\r\nbbbb\x1b[1;1H\x1b[K");
  assert_eq!(row_text(&s, 0), "");
  assert_eq!(row_text(&s, 1), "bbbb");
  feed(&mut s, b"\x1b[2J\x1b[3;2Hx");
  assert_eq!(row_text(&s, 1), "");
  assert_eq!(row_text(&s, 2), " x");
  assert_eq!(s.cursor_position(), (2, 2));
  // Erase uses the current background.
  feed(&mut s, b"\x1b[44m\x1b[2K");
  assert_eq!(style_at(&s, 2, 0).bg, Color::Idx(4));
}

#[test]
fn cursor_shape_reaches_the_frame() {
  let mut s = screen(10, 4);
  feed(&mut s, b"\x1b[3 q");
  assert_eq!(s.frame().cursor_style, CursorStyle::BlinkingUnderline);
  // A task that draws into the grid sets it on the screen, as the
  // console's text fields do.
  s.set_cursor_style(CursorStyle::BlinkingBar);
  assert_eq!(s.frame().cursor_style, CursorStyle::BlinkingBar);
}

#[test]
fn tabs_and_repeat() {
  let mut s = screen(20, 1);
  feed(&mut s, b"\x1b[3G\x1bH\x1b[1Ga\tb\x1b[3G\x1b[0g\x1b[1G\tc");
  assert_eq!(row_text(&s, 0), "a b     c");
  let mut s = screen(20, 1);
  feed(&mut s, b"x\x1b[3b-");
  assert_eq!(row_text(&s, 0), "xxxx-");
}

#[test]
fn autowrap_off_and_insert_mode() {
  let mut s = screen(4, 2);
  feed(&mut s, b"\x1b[?7labcdef");
  assert_eq!(row_text(&s, 0), "abcf");
  assert_eq!(s.cursor_position(), (0, 3));
  let mut s = screen(6, 1);
  feed(&mut s, b"abcd\x1b[1;2H\x1b[4hXY");
  assert_eq!(row_text(&s, 0), "aXYbcd");
}

#[test]
fn replies() {
  let mut s = screen(10, 4);
  let events = feed(&mut s, b"\x1b[c\x1b[2;5H\x1b[6n\x07");
  let replies = reply_bytes(&events);
  assert_eq!(replies[0], b"\x1b[?65;6;22;52c");
  assert_eq!(replies[1], b"\x1b[2;5R");
  assert!(events.contains(&VtEvent::Bell));

  let events = feed(&mut s, b"\x1b[>c\x1b[>0q\x1b[?2026$p\x1b[?9999$p\x1b[4$p");
  let replies = reply_bytes(&events);
  assert!(replies[0].starts_with(b"\x1b[>41;"));
  assert_eq!(
    replies[1],
    format!("\x1bP>|dekit {}\x1b\\", env!("CARGO_PKG_VERSION")).as_bytes()
  );
  assert_eq!(replies[2], b"\x1b[?2026;2$y");
  assert_eq!(replies[3], b"\x1b[?9999;0$y");
  assert_eq!(replies[4], b"\x1b[4;2$y");

  s.set_cell_pixels(10, 20);
  let events = feed(&mut s, b"\x1b[14t\x1b[16t\x1b[18t");
  let replies = reply_bytes(&events);
  assert_eq!(replies[0], b"\x1b[4;80;100t");
  assert_eq!(replies[1], b"\x1b[6;20;10t");
  assert_eq!(replies[2], b"\x1b[8;4;10t");

  // XTGETTCAP and DECRQSS.
  let events = feed(&mut s, b"\x1bP+q5463;6e6f7065\x1b\\");
  let replies = reply_bytes(&events);
  assert_eq!(replies[0], b"\x1bP1+r5463=\x1b\\\x1bP0+r\x1b\\");
  let events = feed(&mut s, b"\x1b[1;31m\x1bP$qm\x1b\\\x1bP$q q\x1b\\");
  let replies = reply_bytes(&events);
  assert_eq!(replies[0], b"\x1bP1$r0;1;31m\x1b\\");
  assert_eq!(replies[1], b"\x1bP1$r0 q\x1b\\");

  // Color queries answer from the palette.
  let events = feed(&mut s, b"\x1b]11;?\x1b\\\x1b]4;1;?\x07");
  let replies = reply_bytes(&events);
  assert_eq!(replies[0], b"\x1b]11;rgb:1616/1b1b/2222\x1b\\");
  assert_eq!(replies[1], b"\x1b]4;1;rgb:ffff/7b7b/7272\x1b\\");
}

#[test]
fn title_links_clipboard_notifications() {
  let mut s = screen(10, 2);
  feed(&mut s, b"\x1b]0;hello\x07");
  assert_eq!(s.title(), "hello");
  feed(&mut s, b"\x1b]2;there\x1b\\");
  assert_eq!(s.title(), "there");
  feed(&mut s, b"\x1b[22;0t\x1b]2;other\x07");
  assert_eq!(s.title(), "other");
  feed(&mut s, b"\x1b[23;0t");
  assert_eq!(s.title(), "there");

  feed(&mut s, b"\x1b]8;id=1;http://x\x1b\\ab\x1b]8;;\x1b\\c");
  let frame = s.frame();
  let linked = frame.style(frame.cell(0, 0).unwrap());
  let link = frame.link(&linked).unwrap();
  assert_eq!(link.uri, "http://x");
  assert_eq!(link.id.as_deref(), Some("1"));
  assert_eq!(frame.style(frame.cell(0, 1).unwrap()).link, linked.link);
  assert!(frame.style(frame.cell(0, 2).unwrap()).link.is_none());
  // SGR reset keeps the link; OSC 8 ends it.
  feed(&mut s, b"\x1b]8;;http://y\x1b\\\x1b[0md");
  let frame = s.frame();
  assert!(!frame.style(frame.cell(0, 3).unwrap()).link.is_none());

  let events = feed(&mut s, b"\x1b]52;c;aGVsbG8=\x07");
  assert_eq!(events, vec![VtEvent::SetClipboard("hello".to_string())]);
  let events = feed(&mut s, b"\x1b]777;notify;Title;Body\x07\x1b]9;plain\x07");
  assert_eq!(
    events,
    vec![
      VtEvent::Notify {
        title: "Title".to_string(),
        body: "Body".to_string()
      },
      VtEvent::Notify {
        title: String::new(),
        body: "plain".to_string()
      }
    ]
  );
}

#[test]
fn modes_and_alt_screen() {
  let mut s = screen(10, 4);
  feed(&mut s, b"main\x1b[?1049h");
  assert_eq!(row_text(&s, 0), "");
  feed(&mut s, b"alt");
  assert_eq!(row_text(&s, 0), "alt");
  feed(&mut s, b"\x1b[?1049l");
  assert_eq!(row_text(&s, 0), "main");
  assert_eq!(s.cursor_position(), (0, 4));

  feed(&mut s, b"\x1b[?1h");
  assert!(s.application_cursor());
  feed(&mut s, b"\x1b[?1l");
  assert!(!s.application_cursor());
  feed(&mut s, b"\x1b[?25l");
  assert!(s.hide_cursor());
  feed(&mut s, b"\x1b[?1004h\x1b[?2004h");
  assert!(s.modes().focus_report);
  assert!(s.bracketed_paste());

  feed(&mut s, b"\x1b[?2026h");
  assert!(s.in_sync_update());
  feed(&mut s, b"\x1b[?2026l");
  assert!(!s.in_sync_update());
  let events = feed(&mut s, b"\x1b[?2048h");
  assert_eq!(reply_bytes(&events)[0], b"\x1b[48;4;10;64;80t");
  s.set_size(5, 12);
  let mut events = Vec::new();
  s.take_events(&mut events);
  assert_eq!(reply_bytes(&events)[0], b"\x1b[48;5;12;80;96t");

  // Combined modes in one sequence.
  feed(&mut s, b"\x1b[?1000;1006h");
  assert_eq!(s.mouse_protocol_mode(), MouseProtocolMode::PressRelease);
  assert_eq!(s.mouse_protocol_encoding(), MouseProtocolEncoding::Sgr);
  feed(&mut s, b"\x1b[?1000;1006l");
  assert_eq!(s.mouse_protocol_mode(), MouseProtocolMode::None);
  feed(&mut s, b"\x1b[?1003h\x1b[?1016h");
  assert_eq!(s.mouse_protocol_mode(), MouseProtocolMode::AnyMotion);
  assert_eq!(
    s.mouse_protocol_encoding(),
    MouseProtocolEncoding::SgrPixels
  );
}

#[test]
fn kitty_keyboard_flags() {
  let mut s = screen(10, 4);
  assert_eq!(s.kitty_flags(), 0);
  assert_eq!(reply_bytes(&feed(&mut s, b"\x1b[?u"))[0], b"\x1b[?0u");

  feed(&mut s, b"\x1b[>2u");
  assert_eq!(s.kitty_flags(), 2);
  feed(&mut s, b"\x1b[<u");
  feed(&mut s, b"\x1b[>1u");
  assert_eq!(s.kitty_flags(), 1);
  feed(&mut s, b"\x1b[>15u");
  assert_eq!(s.kitty_flags(), 15);
  assert_eq!(reply_bytes(&feed(&mut s, b"\x1b[?u"))[0], b"\x1b[?15u");
  feed(&mut s, b"\x1b[=4;3u");
  assert_eq!(s.kitty_flags(), 11);
  feed(&mut s, b"\x1b[=4;2u");
  assert_eq!(s.kitty_flags(), 15);
  feed(&mut s, b"\x1b[=2u");
  assert_eq!(s.kitty_flags(), 2);

  // The alternate screen has its own stack.
  feed(&mut s, b"\x1b[?1049h");
  assert_eq!(s.kitty_flags(), 0);
  feed(&mut s, b"\x1b[>1u");
  feed(&mut s, b"\x1b[?1049l");
  assert_eq!(s.kitty_flags(), 2);

  feed(&mut s, b"\x1b[<u");
  assert_eq!(s.kitty_flags(), 1);
  feed(&mut s, b"\x1b[<5u");
  assert_eq!(s.kitty_flags(), 0);
}

#[test]
fn scroll_region_and_margins() {
  let mut s = screen(4, 4);
  feed(&mut s, b"1\r\n2\r\n3\r\n4\x1b[2;3r\x1b[2;1H\x1b[1S");
  assert_eq!(row_text(&s, 0), "1");
  assert_eq!(row_text(&s, 1), "3");
  assert_eq!(row_text(&s, 2), "");
  assert_eq!(row_text(&s, 3), "4");

  let mut s = screen(8, 3);
  feed(&mut s, b"abcdefgh\r\nijklmnop\r\nqrstuvwx");
  feed(&mut s, b"\x1b[?69h\x1b[3;6s\x1b[1;1H\x1b[M");
  assert_eq!(row_text(&s, 0), "abklmngh");
  assert_eq!(row_text(&s, 1), "ijstuvop");
  assert_eq!(row_text(&s, 2), "qr    wx");
  // Insert and delete columns within the margins.
  feed(&mut s, b"\x1b[1;3H\x1b['}");
  assert_eq!(row_text(&s, 0), "ab klmgh");
  feed(&mut s, b"\x1b['~");
  assert_eq!(row_text(&s, 0), "abklm gh");
}

#[test]
fn soft_reset_and_alignment() {
  let mut s = screen(4, 2);
  feed(&mut s, b"\x1b[1;31m\x1b[?7l\x1b[?25l\x1b[!p");
  assert!(s.modes().autowrap);
  assert!(!s.hide_cursor());
  assert_eq!(s.style, Style::DEFAULT);
  feed(&mut s, b"\x1b#8");
  assert_eq!(row_text(&s, 0), "EEEE");
  assert_eq!(row_text(&s, 1), "EEEE");
}

#[test]
fn history_and_view() {
  let mut s = Screen::new(
    Size {
      width: 6,
      height: 2,
    },
    10,
  );
  feed(&mut s, b"one\r\ntwo\r\nthree\r\n");
  assert_eq!(row_text(&s, 0), "three");
  assert_eq!(s.max_scrollback(), 2);
  s.scroll_screen_up(1);
  assert_eq!(row_text(&s, 0), "two");
  assert_eq!(row_text(&s, 1), "three");
  assert!(s.frame().cursor.is_none(), "no cursor while scrolled back");
  feed(&mut s, b"four\r\n");
  // The view stays on what it showed.
  assert_eq!(row_text(&s, 0), "two");
  assert_eq!(s.scrollback(), 2);
  s.set_scrollback(0);
  assert_eq!(row_text(&s, 0), "four");
  feed(&mut s, b"\x1b[3J");
  assert_eq!(s.max_scrollback(), 0);
}

#[test]
fn oversized_osc_aborts() {
  let mut s = screen(10, 2);
  feed(&mut s, b"\x1b]0;");
  feed(&mut s, &vec![b'x'; 1024 * 1024 + 10]);
  // The flood aborted the OSC; the tail renders as plain text and no
  // title was set.
  assert_eq!(s.title(), "");
  assert_eq!(row_text(&s, 0), "xxxxxxxxxx");
}

mod chunking {
  use super::*;
  use proptest::prelude::*;

  fn atom() -> impl Strategy<Value = Vec<u8>> {
    prop_oneof![
      // Printable text
      "[ -~]{0,6}".prop_map(String::into_bytes),
      // Unicode text
      "[à-语]{0,3}".prop_map(String::into_bytes),
      // Arbitrary bytes
      proptest::collection::vec(any::<u8>(), 0..4),
      // Interesting sequences
      proptest::sample::select(vec![
        b"\x1b[2J".to_vec(),
        b"\x1b[1;31m".to_vec(),
        b"\x1b[38;5;100m".to_vec(),
        b"\x1b[38:2:9:8:7m".to_vec(),
        b"\x1b[4:3m".to_vec(),
        b"\x1b[10;10H".to_vec(),
        b"\r\n".to_vec(),
        b"\x1b]0;title\x07".to_vec(),
        b"\x1b]0;t\x1b\\".to_vec(),
        b"\x1b]8;;http://x\x1b\\".to_vec(),
        b"\x1b]8;;\x1b\\".to_vec(),
        b"\x1b[?1049h".to_vec(),
        b"\x1b[?1049l".to_vec(),
        b"\x1b[A".to_vec(),
        b"\x1b[K".to_vec(),
        b"\x1b7".to_vec(),
        b"\x1b8".to_vec(),
        b"\x1b[6n".to_vec(),
        b"\x1b[c".to_vec(),
        b"\x1b(0".to_vec(),
        b"\x1b[2;3r".to_vec(),
        b"\x1b[?69h\x1b[2;5s".to_vec(),
        b"\x1bPdcs\x1b\\".to_vec(),
        b"\x1b_Gi=1,a=q;AAAA\x1b\\".to_vec(),
        b"\x07".to_vec(),
        "\u{2764}\u{FE0F}".as_bytes().to_vec(),
        "\u{1F1FA}\u{1F1F8}".as_bytes().to_vec(),
        "\u{200D}".as_bytes().to_vec(),
      ]),
    ]
  }

  fn snapshot(s: &Screen) -> (String, (u16, u16), String, bool) {
    (
      crate::term::ansi::render_frame_ansi(&s.frame()),
      s.cursor_position(),
      s.title().to_string(),
      s.hide_cursor(),
    )
  }

  proptest! {
    // Feeding the same bytes in different chunkings must produce the
    // same screen state and the same events.
    #[test]
    fn chunking_invariant(
      atoms in proptest::collection::vec(atom(), 0..30),
      splits in proptest::collection::vec(any::<u16>(), 0..8),
    ) {
      let bytes: Vec<u8> = atoms.concat();

      let mut single = screen(12, 6);
      let single_events = feed(&mut single, &bytes);

      let mut split_points: Vec<usize> = splits
        .iter()
        .map(|s| *s as usize % (bytes.len() + 1))
        .collect();
      split_points.sort_unstable();
      let mut chunked = screen(12, 6);
      let mut chunked_events = Vec::new();
      let mut prev = 0;
      for point in split_points {
        chunked_events.extend(feed(&mut chunked, &bytes[prev..point]));
        prev = point;
      }
      chunked_events.extend(feed(&mut chunked, &bytes[prev..]));

      prop_assert_eq!(snapshot(&single), snapshot(&chunked));
      prop_assert_eq!(single_events, chunked_events);
    }
  }
}

mod snapshot_tests {
  use super::*;
  use crate::term::ansi::render_frame_ansi;

  fn state(s: &Screen) -> (String, (u16, u16), String, bool, usize) {
    (
      render_frame_ansi(&s.frame()),
      s.cursor_position(),
      s.title().to_string(),
      s.hide_cursor(),
      s.scrollback(),
    )
  }

  #[test]
  fn round_trip_preserves_rendering_and_parser_state() {
    let mut screen = Screen::new(
      Size {
        width: 20,
        height: 5,
      },
      50,
    );
    for i in 0..30 {
      screen.process(
        format!("\x1b[1;3{}mline {i} \u{4e2d}\x1b[0m\r\n", i % 8).as_bytes(),
      );
    }
    screen.process(b"\x1b]2;title\x07\x1b[?25l\x1b[2;3H\x1b]8;;http://x\x1b\\L\x1b[4:3;58:5:9mU");
    screen.scroll_screen_up(3);
    // Half an escape sequence, then the rest after the round trip.
    screen.process(b"\x1b[3");

    let snapshot = screen.snapshot();
    let json = serde_json::to_vec(&snapshot).unwrap();
    let decoded = serde_json::from_slice(&json).unwrap();
    let mut restored = Screen::from_snapshot(&decoded).unwrap();
    assert_eq!(state(&restored), state(&screen));
    assert_eq!(restored.pending_input(), b"\x1b[3");
    assert_eq!(restored.max_scrollback(), screen.max_scrollback());

    screen.process(b"0mX");
    restored.process(b"0mX");
    assert_eq!(state(&restored), state(&screen));
    assert_eq!(restored.style, screen.style);
    // Links and underline shapes survive.
    screen.set_scrollback(0);
    restored.set_scrollback(0);
    let (a, b) = (screen.frame(), restored.frame());
    let la = a.style(a.cell(1, 2).unwrap());
    let lb = b.style(b.cell(1, 2).unwrap());
    assert_eq!(a.link(&la).unwrap().uri, b.link(&lb).unwrap().uri);
    assert_eq!(b.style(b.cell(1, 3).unwrap()).underline(), Underline::Curly);
  }

  #[test]
  fn alternate_screen_and_modes_survive() {
    let mut screen = Screen::new(
      Size {
        width: 10,
        height: 3,
      },
      0,
    );
    screen
      .process(b"main\x1b[?1049h\x1b[?1000h\x1b[?1006h\x1b[?7l\x1b[?1004halt");
    let restored = Screen::from_snapshot(&screen.snapshot()).unwrap();
    assert_eq!(state(&restored), state(&screen));
    assert_eq!(
      restored.mouse_protocol_mode(),
      MouseProtocolMode::PressRelease
    );
    assert_eq!(
      restored.mouse_protocol_encoding(),
      MouseProtocolEncoding::Sgr
    );
    assert_eq!(restored.modes, screen.modes);
    assert!(!restored.modes().autowrap);
    assert!(restored.modes().focus_report);
  }

  #[test]
  fn cells_keep_their_boundaries() {
    let mut screen = Screen::new(
      Size {
        width: 10,
        height: 2,
      },
      0,
    );
    // A combining accent, a wide char with its continuation, a gap.
    screen.process("e\u{301}x\u{4e2d}y\x1b[2Cz".as_bytes());
    let restored = Screen::from_snapshot(&screen.snapshot()).unwrap();
    let (a, b) = (screen.frame(), restored.frame());
    let mut buf_a = [0u8; 4];
    let mut buf_b = [0u8; 4];
    for col in 0..10 {
      let ca = a.cell(0, col).unwrap();
      let cb = b.cell(0, col).unwrap();
      assert_eq!(a.text(ca, &mut buf_a), b.text(cb, &mut buf_b), "col {col}");
      assert_eq!(ca.is_continuation(), cb.is_continuation(), "col {col}");
    }
    assert_eq!(state(&restored), state(&screen));
  }

  #[test]
  fn pending_wrap_survives() {
    let mut screen = Screen::new(
      Size {
        width: 5,
        height: 3,
      },
      0,
    );
    // Exactly fills the row: the next char wraps.
    screen.process(b"abcde");
    let mut restored = Screen::from_snapshot(&screen.snapshot()).unwrap();
    screen.process(b"X");
    restored.process(b"X");
    assert_eq!(state(&restored), state(&screen));
  }

  #[test]
  fn wrap_fillers_survive() {
    let size = Size {
      width: 4,
      height: 2,
    };
    // The wide char does not fit: a filler, then the wrap.
    let mut screen = Screen::new(size, 10);
    screen.process("abc\u{6d4b}".as_bytes());
    let snapshot = screen.snapshot();
    assert_eq!(snapshot.grid.rows[0].fillers, vec![3]);
    let mut restored = Screen::from_snapshot(&snapshot).unwrap();
    assert_eq!(state(&restored), state(&screen));
    // Padding, not a space: the line rejoins without one.
    screen.set_size(2, 8);
    restored.set_size(2, 8);
    assert_eq!(state(&restored), state(&screen));
    assert_eq!(restored.open_log_line().as_deref(), Some("abc\u{6d4b}"));

    // In scrollback too: the line comes back whole when the screen grows.
    let mut screen = Screen::new(size, 10);
    screen.process("abc\u{6d4b}\r\n\r\n\r\n".as_bytes());
    let mut restored = Screen::from_snapshot(&screen.snapshot()).unwrap();
    screen.set_size(6, 8);
    restored.set_size(6, 8);
    assert_eq!(state(&restored), state(&screen));
    assert_eq!(row_text(&restored, 0), "abc\u{6d4b}");
  }

  #[test]
  fn an_older_snapshot_with_scrollback_rows_loads() {
    // What a 0.10.2 binary wrote: history rows before the screen rows,
    // attrs with the legacy mode byte only.
    let json = r#"{
      "grid": {"width": 4, "height": 2, "pos": [1, 0], "saved_pos": [0, 0],
        "scroll_top": 0, "scroll_bottom": 1,
        "rows": [
          {"text": "old", "cells": [[1, 3], [0, 1]], "attrs": [[0, 4]], "wrapped": false, "size": 3},
          {"text": "abcd", "cells": [[1, 4]], "attrs": [[1, 4]], "wrapped": true, "size": 4},
          {"text": "ef", "cells": [[1, 2], [0, 2]], "attrs": [[0, 4]], "wrapped": false, "size": 2}
        ],
        "used_rows": 2, "origin_mode": false, "saved_origin_mode": false,
        "scrollback_len": 10, "scrollback_offset": 0, "cursor_style": 0},
      "alt_grid": {"width": 4, "height": 2, "pos": [0, 0], "saved_pos": [0, 0],
        "scroll_top": 0, "scroll_bottom": 1,
        "rows": [
          {"text": "", "cells": [[0, 4]], "attrs": [[0, 4]], "wrapped": false, "size": 0},
          {"text": "", "cells": [[0, 4]], "attrs": [[0, 4]], "wrapped": false, "size": 0}
        ],
        "used_rows": 0, "origin_mode": false, "saved_origin_mode": false,
        "scrollback_len": 0, "scrollback_offset": 0, "cursor_style": 0},
      "attrs": {"fg": "default", "bg": "default", "mode": 0},
      "saved_attrs": {"fg": "default", "bg": "default", "mode": 0},
      "modes": 0, "mouse_mode": "none", "mouse_encoding": "default",
      "g0": "ascii", "g1": "ascii", "shift_out": false, "insert": false,
      "kitty_flags": [], "alt_kitty_flags": [], "title": "t",
      "attrs_table": [
        {"fg": "default", "bg": "default", "mode": 0},
        {"fg": {"idx": 1}, "bg": "default", "mode": 5}
      ]
    }"#;
    let saved: crate::upgrade::snapshot::Screen =
      serde_json::from_str(json).unwrap();
    let screen = Screen::from_snapshot(&saved).unwrap();
    assert_eq!(row_text(&screen, 0), "abcd");
    assert_eq!(row_text(&screen, 1), "ef");
    assert_eq!(screen.max_scrollback(), 1);
    let style = style_at(&screen, 0, 0);
    assert_eq!(style.fg, Color::Idx(1));
    assert!(style.bold());
    assert_eq!(style.underline(), Underline::Single);
    assert_eq!(screen.title(), "t");
  }
}

#[test]
fn a_fresh_screen_snapshot_has_only_the_keys_an_older_runner_knows() {
  // A screen in the state an older runner had writes no field it would
  // refuse; new fields appear only when their state is in use.
  let fixture: serde_json::Value =
    serde_json::from_slice(include_bytes!("../../upgrade/fixtures/v1.json"))
      .unwrap();
  fn find_screen(v: &serde_json::Value) -> Option<&serde_json::Value> {
    match v {
      serde_json::Value::Object(map) => {
        if map.contains_key("grid") && map.contains_key("alt_grid") {
          return Some(v);
        }
        map.values().find_map(find_screen)
      }
      serde_json::Value::Array(items) => items.iter().find_map(find_screen),
      _ => None,
    }
  }
  fn check(path: &str, written: &serde_json::Value, known: &serde_json::Value) {
    match (written, known) {
      (serde_json::Value::Object(w), serde_json::Value::Object(k)) => {
        for (key, value) in w {
          let Some(template) = k.get(key) else {
            panic!("new key {key} at {path}");
          };
          check(&format!("{path}.{key}"), value, template);
        }
      }
      (serde_json::Value::Array(w), serde_json::Value::Array(k)) => {
        if let Some(template) = k.first() {
          for item in w {
            check(&format!("{path}[]"), item, template);
          }
        }
      }
      _ => (),
    }
  }
  let known = find_screen(&fixture).expect("a screen in the fixture");
  let mut s = screen(10, 4);
  feed(
    &mut s,
    b"hello\r\n\x1b[1;31mred\x1b[m\r\nmore lines than rows\r\n\r\n\r\n",
  );
  let written = serde_json::to_value(s.snapshot()).unwrap();
  check("screen", &written, known);
}

#[test]
fn ris_keeps_the_replies_already_owed() {
  let mut s = screen(10, 2);
  let events = feed(&mut s, b"\x1b[6n\x1bc");
  assert_eq!(reply_bytes(&events), vec![b"\x1b[1;1R".to_vec()]);
}

#[test]
fn a_cell_size_change_alone_is_reported_inband() {
  let mut s = screen(10, 2);
  s.set_cell_pixels(8, 16);
  feed(&mut s, b"\x1b[?2048h");
  s.set_cell_pixels(10, 20);
  let mut events = Vec::new();
  s.take_events(&mut events);
  let replies = reply_bytes(&events);
  assert_eq!(replies.last().unwrap(), b"\x1b[48;2;10;40;100t");
  // The same size again says nothing.
  s.set_cell_pixels(10, 20);
  assert!(!s.has_pending_events());
}

#[test]
fn shift_left_and_right_ignore_origin_mode() {
  let mut s = screen(20, 6);
  feed(&mut s, b"\x1b[?69h\x1b[5;15s\x1b[2;5r\x1b[?6h");
  // Row 2 (region top), columns 5..15: "abcdefghijk".
  feed(&mut s, b"\x1b[1;1Habcdefghijk");
  // Cursor at origin-relative (2, 3) = absolute (3, 7); shift left by 2.
  feed(&mut s, b"\x1b[2;3H\x1b[2 @");
  assert_eq!(row_text(&s, 1), "    cdefghijk");
  assert_eq!(s.cursor_position(), (2, 6));
}

#[test]
fn a_cluster_stops_growing_at_a_bound() {
  let mut s = screen(10, 2);
  let mut text = String::from("a");
  for _ in 0..2000 {
    text.push('\u{301}');
  }
  feed(&mut s, text.as_bytes());
  let frame = s.frame();
  let cell = frame.cell(0, 0).unwrap();
  let mut buf = [0u8; 4];
  assert!(
    frame.text(cell, &mut buf).len() <= crate::term::unicode::MAX_CLUSTER_BYTES
  );
  // Each prefix up to the bound was interned once, nothing beyond.
  assert!(
    frame.tables.graphemes.len() <= crate::term::unicode::MAX_CLUSTER_BYTES
  );
  assert_eq!(s.cursor_position(), (0, 1));
}

#[test]
fn reverse_video_and_dynamic_colors_show() {
  use crate::protocol::{Feature, TermCaps};
  use crate::term::{Painter, color::Rgb};
  let mut s = screen(4, 1);
  feed(&mut s, b"ab");
  assert!(s.frame_colors().is_plain());
  // Reverse video inverts every cell, blanks included; a cell already
  // inverse shows plain.
  feed(&mut s, b"\x1b[?5h\x1b[7mc\x1b[m");
  assert!(s.frame_colors().reverse);
  assert!(style_at(&s, 0, 0).inverse());
  assert!(!style_at(&s, 0, 2).inverse());
  assert!(style_at(&s, 0, 3).inverse());
  // Frames share the shown table while nothing changes.
  let (a, b) = (s.frame(), s.frame());
  assert!(Arc::ptr_eq(&a.tables.styles, &b.tables.styles));
  feed(&mut s, b"\x1b[?5l");
  assert!(!style_at(&s, 0, 0).inverse());
  // Default colors a program sets apply to default-colored cells only.
  feed(
    &mut s,
    b"\x1b]11;rgb:10/20/30\x1b\\\x1b]10;#ff0000\x1b\\\x1b[34md",
  );
  assert_eq!(s.frame_colors().bg, Some(Rgb(0x10, 0x20, 0x30)));
  assert_eq!(s.frame_colors().fg, Some(Rgb(0xff, 0, 0)));
  let shown = style_at(&s, 0, 0);
  assert_eq!(shown.bg, Color::Rgb(0x10, 0x20, 0x30));
  assert_eq!(shown.fg, Color::Rgb(0xff, 0, 0));
  assert_eq!(style_at(&s, 0, 3).fg, Color::BLUE);
  // The painter emits them, and repaints everything when they change.
  let mut painter = Painter::new(TermCaps::full().without(Feature::Sync));
  let mut out = Vec::new();
  painter.paint(&mut out, &Arc::new(s.frame()));
  let text = String::from_utf8_lossy(&out);
  assert!(text.contains("48;2;16;32;48"), "{text:?}");
  assert!(text.contains("38;2;255;0;0"), "{text:?}");
  feed(&mut s, b"\x1b]111\x1b\\\x1b]110\x1b\\");
  assert!(s.frame_colors().is_plain());
  out.clear();
  painter.paint(&mut out, &Arc::new(s.frame()));
  let text = String::from_utf8_lossy(&out);
  assert!(text.contains("ab"), "{text:?}");
  assert!(!text.contains("48;2"), "{text:?}");
}

#[test]
fn composed_frames_apply_screen_colors() {
  use crate::console::ui_term::render_frame;
  use crate::term::{Grid, Palette, grid::Rect};
  let mut s = screen(4, 1);
  feed(&mut s, b"\x1b]11;#102030\x1b\\a\x1b[?5h");
  let frame = s.frame();
  let mut grid = Grid::new(
    Size {
      width: 4,
      height: 1,
    },
    0,
  );
  render_frame(&frame, Rect::new(0, 0, 4, 1), &mut grid, &Palette::dark());
  for col in [0, 3] {
    let cell = grid.visible_cell(Pos { row: 0, col }).unwrap();
    let style = grid.style_of(cell.style);
    assert_eq!(style.bg, Color::Rgb(0x10, 0x20, 0x30));
    assert!(style.inverse());
  }
}

#[test]
fn dynamic_colors_survive_a_snapshot() {
  use crate::term::color::Rgb;
  let mut s = screen(4, 1);
  feed(&mut s, b"\x1b]11;rgb:10/20/30\x1b\\\x1b[?5h");
  let json = serde_json::to_vec(&s.snapshot()).unwrap();
  let decoded = serde_json::from_slice(&json).unwrap();
  let restored = Screen::from_snapshot(&decoded).unwrap();
  let colors = restored.frame_colors();
  assert_eq!(colors.bg, Some(Rgb(0x10, 0x20, 0x30)));
  assert_eq!(colors.fg, None);
  assert!(colors.reverse);
}

mod log_lines {
  use super::*;

  fn logged(s: &mut Screen) -> Vec<String> {
    let mut out = Vec::new();
    s.take_log_lines(&mut out);
    out
  }

  #[test]
  fn a_newline_logs_the_line_it_ends() {
    let mut s = screen(20, 3);
    feed(&mut s, b"\x1b[32mlistening\x1b[0m on :3000\r\npartial");
    assert_eq!(logged(&mut s), ["listening on :3000"]);
    assert_eq!(s.open_log_line().as_deref(), Some("partial"));
    // NEL and IND end lines too.
    feed(&mut s, b" line\x1bE\x1b[1Gind\x1bD");
    assert_eq!(logged(&mut s), ["partial line", "ind"]);
  }

  #[test]
  fn repeated_and_blank_lines_are_all_logged() {
    let mut s = screen(20, 5);
    feed(&mut s, b"same\r\nsame\r\n\r\n  same\r\n");
    assert_eq!(logged(&mut s), ["same", "same", "", "  same"]);
  }

  #[test]
  fn a_progress_bar_logs_its_last_frame() {
    let mut s = screen(20, 3);
    feed(&mut s, b"build  10%\rbuild  50%\rbuild 100%\r\ndone\r\n");
    assert_eq!(logged(&mut s), ["build 100%", "done"]);
  }

  #[test]
  fn a_needle_overwritten_in_place_is_found() {
    let mut s = screen(30, 3);
    s.look_for(Some("listening".into()));
    let spinner = b"\r\x1b[Kstarting".repeat(200);
    feed(&mut s, &spinner);
    assert!(!s.take_found());
    feed(&mut s, b"\r\x1b[Klistening\r\x1b[Kwatching");
    assert!(s.take_found());
    // Found once: no longer looked for.
    feed(&mut s, b"\r\nlistening\r\n");
    assert!(!s.take_found());
  }

  #[test]
  fn a_needle_is_found_in_logged_lines_and_the_line_under_way() {
    let mut s = screen(20, 3);
    s.look_for(Some("ready".into()));
    feed(&mut s, b"booting\r\nre");
    assert!(!s.take_found());
    feed(&mut s, b"ady");
    assert!(s.take_found());

    // Each logged line is checked, though gone from the screen by now.
    s.look_for(Some("db up".into()));
    feed(&mut s, b"\r\ndb up\r\n1\r\n2\r\n3\r\n");
    assert!(s.take_found());

    // Looking starts with the line under way, and goes on across RIS.
    feed(&mut s, b"port 80");
    s.look_for(Some("port".into()));
    assert!(s.take_found());
    s.look_for(Some("again".into()));
    feed(&mut s, b"\x1bcagain");
    assert!(s.take_found());

    s.look_for(Some("x".into()));
    s.look_for(None);
    feed(&mut s, b"x\r\n");
    assert!(!s.take_found());
  }

  #[test]
  fn a_status_line_logs_what_replaced_it() {
    // Cargo: the bar is cleared and overwritten by each finished step.
    let mut s = screen(30, 3);
    feed(&mut s, b"   Compiling a\r\nBuilding [=>  ] 1/3");
    feed(&mut s, b"\r\x1b[K   Compiling b\r\nBuilding [==> ] 2/3");
    feed(&mut s, b"\r\x1b[2K");
    s.finish_log_line();
    assert_eq!(logged(&mut s), ["   Compiling a", "   Compiling b"]);
  }

  #[test]
  fn redrawn_lines_keep_their_first_state() {
    // docker buildx: the cursor goes back up over the steps it printed.
    let mut s = screen(30, 5);
    feed(&mut s, b"[+] step 1 0.1s\r\n[+] step 2 0.0s\r\n");
    feed(
      &mut s,
      b"\x1b[2A\r\x1b[K[+] step 1 DONE\r\n\x1b[K[+] step 2 0.5s\r\n",
    );
    feed(&mut s, b"\x1b[K[+] step 3\r\n");
    assert_eq!(
      logged(&mut s),
      ["[+] step 1 0.1s", "[+] step 2 0.0s", "[+] step 3"]
    );
  }

  #[test]
  fn erase_and_redraw_frames_are_logged_once() {
    // log-update: each frame erases the lines above and draws again.
    let mut s = screen(20, 5);
    feed(&mut s, b"a 1\r\nb 1");
    feed(&mut s, b"\x1b[2K\x1b[1A\x1b[2K\x1b[Ga 2\r\nb 2");
    feed(&mut s, b"\x1b[2K\x1b[1A\x1b[2K\x1b[Ga 3\r\nb 3\r\n");
    assert_eq!(logged(&mut s), ["a 1", "b 3"]);
  }

  #[test]
  fn a_full_clear_starts_afresh() {
    let mut s = screen(20, 5);
    feed(&mut s, b"one\r\ntwo\r\n");
    assert_eq!(logged(&mut s), ["one", "two"]);
    // tsc --watch
    feed(&mut s, b"one\r\ntwo\r\n\x1bcthree\r\n");
    assert_eq!(logged(&mut s), ["one", "two", "three"]);
    // jest
    feed(&mut s, b"\x1b[2J\x1b[3J\x1b[Hfour\r\n");
    assert_eq!(logged(&mut s), ["four"]);
    // vite: erase below from the top-left
    feed(&mut s, b"\x1b[H\x1b[Jfive\r\n");
    assert_eq!(logged(&mut s), ["five"]);
  }

  #[test]
  fn partial_clears_do_not_start_afresh() {
    let mut s = screen(20, 5);
    feed(&mut s, b"one\r\ntwo\r\n");
    logged(&mut s);
    feed(&mut s, b"\x1b[3J\x1b[Hxx\r\n");
    feed(&mut s, b"\x1b[2;1H\x1b[Jyy\r\n");
    assert_eq!(logged(&mut s), Vec::<String>::new());
  }

  #[test]
  fn the_alternate_screen_is_not_logged() {
    let mut s = screen(20, 5);
    feed(&mut s, b"before\r\npending");
    feed(&mut s, b"\x1b[?1049hvim\r\nstuff\r\n");
    assert_eq!(logged(&mut s), ["before"]);
    assert_eq!(s.open_log_line().as_deref(), Some("pending"));
    feed(&mut s, b"\x1b[?1049l line\r\nafter\r\n");
    assert_eq!(logged(&mut s), ["pending line", "after"]);
  }

  #[test]
  fn long_lines_survive_scrolling_off() {
    let line = "x".repeat(45);
    for scrollback in [0, 100] {
      let mut s = Screen::new(
        Size {
          width: 10,
          height: 3,
        },
        scrollback,
      );
      feed(&mut s, format!("{line}\r\nnext\r\n").as_bytes());
      assert_eq!(logged(&mut s), [line.as_str(), "next"], "{scrollback}");
    }
  }

  #[test]
  fn rows_no_one_logged_are_logged_as_they_scroll_off() {
    let mut s = screen(10, 3);
    feed(&mut s, b"\x1b[3;1Hbottom\x1b[H\x1b[3S");
    assert_eq!(logged(&mut s), ["bottom"]);
  }

  #[test]
  fn a_scroll_region_logs_each_line() {
    // apt: progress on the last row, output scrolling above it.
    let mut s = screen(20, 4);
    feed(&mut s, b"\x1b[1;3r");
    for i in 1..=5 {
      feed(&mut s, format!("line {i}\r\n").as_bytes());
      feed(
        &mut s,
        format!("\x1b7\x1b[4;1HProgress {i}0%\x1b8").as_bytes(),
      );
    }
    assert_eq!(
      logged(&mut s),
      ["line 1", "line 2", "line 3", "line 4", "line 5"]
    );
  }

  #[test]
  fn a_resize_keeps_the_open_line() {
    let mut s = screen(20, 3);
    feed(&mut s, b"done\r\nhalf of a line");
    logged(&mut s);
    s.set_size(3, 6);
    feed(&mut s, b" and more\r\n");
    s.set_size(3, 30);
    feed(&mut s, b"next\r\n");
    assert_eq!(logged(&mut s), ["half of a line and more", "next"]);
  }

  #[test]
  fn an_exit_logs_the_open_line_once() {
    let mut s = screen(20, 3);
    feed(&mut s, b"a\r\nno newline");
    s.finish_log_line();
    s.finish_log_line();
    assert_eq!(logged(&mut s), ["a", "no newline"]);
    assert_eq!(s.open_log_line(), None);
  }

  #[test]
  fn a_reset_keeps_lines_not_taken() {
    let mut s = screen(20, 3);
    feed(&mut s, b"a\r\n\x1bcb\r\n");
    assert_eq!(logged(&mut s), ["a", "b"]);
  }

  #[test]
  fn a_restored_screen_logs_only_new_lines() {
    let mut s = screen(10, 3);
    feed(&mut s, b"a\r\nb\r\nopen");
    logged(&mut s);
    let mut restored = Screen::from_snapshot(&s.snapshot()).unwrap();
    feed(&mut restored, b"\r\nd\r\ne\r\n");
    assert_eq!(logged(&mut restored), ["open", "d", "e"]);
  }
}
