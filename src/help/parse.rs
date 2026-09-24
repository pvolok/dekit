use super::error::{DocError, err};
use super::inline::parse_inlines;
use super::ir::{CalloutVariant, FieldKind, Inline};
use super::patterns::{MAX_FILE_BYTES, is_tag_link};

/// A block as the line parser sees it; `compile` turns it into an IR block.
#[derive(Debug)]
pub enum RawBlock {
  Heading {
    level: u8,
    explicit_id: Option<String>,
    inlines: Vec<Inline>,
    line: usize,
  },
  Paragraph {
    inlines: Vec<Inline>,
  },
  Code {
    lang: Option<String>,
    text: String,
  },
  List {
    ordered: bool,
    items: Vec<Vec<Inline>>,
  },
  Callout {
    variant: CalloutVariant,
    title: Option<String>,
    blocks: Vec<RawBlock>,
  },
  Commands {
    yaml: String,
    line: usize,
  },
  Fields {
    kind: FieldKind,
    yaml: String,
    line: usize,
  },
  Footnote {
    inlines: Vec<Inline>,
  },
  Image {
    src: String,
    alt: String,
    line: usize,
  },
  Usage {
    line: usize,
  },
}

pub struct Split {
  pub fm_lines: Vec<String>,
  pub body_lines: Vec<String>,
  pub fm_start_line: usize,
  pub body_start_line: usize,
}

pub fn split_lines(source: &str) -> Vec<String> {
  let source = source.strip_prefix('\u{feff}').unwrap_or(source);
  let normalized = source.replace("\r\n", "\n").replace('\r', "\n");
  normalized.split('\n').map(str::to_string).collect()
}

/// Err means the file cannot be parsed further.
pub fn split_frontmatter(source: &str, path: &str) -> Result<Split, DocError> {
  if source.len() > MAX_FILE_BYTES {
    return Err(err(
      path,
      1,
      format!("file exceeds {MAX_FILE_BYTES} byte cap"),
    ));
  }
  let lines = split_lines(source);
  if lines.first().map(String::as_str) != Some("---") {
    return Err(err(path, 1, "file must start with --- frontmatter"));
  }
  let Some(close) = lines.iter().skip(1).position(|line| line == "---") else {
    return Err(err(path, 1, "unclosed frontmatter"));
  };
  let close = close + 1;
  let fm_lines = lines[1..close].to_vec();
  let mut body_lines = lines[close + 1..].to_vec();
  let mut body_start_line = close + 2;
  if body_lines.first().map(String::as_str) == Some("") {
    body_lines.remove(0);
    body_start_line += 1;
  }
  Ok(Split {
    fm_lines,
    body_lines,
    fm_start_line: 2,
    body_start_line,
  })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LineClass {
  Blank,
  Closer,
  Container,
  Fence,
  Heading,
  HashError,
  Blockquote,
  Html,
  TagParagraph,
  Table,
  Thematic,
  RefDef,
  Ul,
  StarList,
  Ol,
  Image,
  Paragraph,
}

fn heading_parts(line: &str) -> Option<(u8, &str)> {
  let hashes = line.chars().take_while(|c| *c == '#').count();
  if !(2..=3).contains(&hashes) {
    return None;
  }
  let rest = line[hashes..].strip_prefix(' ')?;
  if rest.is_empty() {
    return None;
  }
  Some((hashes as u8, rest))
}

fn is_html_open(line: &str) -> bool {
  if line.starts_with("<!--") {
    return true;
  }
  let rest = match line.strip_prefix('<') {
    Some(rest) => rest,
    None => return false,
  };
  let rest = rest.strip_prefix('/').unwrap_or(rest);
  rest.chars().next().is_some_and(|c| c.is_ascii_alphabetic())
}

fn is_whole_line_tag(line: &str) -> bool {
  line.len() >= 3
    && line.starts_with('|')
    && line.ends_with('|')
    && is_tag_link(&line[1..line.len() - 1])
}

fn is_table(line: &str) -> bool {
  line.starts_with('|')
    && (line.matches('|').count() >= 3
      || line.chars().nth(1).is_some_and(char::is_whitespace))
}

fn is_thematic(line: &str) -> bool {
  line.chars().all(|c| matches!(c, '-' | '*' | '_' | ' '))
    && line.chars().filter(|c| *c != ' ').count() >= 3
}

fn is_setext_underline(line: &str) -> bool {
  line.len() >= 3
    && (line.chars().all(|c| c == '=') || line.chars().all(|c| c == '-'))
}

fn is_ref_def(line: &str) -> bool {
  let Some(rest) = line.strip_prefix('[') else {
    return false;
  };
  let Some(rb) = rest.find(']') else {
    return false;
  };
  if rb == 0 {
    return false;
  }
  let after = &rest[rb + 1..];
  after.starts_with(':')
    && after[1..].chars().next().is_some_and(char::is_whitespace)
}

fn ordered_item(line: &str) -> Option<&str> {
  let digits = line.chars().take_while(char::is_ascii_digit).count();
  if digits == 0 {
    return None;
  }
  line[digits..].strip_prefix(". ")
}

fn md_image(line: &str) -> Option<(String, String)> {
  let rest = line.strip_prefix("![")?;
  let rb = rest.find(']')?;
  let alt = &rest[..rb];
  let src = rest[rb + 1..].strip_prefix('(')?.strip_suffix(')')?;
  if src.is_empty() || src.contains(')') {
    return None;
  }
  Some((alt.to_string(), src.to_string()))
}

fn classify(line: &str) -> LineClass {
  if line.trim().is_empty() {
    return LineClass::Blank;
  }
  if line.trim() == ":::" {
    return LineClass::Closer;
  }
  if line.trim_start().starts_with(":::") {
    return LineClass::Container;
  }
  if line.starts_with("```") {
    return LineClass::Fence;
  }
  if heading_parts(line).is_some() {
    return LineClass::Heading;
  }
  if line.starts_with('#') {
    return LineClass::HashError;
  }
  if line.starts_with('>') {
    return LineClass::Blockquote;
  }
  if is_html_open(line) {
    return LineClass::Html;
  }
  if is_whole_line_tag(line) {
    return LineClass::TagParagraph;
  }
  if is_table(line) {
    return LineClass::Table;
  }
  if is_thematic(line) {
    return LineClass::Thematic;
  }
  if is_ref_def(line) {
    return LineClass::RefDef;
  }
  if line.starts_with("- ") {
    return LineClass::Ul;
  }
  if line.starts_with("* ") {
    return LineClass::StarList;
  }
  if ordered_item(line).is_some() {
    return LineClass::Ol;
  }
  if md_image(line).is_some() {
    return LineClass::Image;
  }
  LineClass::Paragraph
}

fn is_interrupt(class: LineClass) -> bool {
  class != LineClass::Blank && class != LineClass::Paragraph
}

pub struct ParseCtx<'a> {
  pub path: &'a str,
  pub topic_title: &'a str,
  pub allow_containers: bool,
}

