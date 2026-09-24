use std::collections::{BTreeMap, HashSet};

use serde_yaml::{Mapping, Value};

use super::error::{DocError, err};
use super::generate;
use super::ir::{
  Block, CommandRecord, DocsIr, FieldKind, FieldRecord, Heading, Inline,
  NavSection, TagTarget, Topic, plain_text,
};
use super::parse::{ParseCtx, RawBlock, parse_blocks, split_frontmatter};
use super::patterns::{
  MAX_FILE_BYTES, is_ident, is_record_tag, is_slug_segment,
};

const FRONTMATTER_KEYS: &[&str] = &[
  "title", "summary", "cli", "tags", "related", "order", "hidden",
];
const MANIFEST: &str = "index.yaml";

struct Section {
  dir: String,
  title: String,
}

struct ParsedTopic {
  path: String,
  id: String,
  section: String,
  title: String,
  summary: Option<String>,
  cli: Option<String>,
  tags: Vec<String>,
  related: Vec<String>,
  order: f64,
  hidden: bool,
  blocks: Vec<RawBlock>,
  fm_line: usize,
}

/// Compiles topic sources (`docs/`-relative path, text) and the manifest
/// into the tree. Generated blocks read the clap tree.
pub fn compile(
  sources: &[(&str, &str)],
  manifest: &str,
  cli: &clap::Command,
) -> Result<DocsIr, Vec<DocError>> {
  let mut errors = Vec::new();
  let sections = parse_manifest(manifest, &mut errors);

  let mut parsed: Vec<ParsedTopic> = Vec::new();
  for (path, source) in sources {
    if path.ends_with("README.md") {
      continue;
    }
    if let Some(topic) = parse_topic(path, source, cli, &mut errors) {
      if let Some(other) = parsed.iter().find(|t| t.id == topic.id) {
        errors.push(err(
          path,
          1,
          format!("duplicate topic \"{}\" (also {})", topic.id, other.path),
        ));
        continue;
      }
      if let Some(cli_path) = &topic.cli
        && let Some(other) =
          parsed.iter().find(|t| t.cli.as_ref() == Some(cli_path))
      {
        errors.push(err(
          path,
          topic.fm_line,
          format!("cli \"{cli_path}\" is already documented by {}", other.path),
        ));
      }
      parsed.push(topic);
    }
  }

  for topic in &parsed {
    if !topic.section.is_empty()
      && !sections.iter().any(|section| section.dir == topic.section)
    {
      errors.push(err(
        &topic.path,
        1,
        format!("section \"{}\" is not in {MANIFEST}", topic.section),
      ));
    }
  }
  for section in &sections {
    if !parsed.iter().any(|topic| topic.section == section.dir) {
      errors.push(err(
        MANIFEST,
        1,
        format!("section \"{}\" has no topics", section.dir),
      ));
    }
  }

  let mut built = generate::prepare(cli);
  let mut tags: BTreeMap<String, (TagTarget, String, usize)> = BTreeMap::new();
  let mut topics = Vec::new();
  for topic in parsed.iter_mut() {
    let raw = std::mem::take(&mut topic.blocks);
    let mut cx = TopicCx {
      path: &topic.path,
      id: &topic.id,
      cli: topic.cli.as_deref(),
      heading_ids: HashSet::new(),
      headings: Vec::new(),
      tags: Vec::new(),
      has_usage: false,
      built: &mut built,
    };
    let mut blocks = Vec::new();
    for block in raw {
      convert(block, &mut cx, &mut blocks, &mut errors);
    }

    let footnotes = blocks
      .iter()
      .filter(|block| matches!(block, Block::Footnote { .. }))
      .count();
    if footnotes > 1 {
      errors.push(err(&topic.path, 1, "at most one :::footnote per topic"));
    }
    if footnotes == 1 && !matches!(blocks.last(), Some(Block::Footnote { .. }))
    {
      errors.push(err(&topic.path, 1, ":::footnote must be the last block"));
    }
    if topic.cli.is_some() && !cx.has_usage {
      errors.push(err(
        &topic.path,
        topic.fm_line,
        "a cli topic must contain :::usage",
      ));
    }

    let headings = std::mem::take(&mut cx.headings);
    let record_tags = std::mem::take(&mut cx.tags);
    drop(cx);
    for (tag, target, line) in record_tags {
      register_tag(&mut tags, tag, target, &topic.path, line, &mut errors);
    }
    register_tag(
      &mut tags,
      topic.id.clone(),
      TagTarget {
        topic: topic.id.clone(),
        heading: None,
      },
      &topic.path,
      topic.fm_line,
      &mut errors,
    );
    for alias in &topic.tags {
      if alias == &topic.id {
        errors.push(err(
          &topic.path,
          topic.fm_line,
          "do not repeat the id in tags",
        ));
        continue;
      }
      register_tag(
        &mut tags,
        alias.clone(),
        TagTarget {
          topic: topic.id.clone(),
          heading: None,
        },
        &topic.path,
        topic.fm_line,
        &mut errors,
      );
    }

    let summary = match (&topic.cli, &topic.summary) {
      (Some(cli_path), _) => generate::words(cli_path)
        .and_then(|words| generate::find(cli, &words))
        .map(generate::about)
        .unwrap_or_default(),
      (None, Some(summary)) => summary.clone(),
      (None, None) => String::new(),
    };
    let mut all_tags = vec![topic.id.clone()];
    all_tags.extend(topic.tags.iter().cloned());
    topics.push(Topic {
      id: topic.id.clone(),
      title: topic.title.clone(),
      slug: topic.id.clone(),
      section: topic.section.clone(),
      summary,
      tags: all_tags,
      related: topic.related.clone(),
      headings,
      blocks,
      source: topic.path.clone(),
      hidden: topic.hidden,
      cli: topic.cli.clone(),
    });
  }

  match topics.iter().find(|topic| topic.id == "index") {
    None => {
      errors.push(err("index.md", 1, "the home topic index.md is missing"))
    }
    Some(home) if home.hidden => {
      errors.push(err(&home.source, 1, "the home topic cannot be hidden"))
    }
    Some(_) => {}
  }

  let mut nav = Vec::new();
  for section in &sections {
    let mut members: Vec<(&ParsedTopic, &Topic)> = parsed
      .iter()
      .zip(topics.iter())
      .filter(|(p, t)| p.section == section.dir && !t.hidden)
      .collect();
    members.sort_by(|(a, _), (b, _)| {
      a.order
        .partial_cmp(&b.order)
        .unwrap_or(std::cmp::Ordering::Equal)
        .then_with(|| a.path.cmp(&b.path))
    });
    if members.is_empty() {
      continue;
    }
    nav.push(NavSection {
      id: section.dir.clone(),
      title: section.title.clone(),
      topics: members.iter().map(|(_, t)| t.id.clone()).collect(),
    });
  }

  // Home first, then nav order; hidden topics keep their source order.
  let visible_order: Vec<String> = std::iter::once("index".to_string())
    .chain(
      nav
        .iter()
        .flat_map(|section| section.topics.iter().cloned()),
    )
    .collect();
  topics.sort_by_key(|topic| {
    visible_order
      .iter()
      .position(|id| id == &topic.id)
      .unwrap_or(usize::MAX)
  });

  let tag_targets: BTreeMap<String, TagTarget> = tags
    .iter()
    .map(|(tag, (target, _, _))| (tag.clone(), target.clone()))
    .collect();
  let hidden = |id: &str| topics.iter().any(|t| t.id == id && t.hidden);

  let mut commands = Vec::new();
  let mut fields = Vec::new();
  let mut command_keys = HashSet::new();
  let mut field_keys = HashSet::new();
  let home_first = std::iter::once("index").chain(
    nav
      .iter()
      .flat_map(|section| section.topics.iter().map(String::as_str)),
  );
  for id in home_first {
    let Some(topic) = topics.iter().find(|t| t.id == id) else {
      continue;
    };
    collect_catalogs(
      &topic.blocks,
      &mut commands,
      &mut fields,
      &mut command_keys,
      &mut field_keys,
    );
  }

  for topic in &topics {
    let mut links = Vec::new();
    collect_tag_links(&topic.blocks, &mut links);
    for tag in links {
      match tag_targets.get(&tag) {
        None => {
          errors.push(err(&topic.source, 1, format!("unknown tag \"{tag}\"")))
        }
        Some(target) if !topic.hidden && hidden(&target.topic) => {
          errors.push(err(
            &topic.source,
            1,
            format!("\"{tag}\" is hidden; a visible page cannot link to it"),
          ))
        }
        Some(_) => {}
      }
    }
    for related in &topic.related {
      if !topics.iter().any(|t| &t.id == related) {
        errors.push(err(
          &topic.source,
          1,
          format!("related topic \"{related}\" does not exist"),
        ));
      } else if !topic.hidden && hidden(related) {
        errors.push(err(
          &topic.source,
          1,
          format!("related topic \"{related}\" is hidden"),
        ));
      }
    }
  }

  if !errors.is_empty() {
    errors.sort_by(|a, b| a.path.cmp(&b.path).then(a.line.cmp(&b.line)));
    errors.dedup();
    return Err(errors);
  }

  Ok(DocsIr {
    version: 1,
    dekit: env!("CARGO_PKG_VERSION").to_string(),
    home: "index".to_string(),
    topics,
    nav,
    tags: tag_targets,
    commands,
    fields,
  })
}

