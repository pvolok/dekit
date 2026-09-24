use std::collections::BTreeMap;

use serde::Serialize;

/// The compiled documentation tree. Serializes to the JSON dekit-web
/// renders (`src/lib/docs/ir.ts` mirrors these types); field order is the
/// wire order.
#[derive(Clone, Debug, Serialize)]
pub struct DocsIr {
  pub version: u32,
  pub dekit: String,
  pub home: String,
  pub topics: Vec<Topic>,
  pub nav: Vec<NavSection>,
  pub tags: BTreeMap<String, TagTarget>,
  pub commands: Vec<CommandRecord>,
  pub fields: Vec<FieldRecord>,
}

impl DocsIr {
  pub fn topic(&self, id: &str) -> Option<&Topic> {
    self.topics.iter().find(|topic| topic.id == id)
  }

  /// The tree without hidden topics: what the website gets.
  pub fn export(&self) -> DocsIr {
    let topics: Vec<Topic> = self
      .topics
      .iter()
      .filter(|topic| !topic.hidden)
      .cloned()
      .collect();
    let tags = self
      .tags
      .iter()
      .filter(|(_, target)| topics.iter().any(|topic| topic.id == target.topic))
      .map(|(tag, target)| (tag.clone(), target.clone()))
      .collect();
    DocsIr {
      version: self.version,
      dekit: self.dekit.clone(),
      home: self.home.clone(),
      topics,
      nav: self.nav.clone(),
      tags,
      commands: self.commands.clone(),
      fields: self.fields.clone(),
    }
  }
}

#[derive(Clone, Debug, Serialize)]
pub struct NavSection {
  pub id: String,
  pub title: String,
  pub topics: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct TagTarget {
  pub topic: String,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub heading: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct Topic {
  pub id: String,
  pub title: String,
  pub slug: String,
  pub section: String,
  pub summary: String,
  pub tags: Vec<String>,
  pub related: Vec<String>,
  pub headings: Vec<Heading>,
  pub blocks: Vec<Block>,
  pub source: String,
  #[serde(skip)]
  pub hidden: bool,
  #[serde(skip)]
  pub cli: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct Heading {
  pub id: String,
  pub level: u8,
  pub text: String,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum Block {
  Heading {
    level: u8,
    id: String,
    inlines: Vec<Inline>,
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
    blocks: Vec<Block>,
  },
  Commands {
    items: Vec<CommandRecord>,
  },
  Fields {
    kind: FieldKind,
    items: Vec<FieldRecord>,
  },
  Footnote {
    inlines: Vec<Inline>,
  },
  Image {
    src: String,
    alt: String,
    #[serde(rename = "webOnly")]
    web_only: bool,
  },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum CalloutVariant {
  Note,
  Tip,
  Warning,
}

impl CalloutVariant {
  pub fn label(self) -> &'static str {
    match self {
      CalloutVariant::Note => "Note",
      CalloutVariant::Tip => "Tip",
      CalloutVariant::Warning => "Warning",
    }
  }
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum Inline {
  Text { text: String },
  Code { text: String },
  Strong { children: Vec<Inline> },
  Em { children: Vec<Inline> },
  TagLink { tag: String },
  UrlLink { href: String, children: Vec<Inline> },
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct CommandRecord {
  pub cmd: String,
  pub desc: String,
  pub topic: String,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub tag: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum FieldKind {
  Config,
  Js,
  CliFlag,
}

impl FieldKind {
  pub fn parse(name: &str) -> Option<FieldKind> {
    match name {
      "config" => Some(FieldKind::Config),
      "js" => Some(FieldKind::Js),
      "cli-flag" => Some(FieldKind::CliFlag),
      _ => None,
    }
  }

  pub fn name(self) -> &'static str {
    match self {
      FieldKind::Config => "config",
      FieldKind::Js => "js",
      FieldKind::CliFlag => "cli-flag",
    }
  }
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct FieldRecord {
  pub kind: FieldKind,
  pub key: String,
  pub desc: String,
  pub topic: String,
  #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
  pub ty: Option<String>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub default: Option<String>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub required: Option<bool>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub signature: Option<String>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub tag: Option<String>,
}

/// The visible text of a run of inlines: heading ids, search, and the
/// index all read this.
pub fn plain_text(inlines: &[Inline]) -> String {
  let mut out = String::new();
  for inline in inlines {
    match inline {
      Inline::Text { text } | Inline::Code { text } => out.push_str(text),
      Inline::Strong { children }
      | Inline::Em { children }
      | Inline::UrlLink { children, .. } => out.push_str(&plain_text(children)),
      Inline::TagLink { tag } => out.push_str(tag),
    }
  }
  out
}