struct Parsed {
  block: Option<RawBlock>,
  next: usize,
}

pub fn parse_blocks(
  lines: &[String],
  start_line: usize,
  ctx: &ParseCtx<'_>,
  errors: &mut Vec<DocError>,
) -> Vec<RawBlock> {
  let mut blocks = Vec::new();
  let mut i = 0;
  let mut stripped_h1 = false;
  let path = ctx.path;

  while i < lines.len() {
    let line = &lines[i];
    let class = classify(line);
    let ln = start_line + i;

    if class == LineClass::Blank {
      i += 1;
      continue;
    }

    if !stripped_h1 && blocks.is_empty() && line.starts_with("# ") {
      let text = &line[2..];
      if text == ctx.topic_title {
        stripped_h1 = true;
      } else {
        errors.push(err(
          path,
          ln,
          format!("leading h1 must match title \"{}\"", ctx.topic_title),
        ));
      }
      i += 1;
      continue;
    }

    match class {
      LineClass::Blank => unreachable!(),
      LineClass::Closer => {
        errors.push(err(path, ln, "stray container closer"));
        i += 1;
      }
      LineClass::Container => {
        if !ctx.allow_containers {
          errors.push(err(path, ln, "nested containers are forbidden"));
          i = skip_until_closer(lines, i);
          continue;
        }
        let parsed = parse_container(lines, i, start_line, ctx, errors);
        blocks.extend(parsed.block);
        i = parsed.next;
      }
      LineClass::Fence => {
        let parsed = parse_fence(lines, i, start_line, path, errors);
        blocks.extend(parsed.block);
        i = parsed.next;
      }
      LineClass::Heading => {
        blocks.push(parse_heading(line, path, ln, errors));
        i += 1;
      }
      LineClass::HashError => {
        errors.push(err(
          path,
          ln,
          "only ## / ### headings are allowed in the body",
        ));
        i += 1;
      }
      LineClass::Blockquote => {
        errors.push(err(path, ln, "blockquotes are forbidden; use :::callout"));
        i += 1;
      }
      LineClass::Html => {
        errors.push(err(path, ln, "HTML is forbidden"));
        i += 1;
      }
      LineClass::Table => {
        errors.push(err(
          path,
          ln,
          "GFM tables are forbidden; use :::commands or :::fields",
        ));
        i += 1;
      }
      LineClass::Thematic => {
        errors.push(err(path, ln, "thematic breaks are forbidden"));
        i += 1;
      }
      LineClass::RefDef => {
        errors.push(err(
          path,
          ln,
          "reference-style link definitions are forbidden",
        ));
        i += 1;
      }
      LineClass::StarList => {
        errors.push(err(path, ln, "use '- ' for unordered lists, not '* '"));
        i += 1;
      }
      LineClass::Ul | LineClass::Ol => {
        let parsed =
          parse_list(lines, i, start_line, ctx, class == LineClass::Ol, errors);
        blocks.extend(parsed.block);
        i = parsed.next;
      }
      LineClass::Image => {
        let (alt, src) = md_image(line).expect("classified as image");
        blocks.push(RawBlock::Image { src, alt, line: ln });
        i += 1;
      }
      LineClass::TagParagraph | LineClass::Paragraph => {
        let parsed = parse_paragraph(
          lines,
          i,
          start_line,
          ctx,
          class == LineClass::TagParagraph,
          errors,
        );
        blocks.extend(parsed.block);
        i = parsed.next;
      }
    }
  }

  blocks
}