fn register_tag(
  tags: &mut BTreeMap<String, (TagTarget, String, usize)>,
  tag: String,
  target: TagTarget,
  path: &str,
  line: usize,
  errors: &mut Vec<DocError>,
) {
  if let Some((_, other_path, other_line)) = tags.get(&tag) {
    errors.push(err(
      path,
      line,
      format!("duplicate tag \"{tag}\" (also {other_path}:{other_line})"),
    ));
    return;
  }
  tags.insert(tag, (target, path.to_string(), line));
}

struct TopicCx<'a> {
  path: &'a str,
  id: &'a str,
  cli: Option<&'a str>,
  heading_ids: HashSet<String>,
  headings: Vec<Heading>,
  tags: Vec<(String, TagTarget, usize)>,
  has_usage: bool,
  built: &'a mut clap::Command,
}

fn convert(
  raw: RawBlock,
  cx: &mut TopicCx<'_>,
  out: &mut Vec<Block>,
  errors: &mut Vec<DocError>,
) {
  match raw {
    RawBlock::Heading {
      level,
      explicit_id,
      inlines,
      line,
    } => {
      let text = plain_text(&inlines);
      let id = match explicit_id {
        Some(id) => {
          if !is_ident(&id) {
            errors.push(err(
              cx.path,
              line,
              format!("heading id \"{id}\" must be lowercase letters, digits, and hyphens"),
            ));
          }
          id
        }
        None => match slugify(&text) {
          Some(id) => id,
          None => {
            errors.push(err(
              cx.path,
              line,
              "heading has no slugifiable text; add {#id}",
            ));
            "missing".to_string()
          }
        },
      };
      if !cx.heading_ids.insert(id.clone()) {
        errors.push(err(
          cx.path,
          line,
          format!("duplicate heading id \"{id}\"; add {{#id}}"),
        ));
      }
      cx.headings.push(Heading {
        id: id.clone(),
        level,
        text,
      });
      cx.tags.push((
        format!("{}#{id}", cx.id),
        TagTarget {
          topic: cx.id.to_string(),
          heading: Some(id.clone()),
        },
        line,
      ));
      out.push(Block::Heading { level, id, inlines });
    }
    RawBlock::Paragraph { inlines } => out.push(Block::Paragraph { inlines }),
    RawBlock::Code { lang, text } => out.push(Block::Code { lang, text }),
    RawBlock::List { ordered, items } => {
      out.push(Block::List { ordered, items })
    }
    RawBlock::Callout {
      variant,
      title,
      blocks,
    } => {
      let mut inner = Vec::new();
      for block in blocks {
        convert(block, cx, &mut inner, errors);
      }
      out.push(Block::Callout {
        variant,
        title,
        blocks: inner,
      });
    }
    RawBlock::Commands { yaml, line } => {
      let items = parse_commands(&yaml, cx.path, line, cx.id, errors);
      for item in &items {
        if let Some(tag) = &item.tag {
          cx.tags.push((
            tag.clone(),
            TagTarget {
              topic: cx.id.to_string(),
              heading: None,
            },
            line,
          ));
        }
      }
      out.push(Block::Commands { items });
    }
    RawBlock::Fields { kind, yaml, line } => {
      let items = parse_fields(&yaml, cx.path, line, cx.id, kind, errors);
      for item in &items {
        if let Some(tag) = &item.tag {
          cx.tags.push((
            tag.clone(),
            TagTarget {
              topic: cx.id.to_string(),
              heading: None,
            },
            line,
          ));
        }
      }
      out.push(Block::Fields { kind, items });
    }
    RawBlock::Footnote { inlines, line: _ } => {
      out.push(Block::Footnote { inlines })
    }
    RawBlock::Image { src, alt, line } => {
      if !(src.starts_with("https://") || src.starts_with("http://")) {
        errors.push(err(
          cx.path,
          line,
          "image src must be an https:// or http:// URL",
        ));
      }
      out.push(Block::Image {
        src,
        alt,
        web_only: true,
      });
    }
    RawBlock::Usage { line } => {
      cx.has_usage = true;
      match cx.cli {
        Some(cli) => out.extend(generate::usage(cx.built, cli, cx.id)),
        None => errors.push(err(
          cx.path,
          line,
          ":::usage needs a cli: key in the frontmatter",
        )),
      }
    }
  }
}

