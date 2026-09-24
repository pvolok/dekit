//! Topics as markdown: what `dekit help` prints when stdout is not a
//! terminal, which is what an agent reads.

use super::ir::{Block, DocsIr, FieldKind, Inline, Topic};

pub fn topic(topic: &Topic, docs: &DocsIr) -> String {
  let mut out = format!("# {}\n", topic.title);
  if !topic.summary.is_empty() {
    out.push_str(&format!("\n{}\n", topic.summary));
  }
  blocks(&topic.blocks, "", &mut out);
  let related: Vec<&Topic> = topic
    .related
    .iter()
    .filter_map(|id| docs.topic(id))
    .collect();
  if !related.is_empty() {
    out.push_str("\n## Related\n\n");
    for related in related {
      out.push_str(&format!(
        "- `dekit help {}` — {}\n",
        related.id, related.title
      ));
    }
  }
  out
}

pub fn index(docs: &DocsIr) -> String {
  let mut out = String::from(
    "# dekit help\n\nDocumentation topics. Open one with `dekit help <topic>`; \
     a command's name, a page name, or a tag all work.\n",
  );
  for section in &docs.nav {
    out.push_str(&format!("\n## {}\n\n", section.title));
    for topic in section.topics.iter().filter_map(|id| docs.topic(id)) {
      out.push_str(&format!("- `{}` — {}\n", topic.id, topic.summary));
    }
  }
  out.push_str(
    "\n`dekit <command> --help` shows a command's flags only; \
     `dekit help <command>` shows its page.\n",
  );
  out
}

fn inlines(inlines: &[Inline]) -> String {
  let mut out = String::new();
  for inline in inlines {
    match inline {
      Inline::Text { text } => out.push_str(text),
      Inline::Code { text } => out.push_str(&code(text)),
      Inline::Strong { children } => {
        out.push_str(&format!("**{}**", self::inlines(children)))
      }
      Inline::Em { children } => {
        out.push_str(&format!("_{}_", self::inlines(children)))
      }
      Inline::TagLink { tag } => out.push_str(&code(tag)),
      Inline::UrlLink { href, children } => {
        out.push_str(&format!("[{}]({href})", self::inlines(children)))
      }
    }
  }
  out
}

fn code(text: &str) -> String {
  if text.contains('`') {
    format!("`` {text} ``")
  } else {
    format!("`{text}`")
  }
}

fn cell(text: &str) -> String {
  text.replace('|', "\\|")
}

fn blocks(blocks: &[Block], quote: &str, out: &mut String) {
  for block in blocks {
    out.push_str(quote.trim_end());
    out.push('\n');
    match block {
      Block::Heading { level, inlines, .. } => {
        let hashes = "#".repeat(*level as usize);
        out.push_str(&format!("{quote}{hashes} {}\n", self::inlines(inlines)));
      }
      Block::Paragraph { inlines } => {
        out.push_str(&format!("{quote}{}\n", self::inlines(inlines)));
      }
      Block::Code { lang, text } => {
        out.push_str(&format!("{quote}```{}\n", lang.as_deref().unwrap_or("")));
        for line in text.lines() {
          out.push_str(&format!("{quote}{line}\n"));
        }
        out.push_str(&format!("{quote}```\n"));
      }
      Block::List { ordered, items } => {
        for (n, item) in items.iter().enumerate() {
          let marker = if *ordered {
            format!("{}.", n + 1)
          } else {
            "-".to_string()
          };
          out.push_str(&format!("{quote}{marker} {}\n", self::inlines(item)));
        }
      }
      Block::Callout {
        variant,
        title,
        blocks,
      } => {
        let label =
          title.clone().unwrap_or_else(|| variant.label().to_string());
        out.push_str(&format!("{quote}> **{label}**\n"));
        let inner = format!("{quote}> ");
        self::blocks(blocks, &inner, out);
      }
      Block::Commands { items } => {
        out.push_str(&format!("{quote}| Command | Description |\n"));
        out.push_str(&format!("{quote}| --- | --- |\n"));
        for item in items {
          out.push_str(&format!(
            "{quote}| {} | {} |\n",
            code(&cell(&item.cmd)),
            cell(&item.desc)
          ));
        }
      }
      Block::Fields { kind, items } => {
        let (header, rule) = match kind {
          FieldKind::Config => (
            "| Key | Type | Default | Description |",
            "| --- | --- | --- | --- |",
          ),
          FieldKind::Js => {
            ("| API | Signature | Description |", "| --- | --- | --- |")
          }
          FieldKind::CliFlag => ("| Flag | Description |", "| --- | --- |"),
        };
        out.push_str(&format!("{quote}{header}\n{quote}{rule}\n"));
        for item in items {
          let key = code(&cell(&item.key));
          let desc = cell(&item.desc);
          let row = match kind {
            FieldKind::Config => {
              let default = if item.required == Some(true) {
                "required".to_string()
              } else {
                item.default.as_deref().map(cell).unwrap_or_default()
              };
              let ty = item.ty.as_deref().map(cell).unwrap_or_default();
              format!("| {key} | {ty} | {default} | {desc} |")
            }
            FieldKind::Js => {
              let signature = item
                .signature
                .as_deref()
                .map(|s| code(&cell(s)))
                .unwrap_or_default();
              format!("| {key} | {signature} | {desc} |")
            }
            FieldKind::CliFlag => format!("| {key} | {desc} |"),
          };
          out.push_str(&format!("{quote}{row}\n"));
        }
      }
      Block::Footnote { inlines } => {
        out.push_str(&format!("{quote}{}\n", self::inlines(inlines)));
      }
      Block::Image { src, alt, .. } => {
        out.push_str(&format!("{quote}![{alt}]({src})\n"));
      }
    }
  }
}