fn skip_until_closer(lines: &[String], mut i: usize) -> usize {
  i += 1;
  while i < lines.len() && classify(&lines[i]) != LineClass::Closer {
    i += 1;
  }
  if i < lines.len() { i + 1 } else { i }
}

fn parse_paragraph(
  lines: &[String],
  i: usize,
  start_line: usize,
  ctx: &ParseCtx<'_>,
  single: bool,
  errors: &mut Vec<DocError>,
) -> Parsed {
  let ln = start_line + i;
  if single {
    let (inlines, inner) = parse_inlines(&lines[i], ctx.path, ln);
    errors.extend(inner);
    return Parsed {
      block: Some(RawBlock::Paragraph { inlines }),
      next: i + 1,
    };
  }
  let mut parts = vec![lines[i].as_str()];
  let mut j = i + 1;
  while j < lines.len() {
    let next = &lines[j];
    if is_setext_underline(next) {
      errors.push(err(
        ctx.path,
        start_line + j,
        "setext headings are forbidden",
      ));
      j += 1;
      break;
    }
    let class = classify(next);
    if class == LineClass::Blank || is_interrupt(class) {
      break;
    }
    parts.push(next);
    j += 1;
  }
  let (inlines, inner) = parse_inlines(&parts.join(" "), ctx.path, ln);
  errors.extend(inner);
  Parsed {
    block: Some(RawBlock::Paragraph { inlines }),
    next: j,
  }
}

fn parse_heading(
  line: &str,
  path: &str,
  ln: usize,
  errors: &mut Vec<DocError>,
) -> RawBlock {
  let (level, text) = heading_parts(line).expect("classified as heading");
  let (text, explicit_id) = match text.strip_suffix('}') {
    Some(head) => match head.rfind(" {#") {
      Some(at) => (&text[..at], Some(head[at + 3..].to_string())),
      None => (text, None),
    },
    None => (text, None),
  };
  let (inlines, inner) = parse_inlines(text, path, ln);
  errors.extend(inner);
  RawBlock::Heading {
    level,
    explicit_id,
    inlines,
    line: ln,
  }
}

fn parse_fence(
  lines: &[String],
  i: usize,
  start_line: usize,
  path: &str,
  errors: &mut Vec<DocError>,
) -> Parsed {
  let opener = &lines[i];
  let ticks: String = opener.chars().take_while(|c| *c == '`').collect();
  let lang = opener[ticks.len()..].trim();
  let lang = if lang.is_empty() {
    None
  } else {
    Some(lang.to_string())
  };
  let mut body = Vec::new();
  let mut j = i + 1;
  while j < lines.len() {
    let line = &lines[j];
    if line.starts_with(&ticks) && line[ticks.len()..].trim().is_empty() {
      return Parsed {
        block: Some(RawBlock::Code {
          lang,
          text: body.join("\n"),
        }),
        next: j + 1,
      };
    }
    body.push(line.as_str());
    j += 1;
  }
  errors.push(err(path, start_line + i, "unclosed code fence"));
  Parsed {
    block: None,
    next: j,
  }
}