/// Heading id from its text: ASCII letters, digits, spaces, and hyphens
/// survive; everything else is dropped.
pub fn slugify(text: &str) -> Option<String> {
  let kept: String = text
    .chars()
    .filter(|c| c.is_ascii_alphanumeric() || *c == ' ' || *c == '-')
    .collect::<String>()
    .to_ascii_lowercase();
  let mut out = String::new();
  let mut pending_dash = false;
  for c in kept.chars() {
    if c == ' ' || c == '-' {
      pending_dash = !out.is_empty();
    } else {
      if pending_dash {
        out.push('-');
        pending_dash = false;
      }
      out.push(c);
    }
  }
  if out.is_empty() {
    return None;
  }
  if out.starts_with(|c: char| c.is_ascii_digit()) {
    out.insert_str(0, "h-");
  }
  Some(out)
}

fn collect_catalogs(
  blocks: &[Block],
  commands: &mut Vec<CommandRecord>,
  fields: &mut Vec<FieldRecord>,
  command_keys: &mut HashSet<String>,
  field_keys: &mut HashSet<String>,
) {
  for block in blocks {
    match block {
      Block::Commands { items } => {
        for item in items {
          let key = item.tag.clone().unwrap_or_else(|| item.cmd.clone());
          if command_keys.insert(key) {
            commands.push(item.clone());
          }
        }
      }
      Block::Fields { items, .. } => {
        for item in items {
          let key = item
            .tag
            .clone()
            .unwrap_or_else(|| format!("{}:{}", item.kind.name(), item.key));
          if field_keys.insert(key) {
            fields.push(item.clone());
          }
        }
      }
      Block::Callout { blocks, .. } => {
        collect_catalogs(blocks, commands, fields, command_keys, field_keys)
      }
      Block::Heading { .. }
      | Block::Paragraph { .. }
      | Block::Code { .. }
      | Block::List { .. }
      | Block::Footnote { .. }
      | Block::Image { .. } => {}
    }
  }
}

