use super::error::{DocError, err_at};
use super::ir::Inline;
use super::patterns::is_tag_link;

fn is_alnum(c: Option<char>) -> bool {
  c.is_some_and(|c| c.is_ascii_alphanumeric())
}

fn is_space(c: Option<char>) -> bool {
  match c {
    None => true,
    Some(c) => c == ' ' || c == '\t',
  }
}

fn starts_with(chars: &[char], at: usize, pat: &str) -> bool {
  let pat: Vec<char> = pat.chars().collect();
  chars.len() >= at + pat.len() && chars[at..at + pat.len()] == pat[..]
}

fn find_str(chars: &[char], from: usize, pat: &str) -> Option<usize> {
  (from..chars.len()).find(|&j| starts_with(chars, j, pat))
}

fn find_char(chars: &[char], from: usize, c: char) -> Option<usize> {
  (from..chars.len()).find(|&j| chars[j] == c)
}

fn slice(chars: &[char], from: usize, to: usize) -> String {
  chars[from..to].iter().collect()
}

struct MdLink {
  label: String,
  href: String,
  end: usize,
}

fn match_md_link(chars: &[char], i: usize) -> Option<MdLink> {
  if chars.get(i) != Some(&'[') {
    return None;
  }
  let rb = find_char(chars, i + 1, ']')?;
  if chars.get(rb + 1) != Some(&'(') {
    return None;
  }
  let rp = find_char(chars, rb + 2, ')')?;
  Some(MdLink {
    label: slice(chars, i + 1, rb),
    href: slice(chars, rb + 2, rp),
    end: rp + 1,
  })
}

fn match_ref_use(chars: &[char], i: usize) -> Option<usize> {
  if chars.get(i) != Some(&'[') {
    return None;
  }
  let rb = find_char(chars, i + 1, ']')?;
  if chars.get(rb + 1) != Some(&'[') {
    return None;
  }
  let rb2 = find_char(chars, rb + 2, ']')?;
  Some(rb2 + 1)
}

fn match_autolink(chars: &[char], i: usize) -> Option<usize> {
  if chars.get(i) != Some(&'<') {
    return None;
  }
  let gt = find_char(chars, i + 1, '>')?;
  let inner = slice(chars, i + 1, gt);
  if inner.starts_with("https://")
    || inner.starts_with("http://")
    || inner.starts_with("mailto:")
  {
    Some(gt + 1)
  } else {
    None
  }
}

fn can_open_em(chars: &[char], i: usize) -> bool {
  let prev = if i > 0 { Some(chars[i - 1]) } else { None };
  let next = chars.get(i + 1).copied();
  if is_alnum(prev) {
    return false;
  }
  !is_space(next)
}

fn find_em_close(chars: &[char], from: usize) -> Option<usize> {
  for j in from..chars.len() {
    if chars[j] != '_' {
      continue;
    }
    let prev = Some(chars[j - 1]);
    let next = chars.get(j + 1).copied();
    if is_space(prev) || is_alnum(next) {
      continue;
    }
    return Some(j);
  }
  None
}

pub fn parse_inlines(
  text: &str,
  path: &str,
  line: usize,
) -> (Vec<Inline>, Vec<DocError>) {
  let chars: Vec<char> = text.chars().collect();
  let n = chars.len();
  let mut errors = Vec::new();
  let mut inlines = Vec::new();
  let mut buf = String::new();
  let mut i = 0;

  fn flush(buf: &mut String, inlines: &mut Vec<Inline>) {
    if !buf.is_empty() {
      inlines.push(Inline::Text {
        text: std::mem::take(buf),
      });
    }
  }

  while i < n {
    let c = chars[i];

    if c == '`' {
      let mut count = 0;
      while i + count < n && chars[i + count] == '`' {
        count += 1;
      }
      let ticks = "`".repeat(count);
      match find_str(&chars, i + count, &ticks) {
        None => {
          errors.push(err_at(path, line, i + 1, "unclosed code span"));
          buf.push_str(&slice(&chars, i, n));
          i = n;
        }
        Some(close) => {
          flush(&mut buf, &mut inlines);
          inlines.push(Inline::Code {
            text: slice(&chars, i + count, close),
          });
          i = close + count;
        }
      }
      continue;
    }

    if starts_with(&chars, i, "**") {
      match find_str(&chars, i + 2, "**") {
        None => {
          errors.push(err_at(path, line, i + 1, "unclosed **strong**"));
          buf.push_str(&slice(&chars, i, n));
          i = n;
        }
        Some(close) => {
          flush(&mut buf, &mut inlines);
          let (children, inner_errors) =
            parse_inlines(&slice(&chars, i + 2, close), path, line);
          errors.extend(inner_errors);
          inlines.push(Inline::Strong { children });
          i = close + 2;
        }
      }
      continue;
    }

    if c == '|' {
      if let Some(close) = find_char(&chars, i + 1, '|')
        && close > i + 1
      {
        let tag = slice(&chars, i + 1, close);
        if is_tag_link(&tag) {
          flush(&mut buf, &mut inlines);
          inlines.push(Inline::TagLink { tag });
          i = close + 1;
          continue;
        }
      }
      buf.push('|');
      i += 1;
      continue;
    }

    if c == '!'
      && chars.get(i + 1) == Some(&'[')
      && let Some(link) = match_md_link(&chars, i + 1)
    {
      errors.push(err_at(
        path,
        line,
        i + 1,
        "images are a block (whole-line ![alt](src) or :::image), not an inline",
      ));
      i = link.end;
      continue;
    }

    if c == '[' {
      if let Some(link) = match_md_link(&chars, i) {
        if link.href.starts_with("http://") || link.href.starts_with("https://")
        {
          flush(&mut buf, &mut inlines);
          inlines.push(Inline::UrlLink {
            href: link.href,
            children: vec![Inline::Text { text: link.label }],
          });
        } else {
          errors.push(err_at(
            path,
            line,
            i + 1,
            "url-link must be http or https; use |tag| for internal links",
          ));
        }
        i = link.end;
        continue;
      }
      if let Some(end) = match_ref_use(&chars, i) {
        errors.push(err_at(
          path,
          line,
          i + 1,
          "reference-style links are forbidden",
        ));
        i = end;
        continue;
      }
    }

    if c == '<'
      && let Some(end) = match_autolink(&chars, i)
    {
      errors.push(err_at(
        path,
        line,
        i + 1,
        "autolinks are forbidden; use [label](https://...)",
      ));
      i = end;
      continue;
    }

    if c == '_' && can_open_em(&chars, i) {
      match find_em_close(&chars, i + 1) {
        None => {
          errors.push(err_at(path, line, i + 1, "unclosed _em_"));
          buf.push_str(&slice(&chars, i, n));
          i = n;
        }
        Some(close) => {
          flush(&mut buf, &mut inlines);
          let (children, inner_errors) =
            parse_inlines(&slice(&chars, i + 1, close), path, line);
          errors.extend(inner_errors);
          inlines.push(Inline::Em { children });
          i = close + 1;
        }
      }
      continue;
    }

    buf.push(c);
    i += 1;
  }

  flush(&mut buf, &mut inlines);
  (inlines, errors)
}
