//! The `dekit help` subcommand.

use std::io::{IsTerminal, Write};

use anyhow::anyhow;

use super::ir::{Block, DocsIr, Topic};
use super::{layout, markdown, term};

pub fn run(words: &[String], json: bool) -> anyhow::Result<()> {
  let docs = super::load().map_err(|errors| {
    anyhow!(
      "the embedded docs failed to compile:\n{}",
      super::error::join(&errors)
    )
  })?;
  let query = words.join(" ");
  let query = query.trim();

  if query.is_empty() {
    if json {
      return print_json(&docs.export());
    }
    return show(
      || markdown::index(&docs),
      |width| layout::index(&docs, width),
    );
  }

  let Some(topic) = resolve(&docs, query) else {
    eprintln!("no help topic '{query}'.");
    let hits = search(&docs, query);
    if !hits.is_empty() {
      eprintln!("Did you mean:");
      for topic in hits {
        eprintln!("  {:<28} {}", topic.id, topic.title);
      }
    }
    eprintln!("Run `dekit help` for the index.");
    std::process::exit(1);
  };
  if json {
    return print_json(topic);
  }
  show(
    || markdown::topic(topic, &docs),
    |width| layout::topic(topic, &docs, width),
  )
}

/// A query is a command (`up`, `runner stop`), a topic id, an alias or
/// record tag, a unique last path segment, or a title.
pub fn resolve<'a>(docs: &'a DocsIr, query: &str) -> Option<&'a Topic> {
  if let Some(target) = docs.tags.get(query) {
    return docs.topic(&target.topic);
  }
  let query = query.split_once('#').map(|(head, _)| head).unwrap_or(query);
  let as_cli = format!("dekit {query}");
  docs
    .topics
    .iter()
    .find(|t| {
      t.cli.as_deref() == Some(&as_cli) || t.cli.as_deref() == Some(query)
    })
    .or_else(|| docs.topic(query))
    .or_else(|| {
      docs
        .tags
        .get(query)
        .and_then(|target| docs.topic(&target.topic))
    })
    .or_else(|| {
      let mut matches = docs
        .topics
        .iter()
        .filter(|t| t.id.rsplit('/').next() == Some(query));
      match (matches.next(), matches.next()) {
        (Some(topic), None) => Some(topic),
        _ => None,
      }
    })
    .or_else(|| {
      docs
        .topics
        .iter()
        .find(|t| t.title.eq_ignore_ascii_case(query))
    })
}

/// Visible topics whose id, title, tags, headings, command, or record
/// keys mention the query.
pub fn search<'a>(docs: &'a DocsIr, query: &str) -> Vec<&'a Topic> {
  let needle = query.to_lowercase();
  docs
    .topics
    .iter()
    .filter(|t| !t.hidden)
    .filter(|t| {
      t.id.contains(&needle)
        || t.title.to_lowercase().contains(&needle)
        || t.tags.iter().any(|tag| tag.contains(&needle))
        || t
          .headings
          .iter()
          .any(|h| h.text.to_lowercase().contains(&needle))
        || t.cli.as_deref().is_some_and(|cli| cli.contains(&needle))
        || record_keys(&t.blocks)
          .any(|key| key.to_lowercase().contains(&needle))
    })
    .collect()
}

fn record_keys(blocks: &[Block]) -> impl Iterator<Item = &str> {
  blocks
    .iter()
    .flat_map(|block| -> Box<dyn Iterator<Item = &str>> {
      match block {
        Block::Commands { items } => {
          Box::new(items.iter().map(|c| c.cmd.as_str()))
        }
        Block::Fields { items, .. } => {
          Box::new(items.iter().map(|f| f.key.as_str()))
        }
        Block::Callout { blocks, .. } => Box::new(record_keys(blocks)),
        Block::Heading { .. }
        | Block::Paragraph { .. }
        | Block::Code { .. }
        | Block::List { .. }
        | Block::Footnote { .. }
        | Block::Image { .. } => Box::new(std::iter::empty()),
      }
    })
}

fn print_json<T: serde::Serialize>(value: &T) -> anyhow::Result<()> {
  println!("{}", serde_json::to_string(value)?);
  Ok(())
}

/// Markdown when piped; styled, wrapped, and paged on a terminal.
fn show(
  as_markdown: impl FnOnce() -> String,
  as_lines: impl FnOnce(usize) -> Vec<layout::Line>,
) -> anyhow::Result<()> {
  if !std::io::stdout().is_terminal() {
    print!("{}", as_markdown());
    return Ok(());
  }
  let width = terminal_width().unwrap_or(80).min(layout::MAX_WIDTH);
  let text = term::render(&as_lines(width), color_enabled());
  page(&text)
}

fn color_enabled() -> bool {
  let no_color = std::env::var_os("NO_COLOR").is_some_and(|v| !v.is_empty());
  let dumb = std::env::var("TERM").is_ok_and(|term| term == "dumb");
  !no_color && !dumb
}

/// Writes through `$PAGER` (default `less`). `LESS=FRX` when unset, as
/// git does, so one screen prints without waiting for `q`.
fn page(text: &str) -> anyhow::Result<()> {
  let pager = std::env::var("PAGER").unwrap_or_else(|_| "less".to_string());
  let mut parts = pager.split_whitespace();
  let program = parts.next().unwrap_or("");
  if program.is_empty() || program == "cat" {
    print!("{text}");
    return Ok(());
  }
  let mut command = std::process::Command::new(program);
  command.args(parts).stdin(std::process::Stdio::piped());
  if std::env::var_os("LESS").is_none() {
    command.env("LESS", "FRX");
  }
  let mut child = match command.spawn() {
    Ok(child) => child,
    Err(_) => {
      print!("{text}");
      return Ok(());
    }
  };
  if let Some(mut stdin) = child.stdin.take() {
    // The pager may quit before reading everything; that is not an error.
    let _ = stdin.write_all(text.as_bytes());
  }
  child.wait()?;
  Ok(())
}

#[cfg(unix)]
fn terminal_width() -> Option<usize> {
  let size = rustix::termios::tcgetwinsize(std::io::stdout()).ok()?;
  (size.ws_col > 0).then_some(size.ws_col as usize)
}

#[cfg(windows)]
fn terminal_width() -> Option<usize> {
  use std::os::windows::io::AsRawHandle;

  use ::windows::Win32::{
    Foundation::HANDLE,
    System::Console::{CONSOLE_SCREEN_BUFFER_INFO, GetConsoleScreenBufferInfo},
  };

  let mut info: CONSOLE_SCREEN_BUFFER_INFO = Default::default();
  unsafe {
    GetConsoleScreenBufferInfo(
      HANDLE(std::io::stdout().as_raw_handle()),
      &mut info,
    )
    .ok()?;
  }
  let width = info.srWindow.Right - info.srWindow.Left + 1;
  (width > 0).then_some(width as usize)
}