fn collect_tag_links(blocks: &[Block], out: &mut Vec<String>) {
  fn walk_inlines(inlines: &[Inline], out: &mut Vec<String>) {
    for inline in inlines {
      match inline {
        Inline::TagLink { tag } => out.push(tag.clone()),
        Inline::Strong { children }
        | Inline::Em { children }
        | Inline::UrlLink { children, .. } => walk_inlines(children, out),
        Inline::Text { .. } | Inline::Code { .. } => {}
      }
    }
  }
  for block in blocks {
    match block {
      Block::Heading { inlines, .. }
      | Block::Paragraph { inlines }
      | Block::Footnote { inlines } => walk_inlines(inlines, out),
      Block::List { items, .. } => {
        for item in items {
          walk_inlines(item, out);
        }
      }
      Block::Callout { blocks, .. } => collect_tag_links(blocks, out),
      Block::Code { .. }
      | Block::Commands { .. }
      | Block::Fields { .. }
      | Block::Image { .. } => {}
    }
  }
}

/// `cli/up.md` is `cli/up` in section `cli`; `cli/index.md` is `cli`;
/// `index.md` is the home.
fn topic_id(path: &str) -> Result<(String, String), String> {
  let Some(stem) = path.strip_suffix(".md") else {
    return Err("topic files must end in .md".to_string());
  };
  let segments: Vec<&str> = stem.split('/').collect();
  for segment in &segments {
    if !is_slug_segment(segment) {
      return Err(format!(
        "path segment \"{segment}\" must be lowercase letters, digits, and hyphens"
      ));
    }
  }
  if segments.len() == 1 {
    if stem == "index" {
      return Ok(("index".to_string(), String::new()));
    }
    return Err(
      "only index.md may sit at the top level; put the topic in a section directory"
        .to_string(),
    );
  }
  let section = segments[0].to_string();
  let id = if segments[segments.len() - 1] == "index" {
    segments[..segments.len() - 1].join("/")
  } else {
    stem.to_string()
  };
  Ok((id, section))
}