fn parse_list(
  lines: &[String],
  i: usize,
  start_line: usize,
  ctx: &ParseCtx<'_>,
  ordered: bool,
  errors: &mut Vec<DocError>,
) -> Parsed {
  let mut items = Vec::new();
  let mut j = i;
  while j < lines.len() {
    let line = &lines[j];
    let class = classify(line);
    if ordered {
      if class != LineClass::Ol {
        break;
      }
    } else {
      if class == LineClass::StarList {
        errors.push(err(
          ctx.path,
          start_line + j,
          "use '- ' for unordered lists, not '* '",
        ));
        j += 1;
        continue;
      }
      if class != LineClass::Ul {
        break;
      }
    }
    let text = if ordered {
      ordered_item(line).expect("classified as ordered item")
    } else {
      &line[2..]
    };
    let (inlines, inner) = parse_inlines(text, ctx.path, start_line + j);
    errors.extend(inner);
    items.push(inlines);
    j += 1;
    if j < lines.len() {
      let peek = classify(&lines[j]);
      let is_item = if ordered {
        peek == LineClass::Ol
      } else {
        peek == LineClass::Ul || peek == LineClass::StarList
      };
      if !is_item && peek != LineClass::Blank && !is_interrupt(peek) {
        errors.push(err(
          ctx.path,
          start_line + j,
          "nested list / paragraph content is forbidden",
        ));
        j += 1;
      }
    }
  }
  Parsed {
    block: Some(RawBlock::List { ordered, items }),
    next: j,
  }
}

struct ContainerHead {
  kind: String,
  bare: Option<String>,
  attrs: Vec<(String, String)>,
}

fn tokenize_attr_line(s: &str) -> Vec<String> {
  let chars: Vec<char> = s.chars().collect();
  let mut out = Vec::new();
  let mut i = 0;
  while i < chars.len() {
    while i < chars.len() && chars[i] == ' ' {
      i += 1;
    }
    if i >= chars.len() {
      break;
    }
    let mut token = String::new();
    let mut quoted = false;
    while i < chars.len() {
      let c = chars[i];
      if quoted {
        token.push(c);
        if c == '"' {
          quoted = false;
        }
        i += 1;
        continue;
      }
      if c == ' ' {
        break;
      }
      token.push(c);
      if c == '"' && token.ends_with("=\"") {
        quoted = true;
      }
      i += 1;
    }
    out.push(token);
  }
  out
}

fn parse_attrs(
  line: &str,
  path: &str,
  ln: usize,
  errors: &mut Vec<DocError>,
) -> Option<ContainerHead> {
  let trimmed = line.trim_start();
  let trimmed = trimmed.strip_prefix(":::").unwrap_or(trimmed).trim();
  let tokens = tokenize_attr_line(trimmed);
  let Some(kind) = tokens.first() else {
    errors.push(err(path, ln, "missing container type"));
    return None;
  };
  let mut bare = None;
  let mut attrs = Vec::new();
  for token in &tokens[1..] {
    match token.split_once('=') {
      None => {
        if bare.is_some() {
          errors.push(err(
            path,
            ln,
            format!("unexpected extra argument \"{token}\""),
          ));
        } else {
          bare = Some(token.clone());
        }
      }
      Some((key, value)) => {
        let value = value
          .strip_prefix('"')
          .and_then(|v| v.strip_suffix('"'))
          .unwrap_or(value);
        attrs.push((key.to_string(), value.to_string()));
      }
    }
  }
  Some(ContainerHead {
    kind: kind.clone(),
    bare,
    attrs,
  })
}

fn attr<'a>(attrs: &'a [(String, String)], key: &str) -> Option<&'a str> {
  attrs
    .iter()
    .find(|(k, _)| k == key)
    .map(|(_, v)| v.as_str())
}

fn reject_unknown_attrs(
  head: &ContainerHead,
  allowed: &[&str],
  path: &str,
  ln: usize,
  errors: &mut Vec<DocError>,
) {
  for (key, _) in &head.attrs {
    if !allowed.contains(&key.as_str()) {
      errors.push(err(
        path,
        ln,
        format!("unknown {} key \"{key}\"", head.kind),
      ));
    }
  }
}

