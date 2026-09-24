//! Lays a topic out as lines of styled spans at a given width. The stdout
//! renderer turns them into ANSI; the console will draw the same lines
//! into its grid.

use unicode_width::UnicodeWidthStr;

use super::ir::{Block, CalloutVariant, DocsIr, FieldKind, Inline, Topic};
use crate::term::{Color, attrs::Attrs};

pub const MAX_WIDTH: usize = 100;

#[derive(Clone, Debug, PartialEq)]
pub struct Span {
  pub text: String,
  pub attrs: Attrs,
}

pub type Line = Vec<Span>;

fn span(text: impl Into<String>, attrs: Attrs) -> Span {
  Span {
    text: text.into(),
    attrs,
  }
}

fn plain(text: impl Into<String>) -> Span {
  span(text, Attrs::default())
}

fn bold() -> Attrs {
  Attrs::default().set_bold(true)
}

fn dim() -> Attrs {
  Attrs::default().fg(Color::BRIGHT_BLACK)
}

fn code_style() -> Attrs {
  Attrs::default().fg(Color::CYAN)
}

fn spaces(n: usize) -> Span {
  plain(" ".repeat(n))
}

fn width_of(spans: &[Span]) -> usize {
  spans.iter().map(|s| s.text.width()).sum()
}

pub fn topic(topic: &Topic, docs: &DocsIr, width: usize) -> Vec<Line> {
  let mut out = vec![vec![span(&topic.title, bold())]];
  if !topic.summary.is_empty() {
    out.extend(wrap(vec![plain(&topic.summary)], width, vec![], vec![]));
  }
  out.push(vec![]);
  blocks(&topic.blocks, width, &mut out);
  let related: Vec<&Topic> = topic
    .related
    .iter()
    .filter_map(|id| docs.topic(id))
    .collect();
  if !related.is_empty() {
    out.push(vec![]);
    out.push(vec![span("Related", bold())]);
    let pad = related.iter().map(|t| t.id.width()).max().unwrap_or(0);
    for related in related {
      out.push(vec![
        plain("  "),
        span(format!("dekit help {}", related.id), code_style()),
        spaces(pad - related.id.width() + 2),
        span(&related.title, dim()),
      ]);
    }
  }
  out
}

pub fn index(docs: &DocsIr, width: usize) -> Vec<Line> {
  let mut out = vec![vec![span("dekit help", bold())]];
  out.extend(wrap(
    vec![plain(
      "Documentation topics. Open one with dekit help <topic>; a command's \
       name, a page name, or a tag all work.",
    )],
    width,
    vec![],
    vec![],
  ));
  for section in &docs.nav {
    out.push(vec![]);
    out.push(vec![span(&section.title, bold())]);
    let topics: Vec<&Topic> = section
      .topics
      .iter()
      .filter_map(|id| docs.topic(id))
      .collect();
    let pad = topics.iter().map(|t| t.id.width()).max().unwrap_or(0) + 2;
    for topic in topics {
      let first = vec![
        plain("  "),
        span(&topic.id, code_style()),
        spaces(pad - topic.id.width()),
      ];
      let rest = vec![spaces(2 + pad)];
      out.extend(wrap(vec![plain(&topic.summary)], width, first, rest));
    }
  }
  out.push(vec![]);
  out.extend(wrap(
    vec![plain(
      "dekit <command> --help shows a command's flags only; dekit help \
       <command> shows its page.",
    )],
    width,
    vec![],
    vec![],
  ));
  out
}

fn with(mut attrs: Attrs, change: impl FnOnce(&mut Attrs) -> Attrs) -> Attrs {
  change(&mut attrs)
}

fn inlines(inlines: &[Inline], base: Attrs) -> Vec<Span> {
  let mut out = Vec::new();
  for inline in inlines {
    match inline {
      Inline::Text { text } => out.push(span(text, base)),
      Inline::Code { text } => {
        out.push(span(text, with(base, |a| a.fg(Color::CYAN))))
      }
      Inline::Strong { children } => {
        out.extend(self::inlines(children, with(base, |a| a.set_bold(true))))
      }
      Inline::Em { children } => {
        out.extend(self::inlines(children, with(base, |a| a.set_italic(true))))
      }
      Inline::TagLink { tag } => {
        out.push(span(tag, with(base, |a| a.set_underline(true))))
      }
      Inline::UrlLink { href, children } => {
        out.extend(self::inlines(
          children,
          with(base, |a| a.set_underline(true)),
        ));
        out.push(span(format!(" ({href})"), dim()));
      }
    }
  }
  out
}