fn parse_topic(
  path: &str,
  source: &str,
  cli: &clap::Command,
  errors: &mut Vec<DocError>,
) -> Option<ParsedTopic> {
  let (id, section) = match topic_id(path) {
    Ok(parts) => parts,
    Err(message) => {
      errors.push(err(path, 1, message));
      return None;
    }
  };
  let split = match split_frontmatter(source, path) {
    Ok(split) => split,
    Err(error) => {
      errors.push(error);
      return None;
    }
  };
  let fm_line = split.fm_start_line;
  let yaml = parse_yaml(&split.fm_lines.join("\n"), path, fm_line, errors)?;
  let map = as_mapping(&yaml, path, fm_line, errors)?;
  for key in map.keys() {
    let key = key.as_str().unwrap_or_default();
    if !FRONTMATTER_KEYS.contains(&key) {
      errors.push(err(
        path,
        fm_line,
        format!("unknown frontmatter key \"{key}\""),
      ));
    }
  }
  let title = required_string(map, "title", path, fm_line, errors);
  let summary = optional_string(map, "summary", path, fm_line, errors);
  let cli_path = optional_string(map, "cli", path, fm_line, errors);
  let tags = string_list(map, "tags", path, fm_line, errors);
  let related = string_list(map, "related", path, fm_line, errors);
  for tag in &tags {
    if !is_ident(tag) {
      errors.push(err(
        path,
        fm_line,
        format!("tag \"{tag}\" must be lowercase letters, digits, and hyphens"),
      ));
    }
  }
  let order = match map.get("order") {
    None => 0.0,
    Some(value) => match value.as_f64() {
      Some(order) => order,
      None => {
        errors.push(err(path, fm_line, "order must be a number"));
        0.0
      }
    },
  };
  let hidden = match map.get("hidden") {
    None => false,
    Some(value) => match value.as_bool() {
      Some(hidden) => hidden,
      None => {
        errors.push(err(path, fm_line, "hidden must be a boolean"));
        false
      }
    },
  };
  match (&cli_path, &summary) {
    (Some(_), Some(_)) => errors.push(err(
      path,
      fm_line,
      "summary is forbidden on a cli topic; the command's about line is its summary",
    )),
    (None, None) => errors.push(err(path, fm_line, "summary is required")),
    _ => {}
  }
  if let Some(cli_path) = &cli_path {
    match generate::words(cli_path) {
      None => errors.push(err(
        path,
        fm_line,
        "cli must be \"dekit\" or \"dekit <command> ...\"",
      )),
      Some(words) => {
        if generate::find(cli, &words).is_none() {
          errors.push(err(
            path,
            fm_line,
            format!("cli \"{cli_path}\" is not a dekit command"),
          ));
        }
      }
    }
  }
  let title = title?;
  let ctx = ParseCtx {
    path,
    topic_title: &title,
    allow_containers: true,
  };
  let blocks =
    parse_blocks(&split.body_lines, split.body_start_line, &ctx, errors);
  Some(ParsedTopic {
    path: path.to_string(),
    id,
    section,
    title,
    summary,
    cli: cli_path,
    tags,
    related,
    order,
    hidden,
    blocks,
    fm_line,
  })
}