fn parse_container(
  lines: &[String],
  i: usize,
  start_line: usize,
  ctx: &ParseCtx<'_>,
  errors: &mut Vec<DocError>,
) -> Parsed {
  let path = ctx.path;
  let ln = start_line + i;
  let Some(head) = parse_attrs(&lines[i], path, ln, errors) else {
    return Parsed {
      block: None,
      next: skip_until_closer(lines, i),
    };
  };

  // Void containers are one line: no body, no closer.
  match head.kind.as_str() {
    "usage" => {
      if head.bare.is_some() || !head.attrs.is_empty() {
        errors.push(err(path, ln, ":::usage takes no arguments"));
      }
      return Parsed {
        block: Some(RawBlock::Usage { line: ln }),
        next: i + 1,
      };
    }
    "image" => {
      reject_unknown_attrs(&head, &["src", "alt"], path, ln, errors);
      if head.bare.is_some() {
        errors.push(err(path, ln, ":::image takes no bare argument"));
      }
      let (src, alt) = (attr(&head.attrs, "src"), attr(&head.attrs, "alt"));
      let (Some(src), Some(alt)) = (src, alt) else {
        errors.push(err(path, ln, ":::image requires src and alt"));
        return Parsed {
          block: None,
          next: i + 1,
        };
      };
      return Parsed {
        block: Some(RawBlock::Image {
          src: src.to_string(),
          alt: alt.to_string(),
          line: ln,
        }),
        next: i + 1,
      };
    }
    _ => {}
  }

  let mut j = i + 1;
  let mut body = Vec::new();
  let mut closed = false;
  while j < lines.len() {
    if classify(&lines[j]) == LineClass::Closer {
      closed = true;
      j += 1;
      break;
    }
    body.push(lines[j].clone());
    j += 1;
  }
  if !closed {
    errors.push(err(path, ln, "unclosed container"));
    return Parsed {
      block: None,
      next: j,
    };
  }
  let body_start = ln + 1;

  let block = match head.kind.as_str() {
    "callout" => {
      let variant = match head.bare.as_deref() {
        None | Some("note") => CalloutVariant::Note,
        Some("tip") => CalloutVariant::Tip,
        Some("warning") => CalloutVariant::Warning,
        Some(other) => {
          errors.push(err(
            path,
            ln,
            format!("unknown callout variant \"{other}\""),
          ));
          CalloutVariant::Note
        }
      };
      reject_unknown_attrs(&head, &["title"], path, ln, errors);
      let inner = ParseCtx {
        path,
        topic_title: ctx.topic_title,
        allow_containers: false,
      };
      let blocks = parse_blocks(&body, body_start, &inner, errors);
      if blocks.iter().any(|block| {
        !matches!(
          block,
          RawBlock::Paragraph { .. }
            | RawBlock::List { .. }
            | RawBlock::Code { .. }
        )
      }) {
        errors.push(err(
          path,
          ln,
          "callout body may only contain paragraph, list, or code",
        ));
      }
      Some(RawBlock::Callout {
        variant,
        title: attr(&head.attrs, "title").map(str::to_string),
        blocks,
      })
    }
    "footnote" => {
      if head.bare.is_some() || !head.attrs.is_empty() {
        errors.push(err(path, ln, ":::footnote takes no arguments"));
      }
      if body.iter().any(|line| {
        !matches!(
          classify(line),
          LineClass::Blank | LineClass::Paragraph | LineClass::TagParagraph
        )
      }) {
        errors.push(err(
          path,
          ln,
          ":::footnote body must be inlines (no nested blocks)",
        ));
      }
      let text = body
        .iter()
        .filter(|line| !line.trim().is_empty())
        .map(String::as_str)
        .collect::<Vec<_>>()
        .join(" ");
      let (inlines, inner) = parse_inlines(&text, path, ln);
      errors.extend(inner);
      Some(RawBlock::Footnote { inlines })
    }
    "commands" => {
      if head.bare.is_some() || !head.attrs.is_empty() {
        errors.push(err(path, ln, ":::commands takes no arguments"));
      }
      Some(RawBlock::Commands {
        yaml: body.join("\n"),
        line: body_start,
      })
    }
    "fields" => {
      reject_unknown_attrs(&head, &["kind"], path, ln, errors);
      if head.bare.is_some() {
        errors.push(err(path, ln, ":::fields takes no bare argument"));
      }
      match attr(&head.attrs, "kind").and_then(FieldKind::parse) {
        Some(kind) => Some(RawBlock::Fields {
          kind,
          yaml: body.join("\n"),
          line: body_start,
        }),
        None => {
          errors.push(err(
            path,
            ln,
            ":::fields requires kind=config|js|cli-flag",
          ));
          None
        }
      }
    }
    other => {
      errors.push(err(path, ln, format!("unknown container \"{other}\"")));
      None
    }
  };
  Parsed { block, next: j }
}
