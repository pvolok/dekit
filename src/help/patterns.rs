pub const MAX_FILE_BYTES: usize = 64 * 1024;

fn ident_like(s: &str, rest: impl Fn(char) -> bool) -> bool {
  let mut chars = s.chars();
  match chars.next() {
    Some(first) if first.is_ascii_lowercase() => chars.all(rest),
    _ => false,
  }
}

/// Topic aliases and heading ids: `^[a-z][a-z0-9-]*$`.
pub fn is_ident(s: &str) -> bool {
  ident_like(s, |c| {
    c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-'
  })
}

/// Tags on command and field records: `^[a-z][a-z0-9.:_-]*$`.
pub fn is_record_tag(s: &str) -> bool {
  ident_like(s, |c| {
    c.is_ascii_lowercase()
      || c.is_ascii_digit()
      || matches!(c, '.' | ':' | '_' | '-')
  })
}

fn is_tag_name(s: &str) -> bool {
  ident_like(s, |c| {
    c.is_ascii_lowercase()
      || c.is_ascii_digit()
      || matches!(c, '.' | '/' | ':' | '_' | '-')
  })
}

/// The inside of `|tag|`: a topic id, alias, or record tag, optionally
/// followed by `#heading-id`.
pub fn is_tag_link(s: &str) -> bool {
  match s.split_once('#') {
    Some((tag, heading)) => is_tag_name(tag) && is_ident(heading),
    None => is_tag_name(s),
  }
}

/// One path segment of a topic: `^[a-z0-9]+(-[a-z0-9]+)*$`.
pub fn is_slug_segment(s: &str) -> bool {
  !s.is_empty()
    && s.split('-').all(|part| {
      !part.is_empty()
        && part
          .chars()
          .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
    })
}