fn parse_manifest(source: &str, errors: &mut Vec<DocError>) -> Vec<Section> {
  if source.len() > MAX_FILE_BYTES {
    errors.push(err(
      MANIFEST,
      1,
      format!("file exceeds {MAX_FILE_BYTES} byte cap"),
    ));
    return Vec::new();
  }
  let Some(yaml) = parse_yaml(source, MANIFEST, 1, errors) else {
    return Vec::new();
  };
  let Some(map) = as_mapping(&yaml, MANIFEST, 1, errors) else {
    return Vec::new();
  };
  for key in map.keys() {
    let key = key.as_str().unwrap_or_default();
    if key != "sections" {
      errors.push(err(MANIFEST, 1, format!("unknown manifest key \"{key}\"")));
    }
  }
  let mut sections: Vec<Section> = Vec::new();
  let Some(list) = map.get("sections") else {
    errors.push(err(MANIFEST, 1, "sections is required"));
    return sections;
  };
  let Some(list) = as_sequence(list, MANIFEST, 1, errors) else {
    return sections;
  };
  for item in list {
    let Some(item) = as_mapping(item, MANIFEST, 1, errors) else {
      continue;
    };
    for key in item.keys() {
      let key = key.as_str().unwrap_or_default();
      if key != "dir" && key != "title" {
        errors.push(err(MANIFEST, 1, format!("unknown section key \"{key}\"")));
      }
    }
    let dir = required_string(item, "dir", MANIFEST, 1, errors);
    let title = required_string(item, "title", MANIFEST, 1, errors);
    let (Some(dir), Some(title)) = (dir, title) else {
      continue;
    };
    if !is_ident(&dir) {
      errors.push(err(
        MANIFEST,
        1,
        format!(
          "section dir \"{dir}\" must be lowercase letters, digits, and hyphens"
        ),
      ));
    }
    if sections.iter().any(|section| section.dir == dir) {
      errors.push(err(MANIFEST, 1, format!("duplicate section \"{dir}\"")));
      continue;
    }
    sections.push(Section { dir, title });
  }
  sections
}

fn parse_yaml(
  source: &str,
  path: &str,
  start_line: usize,
  errors: &mut Vec<DocError>,
) -> Option<Value> {
  match serde_yaml::from_str::<Value>(source) {
    Ok(value) => Some(value),
    Err(error) => {
      let line = error.location().map(|at| at.line()).unwrap_or(1);
      errors.push(err(path, start_line + line - 1, error.to_string()));
      None
    }
  }
}

fn as_mapping<'a>(
  value: &'a Value,
  path: &str,
  line: usize,
  errors: &mut Vec<DocError>,
) -> Option<&'a Mapping> {
  match value.as_mapping() {
    Some(map) => Some(map),
    None => {
      errors.push(err(path, line, "expected a YAML mapping"));
      None
    }
  }
}

fn as_sequence<'a>(
  value: &'a Value,
  path: &str,
  line: usize,
  errors: &mut Vec<DocError>,
) -> Option<&'a [Value]> {
  match value.as_sequence() {
    Some(seq) => Some(seq),
    None => {
      errors.push(err(path, line, "expected a YAML sequence"));
      None
    }
  }
}

fn scalar_string(value: &Value) -> Option<String> {
  match value {
    Value::String(s) => Some(s.clone()),
    Value::Number(n) => Some(n.to_string()),
    Value::Bool(b) => Some(b.to_string()),
    Value::Null | Value::Sequence(_) | Value::Mapping(_) | Value::Tagged(_) => {
      None
    }
  }
}

fn required_string(
  map: &Mapping,
  key: &str,
  path: &str,
  line: usize,
  errors: &mut Vec<DocError>,
) -> Option<String> {
  match map.get(key).and_then(Value::as_str) {
    Some(text) if !text.is_empty() => Some(text.to_string()),
    _ => {
      errors.push(err(path, line, format!("{key} must be a non-empty string")));
      None
    }
  }
}

fn optional_string(
  map: &Mapping,
  key: &str,
  path: &str,
  line: usize,
  errors: &mut Vec<DocError>,
) -> Option<String> {
  match map.get(key) {
    None => None,
    Some(value) => match value.as_str() {
      Some(text) if !text.is_empty() => Some(text.to_string()),
      _ => {
        errors.push(err(
          path,
          line,
          format!("{key} must be a non-empty string"),
        ));
        None
      }
    },
  }
}