fn blocks(blocks: &[Block], width: usize, out: &mut Vec<Line>) {
  for (i, block) in blocks.iter().enumerate() {
    if i > 0 {
      out.push(vec![]);
    }
    match block {
      Block::Heading { level, inlines, .. } => {
        let attrs = if *level == 2 {
          bold()
        } else {
          bold().set_italic(true)
        };
        out.extend(wrap(self::inlines(inlines, attrs), width, vec![], vec![]));
      }
      Block::Paragraph { inlines } => {
        out.extend(wrap(
          self::inlines(inlines, Attrs::default()),
          width,
          vec![],
          vec![],
        ));
      }
      Block::Code { text, .. } => {
        for line in text.lines() {
          out.push(vec![plain("    "), plain(line)]);
        }
      }
      Block::List { ordered, items } => {
        for (n, item) in items.iter().enumerate() {
          let marker = if *ordered {
            format!("  {}. ", n + 1)
          } else {
            "  - ".to_string()
          };
          let indent = spaces(marker.width());
          out.extend(wrap(
            self::inlines(item, Attrs::default()),
            width,
            vec![plain(marker)],
            vec![indent],
          ));
        }
      }
      Block::Callout {
        variant,
        title,
        blocks,
      } => {
        let color = match variant {
          CalloutVariant::Note => Color::BLUE,
          CalloutVariant::Tip => Color::GREEN,
          CalloutVariant::Warning => Color::YELLOW,
        };
        let bar = span("│ ", Attrs::default().fg(color));
        let label =
          title.clone().unwrap_or_else(|| variant.label().to_string());
        out.push(vec![bar.clone(), span(label, bold().fg(color))]);
        let mut inner = Vec::new();
        self::blocks(blocks, width.saturating_sub(2), &mut inner);
        for line in inner {
          let mut prefixed = vec![bar.clone()];
          prefixed.extend(line);
          out.push(prefixed);
        }
      }
      Block::Commands { items } => {
        for item in items {
          out.push(vec![plain("  "), span(&item.cmd, bold())]);
          out.extend(wrap(
            vec![plain(&item.desc)],
            width,
            vec![spaces(6)],
            vec![spaces(6)],
          ));
        }
      }
      Block::Fields { kind, items } => {
        for item in items {
          let mut head = vec![plain("  "), span(&item.key, bold())];
          match kind {
            FieldKind::Js => {
              if let Some(signature) = &item.signature {
                head.push(plain("  "));
                head.push(span(signature, dim()));
              }
            }
            FieldKind::Config | FieldKind::CliFlag => {
              if let Some(ty) = &item.ty {
                head.push(plain("  "));
                head.push(span(ty, dim()));
              }
              if item.required == Some(true) {
                head.push(span("  required", dim()));
              } else if let Some(default) = &item.default {
                head.push(span(format!("  = {default}"), dim()));
              }
            }
          }
          out.push(head);
          out.extend(wrap(
            vec![plain(&item.desc)],
            width,
            vec![spaces(6)],
            vec![spaces(6)],
          ));
        }
      }
      Block::Footnote { inlines } => {
        out.extend(wrap(self::inlines(inlines, dim()), width, vec![], vec![]));
      }
      Block::Image { alt, .. } => {
        out.push(vec![span(format!("[image: {alt}]"), dim())]);
      }
    }
  }
}

struct Word {
  spans: Vec<Span>,
  width: usize,
}

fn words(spans: Vec<Span>) -> Vec<Word> {
  let mut words = Vec::new();
  let mut current: Vec<Span> = Vec::new();
  let finish = |current: &mut Vec<Span>, words: &mut Vec<Word>| {
    if !current.is_empty() {
      let spans = std::mem::take(current);
      let width = width_of(&spans);
      words.push(Word { spans, width });
    }
  };
  for Span { text, attrs } in spans {
    let mut piece = String::new();
    for c in text.chars() {
      if c == ' ' {
        if !piece.is_empty() {
          current.push(span(std::mem::take(&mut piece), attrs));
        }
        finish(&mut current, &mut words);
      } else {
        piece.push(c);
      }
    }
    if !piece.is_empty() {
      current.push(span(piece, attrs));
    }
  }
  finish(&mut current, &mut words);
  words
}

/// Greedy word wrap. A word wider than the line stands alone, unbroken.
pub fn wrap(
  spans: Vec<Span>,
  width: usize,
  first: Vec<Span>,
  rest: Vec<Span>,
) -> Vec<Line> {
  let words = words(spans);
  if words.is_empty() {
    return Vec::new();
  }
  let mut lines = Vec::new();
  let mut line = first;
  let mut col = width_of(&line);
  let mut has_word = false;
  for word in words {
    if has_word && col + 1 + word.width > width {
      lines.push(std::mem::replace(&mut line, rest.clone()));
      col = width_of(&line);
      has_word = false;
    }
    if has_word {
      // The space keeps the previous attrs so a styled run stays one run.
      let attrs = line.last().map(|s| s.attrs).unwrap_or_default();
      line.push(span(" ", attrs));
      col += 1;
    }
    col += word.width;
    line.extend(word.spans);
    has_word = true;
  }
  lines.push(line);
  lines
}