fn string_list(
  map: &Mapping,
  key: &str,
  path: &str,
  line: usize,
  errors: &mut Vec<DocError>,
) -> Vec<String> {
  let Some(value) = map.get(key) else {
    return Vec::new();
  };
  let Some(items) = as_sequence(value, path, line, errors) else {
    return Vec::new();
  };
  let mut out = Vec::new();
  for item in items {
    match item.as_str() {
      Some(text) => out.push(text.to_string()),
      None => {
        errors.push(err(path, line, format!("{key} items must be strings")))
      }
    }
  }
  out
}

fn record_tag(
  map: &Mapping,
  path: &str,
  line: usize,
  errors: &mut Vec<DocError>,
) -> Option<String> {
  let tag = optional_string(map, "tag", path, line, errors)?;
  if !is_record_tag(&tag) {
    errors.push(err(path, line, format!("record tag \"{tag}\" is invalid")));
  }
  Some(tag)
}

fn parse_commands(
  yaml: &str,
  path: &str,
  line: usize,
  topic: &str,
  errors: &mut Vec<DocError>,
) -> Vec<CommandRecord> {
  let mut items = Vec::new();
  let Some(value) = parse_yaml(yaml, path, line, errors) else {
    return items;
  };
  let Some(seq) = as_sequence(&value, path, line, errors) else {
    return items;
  };
  for raw in seq {
    let Some(map) = as_mapping(raw, path, line, errors) else {
      continue;
    };
    for key in map.keys() {
      let key = key.as_str().unwrap_or_default();
      if !["cmd", "desc", "tag"].contains(&key) {
        errors.push(err(path, line, format!("unknown command key \"{key}\"")));
      }
    }
    let cmd = required_string(map, "cmd", path, line, errors);
    let desc = required_string(map, "desc", path, line, errors);
    let tag = record_tag(map, path, line, errors);
    if let (Some(cmd), Some(desc)) = (cmd, desc) {
      items.push(CommandRecord {
        cmd,
        desc,
        topic: topic.to_string(),
        tag,
      });
    }
  }
  items
}

fn parse_fields(
  yaml: &str,
  path: &str,
  line: usize,
  topic: &str,
  kind: FieldKind,
  errors: &mut Vec<DocError>,
) -> Vec<FieldRecord> {
  let allowed: &[&str] = match kind {
    FieldKind::Config => &["key", "type", "desc", "default", "required", "tag"],
    FieldKind::Js => &["key", "signature", "desc", "tag"],
    FieldKind::CliFlag => &["key", "desc", "type", "tag"],
  };
  let mut items = Vec::new();
  let Some(value) = parse_yaml(yaml, path, line, errors) else {
    return items;
  };
  let Some(seq) = as_sequence(&value, path, line, errors) else {
    return items;
  };
  for raw in seq {
    let Some(map) = as_mapping(raw, path, line, errors) else {
      continue;
    };
    for key in map.keys() {
      let key = key.as_str().unwrap_or_default();
      if !allowed.contains(&key) {
        errors.push(err(
          path,
          line,
          format!("unknown field key \"{key}\" for kind {}", kind.name()),
        ));
      }
    }
    let key = required_string(map, "key", path, line, errors);
    let desc = required_string(map, "desc", path, line, errors);
    let ty = map.get("type").and_then(scalar_string);
    let default = map.get("default").and_then(scalar_string);
    let signature = map.get("signature").and_then(scalar_string);
    let required = match map.get("required") {
      None => None,
      Some(value) => match value.as_bool() {
        Some(required) => Some(required),
        None => {
          errors.push(err(path, line, "required must be a boolean"));
          None
        }
      },
    };
    if kind == FieldKind::Config && ty.is_none() {
      errors.push(err(path, line, "config fields require type"));
    }
    if kind == FieldKind::Js && signature.is_none() {
      errors.push(err(path, line, "js fields require signature"));
    }
    let tag = record_tag(map, path, line, errors);
    let (Some(key), Some(desc)) = (key, desc) else {
      continue;
    };
    items.push(FieldRecord {
      kind,
      key,
      desc,
      topic: topic.to_string(),
      ty,
      default,
      required,
      signature,
      tag,
    });
  }
  items
}
