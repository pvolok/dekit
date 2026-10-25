//! Writes task edits back into a config file.
//!
//! serde_yaml would drop comments and formatting, so edits are made on the
//! text: a line and indent scanner for block-style YAML finds the `tasks:`
//! entries and their keys, and an edit rewrites only the lines it touches.
//! Everything else stays byte for byte.
//!
//! What the scanner can't handle is refused rather than guessed at: flow
//! mappings (except a one-line `env:`), anchors, aliases, merge keys and
//! tags in an entry being edited, tabs, and multi-document files. As a
//! safety net, every edit is also applied to the parsed original, and the
//! edited text must parse to the same value.

use std::fmt::{self, Write as _};

use serde_yaml::{Mapping, Value};

/// Width that a flow sequence must fit in to be written on one line.
const MAX_WIDTH: usize = 80;

/// One change to a config file's `tasks:` mapping.
#[derive(Clone, Debug, PartialEq)]
pub enum Edit {
  /// Set a key of the task entry `task` under the top-level `tasks:`;
  /// `None` removes it. `field` is a top-level key of the entry (`cmd`,
  /// `cwd`, `env`, `autostart`, ...) or `env.NAME` for one variable of
  /// its `env` mapping (creating `env:` when missing; removing the last
  /// variable removes `env:`).
  Set {
    task: String,
    field: String,
    value: Option<Value>,
  },
  /// Append a task entry at the end of `tasks:` (creating a top-level
  /// `tasks:` at the end of the file when there is none).
  AddTask { task: String, entry: Mapping },
  /// Remove the task entry, with its comment lines directly above it.
  RemoveTask { task: String },
  /// Rename the task's key, keeping its body and place.
  RenameTask { from: String, to: String },
}

impl Edit {
  fn task(&self) -> &str {
    match self {
      Edit::Set { task, .. }
      | Edit::AddTask { task, .. }
      | Edit::RemoveTask { task } => task,
      Edit::RenameTask { from, .. } => from,
    }
  }
}

#[derive(Clone, Debug, PartialEq)]
pub enum WriteError {
  /// The file can't be edited safely; the string says why, in a short
  /// sentence for a user ("env of task api is a flow mapping").
  Unsupported(String),
  /// The task to edit isn't in the file.
  NoTask(String),
}

impl fmt::Display for WriteError {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self {
      WriteError::Unsupported(why) => {
        write!(f, "can't edit the config file: {why}")
      }
      WriteError::NoTask(task) => {
        write!(f, "task {task} is not in the config file")
      }
    }
  }
}

impl std::error::Error for WriteError {}

fn unsupported(why: impl Into<String>) -> WriteError {
  WriteError::Unsupported(why.into())
}

/// Applies the edits in order and returns the new text.
pub fn apply(text: &str, edits: &[Edit]) -> Result<String, WriteError> {
  let mut doc = Doc::parse(text)?;
  // Refuses tabs and multiple documents with a clearer message than the
  // parser has.
  analyze(&doc.lines)?;
  let mut expected = serde_yaml::from_str::<Value>(text).map_err(|err| {
    unsupported(format!("the file is not valid YAML ({err})"))
  })?;
  if !matches!(expected, Value::Mapping(_) | Value::Null) {
    return Err(unsupported("the top level of the file is not a mapping"));
  }
  for edit in edits {
    edit_lines(&mut doc.lines, edit, &expected)?;
    let ok = apply_value(&mut expected, edit).is_some()
      && serde_yaml::from_str::<Value>(&doc.text())
        .is_ok_and(|got| same(&got, &expected));
    if !ok {
      return Err(unsupported(format!(
        "could not edit task {} safely",
        edit.task()
      )));
    }
  }
  Ok(doc.text())
}

/// The file as lines without their line endings.
struct Doc {
  lines: Vec<String>,
  eol: &'static str,
  final_eol: bool,
  bom: bool,
}

impl Doc {
  fn parse(text: &str) -> Result<Doc, WriteError> {
    let (bom, text) = match text.strip_prefix('\u{feff}') {
      Some(rest) => (true, rest),
      None => (false, text),
    };
    let crlf = text.contains("\r\n");
    if crlf && text.matches('\n').count() != text.matches("\r\n").count() {
      return Err(unsupported("the file mixes line endings"));
    }
    let eol = if crlf { "\r\n" } else { "\n" };
    let lines = if text.is_empty() {
      Vec::new()
    } else {
      let body = text.strip_suffix(eol).unwrap_or(text);
      body.split(eol).map(str::to_string).collect()
    };
    Ok(Doc {
      lines,
      eol,
      final_eol: text.is_empty() || text.ends_with('\n'),
      bom,
    })
  }

  fn text(&self) -> String {
    let mut out = String::new();
    if self.bom {
      out.push('\u{feff}');
    }
    out.push_str(&self.lines.join(self.eol));
    if self.final_eol && !self.lines.is_empty() {
      out.push_str(self.eol);
    }
    out
  }
}

/// Lines `start..end` replaced by `lines`.
struct Splice {
  start: usize,
  end: usize,
  lines: Vec<String>,
}

impl Splice {
  fn new(start: usize, end: usize, lines: Vec<String>) -> Self {
    Splice { start, end, lines }
  }
}

fn edit_lines(
  lines: &mut Vec<String>,
  edit: &Edit,
  cur: &Value,
) -> Result<(), WriteError> {
  let view = View::new(lines)?;
  let mut splices = match edit {
    Edit::Set { task, field, value } => {
      view.set(task, field, value.as_ref(), cur)?
    }
    Edit::AddTask { task, entry } => view.add_task(task, entry)?,
    Edit::RemoveTask { task } => view.remove_task(task)?,
    Edit::RenameTask { from, to } => view.rename_task(from, to)?,
  };
  // Splices never overlap; applying them from the bottom up keeps the
  // line numbers of the rest valid.
  splices.sort_by_key(|s| std::cmp::Reverse(s.start));
  for s in splices {
    lines.splice(s.start..s.end, s.lines);
  }
  Ok(())
}

// Scanning

#[derive(Clone, Copy, Debug, PartialEq)]
enum Kind {
  Blank,
  Comment,
  /// Starts with a key, a sequence item or a scalar.
  Content,
  /// Continues a block scalar, a flow collection or a quoted scalar.
  Inner,
  /// `---` before the content, or a directive.
  Marker,
}

const ANCHOR: &str = "an anchor";

#[derive(Clone, Debug)]
struct Info {
  /// Leading spaces.
  indent: usize,
  /// Only whitespace.
  blank: bool,
  kind: Kind,
  /// Byte index of the `#` that starts a comment.
  comment: Option<usize>,
  /// The first anchor, alias, tag or complex key on the line.
  special: Option<&'static str>,
  /// The line defines an anchor.
  anchor: bool,
  /// The line ends inside a flow collection or a quoted scalar.
  open: bool,
}

#[derive(Default)]
struct Lex {
  quote: Option<u8>,
  depth: usize,
  /// In a block scalar: the column of the node that owns it, and the
  /// indent of its body once known.
  block: Option<(usize, Option<usize>)>,
  /// Content was seen.
  content: bool,
}

fn is_ws(c: u8) -> bool {
  c == b' ' || c == b'\t'
}

fn is_flow_indicator(c: u8) -> bool {
  matches!(c, b',' | b'[' | b']' | b'{' | b'}')
}

fn analyze(lines: &[String]) -> Result<Vec<Info>, WriteError> {
  let mut st = Lex::default();
  let mut infos = Vec::with_capacity(lines.len());
  for line in lines {
    let b = line.as_bytes();
    let indent = b.iter().take_while(|&&c| c == b' ').count();
    let blank = b[indent..].iter().all(|&c| is_ws(c));
    let mut info = Info {
      indent,
      blank,
      kind: Kind::Blank,
      comment: None,
      special: None,
      anchor: false,
      open: false,
    };
    if let Some((owner, body)) = &mut st.block {
      let inside = blank
        || match body {
          Some(body) => indent >= *body,
          None if indent > *owner => {
            *body = Some(indent);
            true
          }
          None => false,
        };
      if inside {
        info.kind = Kind::Inner;
        infos.push(info);
        continue;
      }
      st.block = None;
    }
    if st.quote.is_some() || st.depth > 0 {
      info.kind = Kind::Inner;
      scan(b, indent, &mut st, &mut info);
      infos.push(info);
      continue;
    }
    if blank {
      infos.push(info);
      continue;
    }
    if b[indent] == b'\t' {
      return Err(unsupported("the file uses tabs for indentation"));
    }
    if b[indent] == b'#' {
      info.kind = Kind::Comment;
      info.comment = Some(indent);
      infos.push(info);
      continue;
    }
    if indent == 0 && is_marker(line, "---") {
      if st.content {
        return Err(unsupported("the file has more than one YAML document"));
      }
      let rest = line[3..].trim_start();
      if !rest.is_empty() && !rest.starts_with('#') {
        return Err(unsupported(
          "the top level of the file is not a block mapping",
        ));
      }
      info.kind = Kind::Marker;
      infos.push(info);
      continue;
    }
    if indent == 0 && is_marker(line, "...") {
      return Err(unsupported("the file has a document end marker (...)"));
    }
    if indent == 0 && b[0] == b'%' && !st.content {
      info.kind = Kind::Marker;
      infos.push(info);
      continue;
    }
    st.content = true;
    info.kind = Kind::Content;
    scan(b, indent, &mut st, &mut info);
    infos.push(info);
  }
  Ok(infos)
}

fn is_marker(line: &str, marker: &str) -> bool {
  line
    .strip_prefix(marker)
    .is_some_and(|rest| rest.is_empty() || rest.starts_with([' ', '\t']))
}

/// Scans a line for comments, quotes, flow collections, block scalar
/// headers, anchors, aliases and tags, carrying multi-line state in `st`.
fn scan(b: &[u8], from: usize, st: &mut Lex, info: &mut Info) {
  let mut i = from;
  // A new token can start here (as opposed to inside a plain scalar).
  let mut tok = true;
  let mut ws = true;
  // Column of the key or `-` that owns a block scalar.
  let mut owner = from;
  let mut scalar = from;
  while i < b.len() {
    let c = b[i];
    if let Some(q) = st.quote {
      if q == b'"' && c == b'\\' {
        i += 2;
        continue;
      }
      if c == q {
        if q == b'\'' && b.get(i + 1) == Some(&b'\'') {
          i += 2;
          continue;
        }
        st.quote = None;
        tok = false;
      }
      ws = false;
      i += 1;
      continue;
    }
    if is_ws(c) {
      ws = true;
      i += 1;
      continue;
    }
    if c == b'#' && ws {
      info.comment = Some(i);
      break;
    }
    ws = false;
    let flow = st.depth > 0;
    let sep = |j: usize| {
      b.get(j)
        .is_none_or(|&n| is_ws(n) || (flow && is_flow_indicator(n)))
    };
    if tok {
      match c {
        b'"' | b'\'' => {
          st.quote = Some(c);
          scalar = i;
        }
        b'[' | b'{' => st.depth += 1,
        b']' | b'}' => {
          st.depth = st.depth.saturating_sub(1);
          tok = false;
        }
        b',' if flow => {}
        b'&' | b'*' | b'!' => {
          let kind = match c {
            b'&' => ANCHOR,
            b'*' => "an alias",
            _ => "a tag",
          };
          info.special.get_or_insert(kind);
          info.anchor |= c == b'&';
          while i + 1 < b.len()
            && !is_ws(b[i + 1])
            && !is_flow_indicator(b[i + 1])
          {
            i += 1;
          }
          tok = c != b'*';
        }
        b'|' | b'>' if !flow => {
          st.block = Some((owner, None));
          tok = false;
        }
        b'-' | b'?' | b':' if sep(i + 1) => {
          if c == b'-' && !flow {
            owner = i;
          }
          if c == b'?' {
            info.special.get_or_insert("a complex key");
          }
        }
        _ => {
          tok = false;
          scalar = i;
        }
      }
    } else {
      match c {
        b':' if sep(i + 1) => {
          if !flow {
            owner = scalar;
          }
          tok = true;
        }
        b',' if flow => tok = true,
        b'[' | b'{' if flow => {
          st.depth += 1;
          tok = true;
        }
        b']' | b'}' if flow => st.depth -= 1,
        _ => {}
      }
    }
    i += 1;
  }
  info.open = st.quote.is_some() || st.depth > 0;
}

/// A quoted scalar at the start of `s`: its text and its length as written.
fn quoted(s: &str) -> Option<(String, usize)> {
  let b = s.as_bytes();
  let q = *b.first()?;
  let mut i = 1;
  while i < b.len() {
    match b[i] {
      b'\\' if q == b'"' => i += 2,
      c if c == q => {
        if q == b'\'' && b.get(i + 1) == Some(&b'\'') {
          i += 2;
          continue;
        }
        let raw = &s[..=i];
        let text = if q == b'"' {
          serde_yaml::from_str::<String>(raw).ok()?
        } else {
          raw[1..i].replace("''", "'")
        };
        return Some((text, i + 1));
      }
      _ => i += 1,
    }
  }
  None
}

#[derive(Clone, Debug)]
struct Key {
  /// Unquoted.
  text: String,
  /// Byte range of the key as written.
  start: usize,
  end: usize,
  colon: usize,
}

/// A block mapping key starting at byte `at` of `line`.
fn parse_key(line: &str, at: usize) -> Option<Key> {
  let s = &line[at..];
  let b = s.as_bytes();
  let (text, len) = match *b.first()? {
    b'"' | b'\'' => quoted(s)?,
    b'-' | b'?' | b':' if b.get(1).is_none_or(|&c| is_ws(c)) => return None,
    b'[' | b']' | b'{' | b'}' | b',' | b'#' | b'&' | b'*' | b'!' | b'|'
    | b'>' | b'%' | b'@' | b'`' => return None,
    _ => {
      let mut i = 0;
      loop {
        if i >= b.len() || (b[i] == b'#' && i > 0 && is_ws(b[i - 1])) {
          return None;
        }
        if b[i] == b':' && b.get(i + 1).is_none_or(|&c| is_ws(c)) {
          break;
        }
        i += 1;
      }
      let key = s[..i].trim_end();
      (key.to_string(), key.len())
    }
  };
  let mut colon = len;
  while colon < b.len() && is_ws(b[colon]) {
    colon += 1;
  }
  if b.get(colon) != Some(&b':') || !b.get(colon + 1).is_none_or(|&c| is_ws(c))
  {
    return None;
  }
  Some(Key {
    text,
    start: at,
    end: at + len,
    colon: at + colon,
  })
}

fn is_dash(line: &str, at: usize) -> bool {
  line[at..]
    .strip_prefix('-')
    .is_some_and(|rest| rest.is_empty() || rest.starts_with([' ', '\t']))
}

/// What the value on a key line looks like.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Val {
  Empty,
  Null,
  EmptyMap,
  FlowMap,
  FlowSeq,
  BlockScalar,
  Special,
  Scalar,
}

fn classify(v: &str) -> Val {
  match v.as_bytes().first() {
    None => Val::Empty,
    Some(b'{') if v.ends_with('}') && v[1..v.len() - 1].trim().is_empty() => {
      Val::EmptyMap
    }
    Some(b'{') => Val::FlowMap,
    Some(b'[') => Val::FlowSeq,
    Some(b'|' | b'>') => Val::BlockScalar,
    Some(b'&' | b'*' | b'!') => Val::Special,
    _ if matches!(v, "~" | "null" | "Null" | "NULL") => Val::Null,
    _ => Val::Scalar,
  }
}

/// A key of a block mapping.
#[derive(Clone, Debug)]
struct Node {
  line: usize,
  key: Key,
  /// Byte range of the value on the key line, without a comment.
  vstart: usize,
  vend: usize,
  /// One past the node's last line that isn't a comment or blank.
  end: usize,
  /// `end` plus the comment lines indented past the key right below it.
  full_end: usize,
}

impl Node {
  fn col(&self) -> usize {
    self.key.start
  }
}

/// How a node holds a mapping.
enum Form {
  Block(Vec<Node>),
  /// No value, `~`, `null` or `{}`.
  Empty,
  Flow,
}

struct View<'a> {
  lines: &'a [String],
  infos: Vec<Info>,
}

impl<'a> View<'a> {
  fn new(lines: &'a [String]) -> Result<Self, WriteError> {
    Ok(View {
      lines,
      infos: analyze(lines)?,
    })
  }

  fn line(&self, i: usize) -> &'a str {
    &self.lines[i]
  }

  fn value(&self, n: &Node) -> &'a str {
    &self.lines[n.line][n.vstart..n.vend]
  }

  fn blank(&self, i: usize) -> bool {
    self.infos.get(i).is_some_and(|info| info.blank)
  }

  fn node(&self, line: usize, key: Key) -> Node {
    let b = self.line(line).as_bytes();
    let limit = self.infos[line].comment.unwrap_or(b.len());
    let mut vstart = key.colon + 1;
    while vstart < limit && is_ws(b[vstart]) {
      vstart += 1;
    }
    let mut vend = limit;
    while vend > vstart && is_ws(b[vend - 1]) {
      vend -= 1;
    }
    Node {
      line,
      key,
      vstart,
      vend,
      end: line + 1,
      full_end: line + 1,
    }
  }

  /// The keys of the block mapping in lines `start..end`.
  fn children(
    &self,
    start: usize,
    end: usize,
    what: &str,
  ) -> Result<Vec<Node>, WriteError> {
    let mut nodes: Vec<Node> = Vec::new();
    let mut col = None;
    for i in start..end {
      let info = &self.infos[i];
      match info.kind {
        Kind::Inner if !info.blank => {
          if let Some(n) = nodes.last_mut() {
            n.end = i + 1;
          }
          continue;
        }
        Kind::Content => {}
        _ => continue,
      }
      let col = *col.get_or_insert(info.indent);
      if info.indent < col {
        return Err(unsupported(format!("{what} has unexpected indentation")));
      }
      if info.indent > col {
        if let Some(n) = nodes.last_mut() {
          n.end = i + 1;
        }
        continue;
      }
      let line = self.line(i);
      if let Some(key) = parse_key(line, col) {
        nodes.push(self.node(i, key));
        continue;
      }
      // A block sequence may sit at its key's indent.
      if let Some(n) = nodes.last_mut()
        && n.vstart == n.vend
        && is_dash(line, col)
      {
        n.end = i + 1;
        continue;
      }
      return Err(unsupported(format!("{what} is not a block mapping")));
    }
    // Comments indented past the key belong to it even when they are the
    // last lines of the parent.
    for k in 0..nodes.len() {
      let limit = nodes.get(k + 1).map_or(self.lines.len(), |n| n.line);
      let mut j = nodes[k].end;
      while j < limit
        && self.infos[j].kind == Kind::Comment
        && self.infos[j].indent > nodes[k].col()
      {
        j += 1;
      }
      nodes[k].full_end = j;
    }
    Ok(nodes)
  }

  fn form(&self, n: &Node, what: &str) -> Result<Form, WriteError> {
    match classify(self.value(n)) {
      Val::Empty if n.end > n.line + 1 => {
        Ok(Form::Block(self.children(n.line + 1, n.end, what)?))
      }
      Val::Empty | Val::Null | Val::EmptyMap => Ok(Form::Empty),
      Val::FlowMap => Ok(Form::Flow),
      Val::Special => Err(unsupported(format!(
        "{what} uses {}",
        self.infos[n.line].special.unwrap_or("a tag")
      ))),
      _ => Err(unsupported(format!("{what} is not a mapping"))),
    }
  }

  fn root(&self) -> Result<Vec<Node>, WriteError> {
    let what = "the top level of the file";
    let nodes = self.children(0, self.lines.len(), what)?;
    if nodes.first().is_some_and(|n| n.col() != 0) {
      return Err(unsupported(format!("{what} is not a block mapping")));
    }
    Ok(nodes)
  }

  /// The top-level `tasks:` node and its form (never `Form::Flow`).
  fn tasks(&self) -> Result<Option<(Node, Form)>, WriteError> {
    let root = self.root()?;
    let Some(node) = root.into_iter().find(|n| n.key.text == "tasks") else {
      return Ok(None);
    };
    match self.form(&node, "tasks")? {
      Form::Flow => Err(unsupported("tasks is a flow mapping")),
      form => Ok(Some((node, form))),
    }
  }

  /// The `tasks:` node, its entries and the index of `task` among them.
  fn find(&self, task: &str) -> Result<(Node, Vec<Node>, usize), WriteError> {
    let no = || WriteError::NoTask(task.to_string());
    let Some((tasks, Form::Block(entries))) = self.tasks()? else {
      return Err(no());
    };
    let i = entries
      .iter()
      .position(|e| e.key.text == task)
      .ok_or_else(no)?;
    Ok((tasks, entries, i))
  }

  /// Where comment lines directly above `n` start.
  fn attached_start(&self, n: &Node, floor: usize) -> usize {
    let mut s = n.line;
    while s > floor
      && self.infos[s - 1].kind == Kind::Comment
      && self.infos[s - 1].indent <= n.col()
    {
      s -= 1;
    }
    s
  }

  /// Indent unit for a file without task entries to take it from.
  fn unit(&self) -> usize {
    self
      .infos
      .iter()
      .filter(|info| info.kind == Kind::Content && info.indent > 0)
      .map(|info| info.indent)
      .min()
      .unwrap_or(2)
      .min(8)
  }

  /// The key line of `n` with its value replaced, keeping a comment.
  fn rebuild(&self, n: &Node, value: Option<&str>) -> String {
    let text = self.line(n.line);
    let mut s = text[..n.key.colon + 1].to_string();
    if let Some(value) = value {
      s.push(' ');
      s.push_str(value);
    }
    if let Some(c) = self.infos[n.line].comment {
      s.push(' ');
      s.push_str(&text[c..]);
    }
    s
  }

  fn replace(
    &self,
    n: &Node,
    v: &Value,
    unit: usize,
  ) -> Result<Vec<Splice>, WriteError> {
    let one_line = n.end == n.line + 1
      && !self.infos[n.line].open
      && matches!(
        classify(self.value(n)),
        Val::Scalar | Val::Null | Val::FlowMap | Val::FlowSeq | Val::EmptyMap
      );
    let col = if one_line { n.vstart } else { n.key.colon + 2 };
    Ok(match emit_value(v, n.col() + unit, col, unit)? {
      Out::Inline(s) if one_line => {
        let text = self.line(n.line);
        let line = format!("{}{s}{}", &text[..n.vstart], &text[n.vend..]);
        vec![Splice::new(n.line, n.line + 1, vec![line])]
      }
      Out::Inline(s) => {
        vec![Splice::new(n.line, n.end, vec![self.rebuild(n, Some(&s))])]
      }
      Out::Block(body) => {
        let mut lines = vec![self.rebuild(n, None)];
        lines.extend(body);
        vec![Splice::new(n.line, n.end, lines)]
      }
    })
  }

  /// Removes `field` from `entry`, leaving `{}` when it was the last one.
  fn remove_field(
    &self,
    entry: &Node,
    fields: &[Node],
    field: &Node,
  ) -> Vec<Splice> {
    let mut s = vec![Splice::new(field.line, field.full_end, vec![])];
    if fields.len() == 1 {
      let line = self.rebuild(entry, Some("{}"));
      s.push(Splice::new(entry.line, entry.line + 1, vec![line]));
    }
    s
  }

  fn insert_field(
    &self,
    entry: &Node,
    fields: &[Node],
    col: usize,
    key: &str,
    v: &Value,
    unit: usize,
  ) -> Result<Vec<Splice>, WriteError> {
    let lines = emit_pair(&Value::from(key), v, col, unit)?;
    let mut s = vec![Splice::new(entry.end, entry.end, lines)];
    if fields.is_empty() && !self.value(entry).is_empty() {
      let line = self.rebuild(entry, None);
      s.push(Splice::new(entry.line, entry.line + 1, vec![line]));
    }
    Ok(s)
  }

  fn set(
    &self,
    task: &str,
    field: &str,
    value: Option<&Value>,
    cur: &Value,
  ) -> Result<Vec<Splice>, WriteError> {
    let (_, entries, i) = self.find(task)?;
    let entry = &entries[i];
    let what = format!("task {task}");
    let special = (entry.line..entry.full_end)
      .find_map(|j| self.infos[j].special)
      .or_else(|| entry_value(cur, task).and_then(uses));
    if let Some(special) = special {
      return Err(unsupported(format!("{what} uses {special}")));
    }
    let fields = match self.form(entry, &what)? {
      Form::Block(fields) => fields,
      Form::Empty => Vec::new(),
      Form::Flow => {
        return Err(unsupported(format!("{what} is a flow mapping")));
      }
    };
    let unit = entries[0].col();
    let col = fields.first().map_or(entry.col() + unit, Node::col);
    let find = |nodes: &'_ [Node], key: &str| {
      nodes.iter().find(|n| n.key.text == key).cloned()
    };
    let Some(name) = field.strip_prefix("env.") else {
      return Ok(match (find(&fields, field), value) {
        (Some(f), Some(v)) => self.replace(&f, v, unit)?,
        (Some(f), None) => self.remove_field(entry, &fields, &f),
        (None, Some(v)) => {
          self.insert_field(entry, &fields, col, field, v, unit)?
        }
        (None, None) => Vec::new(),
      });
    };

    let Some(env) = find(&fields, "env") else {
      return match value {
        Some(v) => {
          let env = single(name, v);
          self.insert_field(entry, &fields, col, "env", &env, unit)
        }
        None => Ok(Vec::new()),
      };
    };
    let what = format!("env of task {task}");
    match self.form(&env, &what)? {
      Form::Block(vars) => Ok(match (find(&vars, name), value) {
        (Some(var), Some(v)) => self.replace(&var, v, unit)?,
        (Some(_), None) if vars.len() == 1 => {
          self.remove_field(entry, &fields, &env)
        }
        (Some(var), None) => {
          vec![Splice::new(var.line, var.full_end, vec![])]
        }
        (None, Some(v)) => {
          let lines = emit_pair(&Value::from(name), v, vars[0].col(), unit)?;
          vec![Splice::new(env.end, env.end, lines)]
        }
        (None, None) => Vec::new(),
      }),
      Form::Empty => match value {
        Some(v) => self.replace(&env, &single(name, v), unit),
        None => Ok(Vec::new()),
      },
      Form::Flow => self.set_flow_env(entry, &fields, &env, name, value, &what),
    }
  }

  /// Edits one item of a one-line flow `env: {...}`.
  fn set_flow_env(
    &self,
    entry: &Node,
    fields: &[Node],
    env: &Node,
    name: &str,
    value: Option<&Value>,
    what: &str,
  ) -> Result<Vec<Splice>, WriteError> {
    if env.end != env.line + 1 || self.infos[env.line].open {
      return Err(unsupported(format!("{what} is a multi-line flow mapping")));
    }
    let val = self.value(env);
    let items = flow_items(val).ok_or_else(|| {
      unsupported(format!("{what} is a flow mapping that can't be read"))
    })?;
    let text = self.line(env.line);
    let edit = |start: usize, end: usize, with: &str| {
      let mut line = text.to_string();
      line.replace_range(env.vstart + start..env.vstart + end, with);
      vec![Splice::new(env.line, env.line + 1, vec![line])]
    };
    let found = items.iter().position(|item| item.key == name);
    Ok(match (found, value) {
      (Some(i), Some(v)) => {
        let item = &items[i];
        let v = flow(v)?;
        match item.value {
          Some((start, end)) => edit(start, end, &v),
          None => {
            let key = &val[item.start..item.key_end];
            edit(item.start, item.end, &format!("{key}: {v}"))
          }
        }
      }
      (Some(_), None) if items.len() == 1 => {
        self.remove_field(entry, fields, env)
      }
      (Some(0), None) => edit(items[0].start, items[1].start, ""),
      (Some(i), None) => edit(items[i - 1].end, items[i].end, ""),
      (None, Some(v)) => {
        let item =
          format!("{}: {}", emit_key(&Value::from(name), true)?, flow(v)?);
        match items.last() {
          Some(last) => edit(last.end, last.end, &format!(", {item}")),
          None => edit(1, 1, &item),
        }
      }
      (None, None) => Vec::new(),
    })
  }

  fn add_task(
    &self,
    task: &str,
    entry: &Mapping,
  ) -> Result<Vec<Splice>, WriteError> {
    let key = Value::from(task);
    let value = Value::Mapping(entry.clone());
    let Some((tasks, form)) = self.tasks()? else {
      // A new `tasks:` at the end, spaced like the top-level keys are.
      let root = self.root()?;
      let unit = self.unit();
      let spaced = root.iter().skip(1).any(|n| {
        let s = self.attached_start(n, 0);
        s > 0 && self.blank(s - 1)
      });
      let mut lines = Vec::new();
      if spaced && self.lines.last().is_some_and(|l| !l.trim().is_empty()) {
        lines.push(String::new());
      }
      lines.push("tasks:".to_string());
      lines.extend(emit_pair(&key, &value, unit, unit)?);
      let end = self.lines.len();
      return Ok(vec![Splice::new(end, end, lines)]);
    };
    let entries = match form {
      Form::Block(entries) => entries,
      _ => {
        // After any comment lines right below `tasks:`.
        let unit = self.unit();
        let mut at = tasks.line + 1;
        while at < self.lines.len()
          && self.infos[at].kind == Kind::Comment
          && self.infos[at].indent > 0
        {
          at += 1;
        }
        let mut s =
          vec![Splice::new(at, at, emit_pair(&key, &value, unit, unit)?)];
        if !self.value(&tasks).is_empty() {
          let line = self.rebuild(&tasks, None);
          s.push(Splice::new(tasks.line, tasks.line + 1, vec![line]));
        }
        return Ok(s);
      }
    };
    if entries.iter().any(|e| e.key.text == task) {
      return Err(unsupported(format!("task {task} already exists")));
    }
    let Some(last) = entries.last() else {
      return Err(unsupported("tasks is not a block mapping"));
    };
    let col = last.col();
    let unit = match self.form(last, "") {
      Ok(Form::Block(fields)) => fields[0].col() - col,
      _ => col,
    };
    let spaced = entries.len() > 1 && {
      let s = self.attached_start(last, tasks.line + 1);
      self.blank(s - 1)
    };
    let mut lines = Vec::new();
    if spaced {
      lines.push(String::new());
    }
    lines.extend(emit_pair(&key, &value, col, unit)?);
    Ok(vec![Splice::new(last.full_end, last.full_end, lines)])
  }

  fn remove_task(&self, task: &str) -> Result<Vec<Splice>, WriteError> {
    let (tasks, entries, i) = self.find(task)?;
    let e = &entries[i];
    if (e.line..e.full_end).any(|j| self.infos[j].anchor) {
      return Err(unsupported(format!("task {task} defines an anchor")));
    }
    let floor = tasks.line + 1;
    let mut start = self.attached_start(e, floor);
    let mut end = e.full_end;
    if entries.len() == 1 {
      let line = self.rebuild(&tasks, Some("{}"));
      return Ok(vec![
        Splice::new(start, end, vec![]),
        Splice::new(tasks.line, tasks.line + 1, vec![line]),
      ]);
    }
    // Drop one blank separator: the one after the entry, or the one
    // before the last entry (the one after it separates `tasks:` from
    // what follows).
    if i + 1 == entries.len() {
      if start > floor && self.blank(start - 1) {
        start -= 1;
      }
    } else if self.blank(end) {
      end += 1;
    }
    Ok(vec![Splice::new(start, end, vec![])])
  }

  fn rename_task(
    &self,
    from: &str,
    to: &str,
  ) -> Result<Vec<Splice>, WriteError> {
    let (_, entries, i) = self.find(from)?;
    if from == to {
      return Ok(Vec::new());
    }
    if entries.iter().any(|e| e.key.text == to) {
      return Err(unsupported(format!("task {to} already exists")));
    }
    let e = &entries[i];
    let mut line = self.line(e.line).to_string();
    let key = match line.as_bytes()[e.key.start] {
      b'"' => double_quoted(to),
      b'\'' => single_quoted(to).unwrap_or_else(|| double_quoted(to)),
      _ => emit_key(&Value::from(to), false)?,
    };
    line.replace_range(e.key.start..e.key.end, &key);
    Ok(vec![Splice::new(e.line, e.line + 1, vec![line])])
  }
}

/// An item of a one-line flow mapping; byte ranges are into its text.
struct FlowItem {
  start: usize,
  end: usize,
  key: String,
  key_end: usize,
  value: Option<(usize, usize)>,
}

/// The items of the one-line flow mapping `s` (`{a: 1, b: 2}`).
fn flow_items(s: &str) -> Option<Vec<FlowItem>> {
  let b = s.as_bytes();
  if b.first() != Some(&b'{') || b.last() != Some(&b'}') {
    return None;
  }
  let close = b.len() - 1;
  let mut items = Vec::new();
  let mut i = 1;
  while i < close {
    while i < close && is_ws(b[i]) {
      i += 1;
    }
    let start = i;
    let (mut depth, mut quote, mut tok) = (0usize, None, true);
    while i < close {
      let c = b[i];
      if let Some(q) = quote {
        if q == b'"' && c == b'\\' {
          i += 2;
          continue;
        }
        if c == q {
          if q == b'\'' && b.get(i + 1) == Some(&b'\'') {
            i += 2;
            continue;
          }
          quote = None;
          tok = false;
        }
        i += 1;
        continue;
      }
      match c {
        b'"' | b'\'' if tok => quote = Some(c),
        b',' if depth == 0 => break,
        b'[' | b'{' | b',' => tok = true,
        b':'
          if b
            .get(i + 1)
            .is_none_or(|&n| is_ws(n) || is_flow_indicator(n)) =>
        {
          tok = true
        }
        b']' | b'}' => {
          depth = depth.checked_sub(1)?;
          tok = false;
        }
        c if is_ws(c) => {}
        _ => tok = false,
      }
      if matches!(c, b'[' | b'{') {
        depth += 1;
      }
      i += 1;
    }
    if quote.is_some() || depth != 0 {
      return None;
    }
    let mut end = i;
    while end > start && is_ws(b[end - 1]) {
      end -= 1;
    }
    if end > start {
      items.push(flow_item(s, start, end)?);
    }
    i += 1;
  }
  Some(items)
}

fn flow_item(s: &str, start: usize, end: usize) -> Option<FlowItem> {
  let item = &s[start..end];
  let b = item.as_bytes();
  let (key, key_len) = match b[0] {
    b'"' | b'\'' => quoted(item)?,
    _ => {
      let colon = (0..b.len())
        .find(|&i| b[i] == b':' && b.get(i + 1).is_none_or(|&n| is_ws(n)))
        .unwrap_or(b.len());
      let key = item[..colon].trim_end();
      (key.to_string(), key.len())
    }
  };
  let after = item[key_len..].trim_start();
  let value = match after.strip_prefix(':') {
    Some(v) if v.trim().is_empty() => None,
    Some(v) => {
      let v = v.trim_start();
      Some((end - v.len(), end))
    }
    None if after.is_empty() => None,
    None => return None,
  };
  Some(FlowItem {
    start,
    end,
    key,
    key_end: start + key_len,
    value,
  })
}

// Emitting

enum Out {
  Inline(String),
  Block(Vec<String>),
}

/// `v` written after a key or a dash: inline when it's a scalar, an empty
/// collection or a sequence of scalars that fits on the line (the value
/// starts at column `col`), else as lines indented by `ind`.
fn emit_value(
  v: &Value,
  ind: usize,
  col: usize,
  unit: usize,
) -> Result<Out, WriteError> {
  match v {
    Value::Sequence(items) => {
      if items.iter().all(|item| scalar(item, true).is_some()) {
        let s = flow(v)?;
        if items.is_empty() || col + s.chars().count() <= MAX_WIDTH {
          return Ok(Out::Inline(s));
        }
      }
      let mut lines = Vec::new();
      for item in items {
        lines.extend(emit_item(item, ind, unit)?);
      }
      Ok(Out::Block(lines))
    }
    Value::Mapping(m) if m.is_empty() => Ok(Out::Inline("{}".to_string())),
    Value::Mapping(m) => {
      let mut lines = Vec::new();
      for (k, v) in m {
        lines.extend(emit_pair(k, v, ind, unit)?);
      }
      Ok(Out::Block(lines))
    }
    v => scalar(v, false)
      .map(Out::Inline)
      .ok_or_else(|| unsupported("tagged values can't be written")),
  }
}

fn emit_pair(
  k: &Value,
  v: &Value,
  ind: usize,
  unit: usize,
) -> Result<Vec<String>, WriteError> {
  let head = format!("{}{}:", " ".repeat(ind), emit_key(k, false)?);
  Ok(match emit_value(v, ind + unit, head.len() + 1, unit)? {
    Out::Inline(s) => vec![format!("{head} {s}")],
    Out::Block(body) => std::iter::once(head).chain(body).collect(),
  })
}

fn emit_item(
  v: &Value,
  ind: usize,
  unit: usize,
) -> Result<Vec<String>, WriteError> {
  let dash = format!("{}- ", " ".repeat(ind));
  Ok(match emit_value(v, ind + 2, ind + 2, unit)? {
    Out::Inline(s) => vec![format!("{dash}{s}")],
    Out::Block(mut lines) => {
      lines[0].replace_range(..ind + 2, &dash);
      lines
    }
  })
}

fn emit_key(k: &Value, flow: bool) -> Result<String, WriteError> {
  match k {
    Value::Sequence(_) | Value::Mapping(_) | Value::Tagged(_) => {
      Err(unsupported("only scalar keys can be written"))
    }
    k => Ok(scalar(k, flow).unwrap_or_default()),
  }
}

/// `v` in flow style.
pub(crate) fn flow(v: &Value) -> Result<String, WriteError> {
  Ok(match v {
    Value::Sequence(items) => {
      let items = items.iter().map(flow).collect::<Result<Vec<_>, _>>()?;
      format!("[{}]", items.join(", "))
    }
    Value::Mapping(m) => {
      let mut items = Vec::new();
      for (k, v) in m {
        items.push(format!("{}: {}", emit_key(k, true)?, flow(v)?));
      }
      format!("{{{}}}", items.join(", "))
    }
    v => scalar(v, true)
      .ok_or_else(|| unsupported("tagged values can't be written"))?,
  })
}

fn scalar(v: &Value, flow: bool) -> Option<String> {
  Some(match v {
    Value::Null => "null".to_string(),
    Value::Bool(b) => b.to_string(),
    Value::Number(n) => n.to_string(),
    Value::String(s) if plain_ok(s, flow) => s.clone(),
    Value::String(s) => double_quoted(s),
    _ => return None,
  })
}

fn is_unprintable(c: char) -> bool {
  c.is_control() || matches!(c, '\u{2028}' | '\u{2029}' | '\u{feff}')
}

/// Whether `s` reads back as the same string written plain. In flow
/// context, strings with spaces are quoted too, so that `[sh, -c, "a b"]`
/// shows its items.
fn plain_ok(s: &str, flow: bool) -> bool {
  let b = s.as_bytes();
  let Some(&first) = b.first() else {
    return false;
  };
  if s.trim() != s || s.chars().any(is_unprintable) {
    return false;
  }
  let indicator = match first {
    b'-' | b'?' | b':' => b
      .get(1)
      .is_none_or(|&c| c == b' ' || (flow && is_flow_indicator(c))),
    b',' | b'[' | b']' | b'{' | b'}' | b'#' | b'&' | b'*' | b'!' | b'|'
    | b'>' | b'\'' | b'"' | b'%' | b'@' | b'`' => true,
    _ => false,
  };
  if indicator
    || s.contains(": ")
    || s.contains(" #")
    || s.ends_with(':')
    || (flow && s.contains([',', '[', ']', '{', '}', ' ']))
    || s.starts_with("---")
    || s.starts_with("...")
    || s == "<<"
    || yaml11_special(s)
  {
    return false;
  }
  matches!(serde_yaml::from_str::<Value>(s), Ok(Value::String(t)) if t == s)
}

/// Strings that YAML 1.1 parsers read as booleans, nulls or numbers.
fn yaml11_special(s: &str) -> bool {
  const WORDS: &[&str] = &[
    "y", "yes", "n", "no", "on", "off", "true", "false", "null", "~",
  ];
  let lower = s.to_ascii_lowercase();
  if WORDS.contains(&lower.as_str()) {
    return true;
  }
  let t = lower.strip_prefix(['+', '-']).unwrap_or(&lower);
  matches!(t, ".inf" | ".nan")
    || (t.starts_with(|c: char| c.is_ascii_digit() || c == '.')
      && t.chars().any(|c| c.is_ascii_digit())
      && t
        .chars()
        .all(|c| c.is_ascii_hexdigit() || "xo_.:+-".contains(c)))
}

fn double_quoted(s: &str) -> String {
  let mut out = String::with_capacity(s.len() + 2);
  out.push('"');
  for c in s.chars() {
    match c {
      '"' => out.push_str("\\\""),
      '\\' => out.push_str("\\\\"),
      '\n' => out.push_str("\\n"),
      '\t' => out.push_str("\\t"),
      '\r' => out.push_str("\\r"),
      '\0' => out.push_str("\\0"),
      c if is_unprintable(c) => {
        let _ = write!(out, "\\u{:04X}", c as u32);
      }
      c => out.push(c),
    }
  }
  out.push('"');
  out
}

fn single_quoted(s: &str) -> Option<String> {
  if s.chars().any(is_unprintable) {
    return None;
  }
  Some(format!("'{}'", s.replace('\'', "''")))
}

fn single(name: &str, v: &Value) -> Value {
  let mut m = Mapping::new();
  m.insert(Value::from(name), v.clone());
  Value::Mapping(m)
}

// The same edits on parsed values

/// The key of `m` written as `name`.
fn find_key<'m>(m: &'m Mapping, name: &str) -> Option<&'m Value> {
  m.keys().find(|k| match k {
    Value::String(s) => s == name,
    Value::Number(n) => n.to_string() == name,
    Value::Bool(b) => b.to_string() == name,
    _ => false,
  })
}

fn entry_value<'v>(root: &'v Value, task: &str) -> Option<&'v Value> {
  let tasks = root.get("tasks")?.as_mapping()?;
  tasks.get(find_key(tasks, task)?)
}

/// A merge key or a tag in `v`.
fn uses(v: &Value) -> Option<&'static str> {
  match v {
    Value::Mapping(m) => m.iter().find_map(|(k, v)| match k.as_str() {
      Some("<<") => Some("a merge key (<<)"),
      _ => uses(k).or_else(|| uses(v)),
    }),
    Value::Sequence(items) => items.iter().find_map(uses),
    Value::Tagged(_) => Some("a tag"),
    _ => None,
  }
}

fn put(m: &mut Mapping, name: &str, v: Value) {
  let key = find_key(m, name).cloned().unwrap_or_else(|| name.into());
  m.insert(key, v);
}

fn remove(m: &mut Mapping, name: &str) -> bool {
  match find_key(m, name).cloned() {
    Some(key) => {
      m.retain(|k, _| *k != key);
      true
    }
    None => false,
  }
}

/// The mapping in `v`, made from a null.
fn map_mut(v: &mut Value) -> Option<&mut Mapping> {
  if v.is_null() {
    *v = Value::Mapping(Mapping::new());
  }
  v.as_mapping_mut()
}

fn apply_value(root: &mut Value, edit: &Edit) -> Option<()> {
  let root = map_mut(root)?;
  if !root.contains_key("tasks") {
    if !matches!(edit, Edit::AddTask { .. }) {
      return None;
    }
    root.insert("tasks".into(), Value::Null);
  }
  let tasks = map_mut(root.get_mut("tasks")?)?;
  match edit {
    Edit::AddTask { task, entry } => {
      if find_key(tasks, task).is_some() {
        return None;
      }
      tasks.insert(task.as_str().into(), Value::Mapping(entry.clone()));
    }
    Edit::RemoveTask { task } => {
      let key = find_key(tasks, task)?.clone();
      tasks.retain(|k, _| *k != key);
    }
    Edit::RenameTask { from, to } => {
      let key = find_key(tasks, from)?.clone();
      if from != to {
        if find_key(tasks, to).is_some() {
          return None;
        }
        *tasks = std::mem::take(tasks)
          .into_iter()
          .map(|(k, v)| {
            if k == key {
              (to.as_str().into(), v)
            } else {
              (k, v)
            }
          })
          .collect();
      }
    }
    Edit::Set { task, field, value } => {
      let key = find_key(tasks, task)?.clone();
      set_value(tasks.get_mut(&key)?, field, value.as_ref())?;
    }
  }
  Some(())
}

fn set_value(
  entry: &mut Value,
  field: &str,
  value: Option<&Value>,
) -> Option<()> {
  match (field.strip_prefix("env."), value) {
    (None, Some(v)) => put(map_mut(entry)?, field, v.clone()),
    (None, None) => {
      if !entry.is_null() {
        remove(entry.as_mapping_mut()?, field);
      }
    }
    (Some(name), Some(v)) => {
      let m = map_mut(entry)?;
      match find_key(m, "env").cloned() {
        Some(key) => put(map_mut(m.get_mut(&key)?)?, name, v.clone()),
        None => put(m, "env", single(name, v)),
      }
    }
    (Some(name), None) => {
      if entry.is_null() {
        return Some(());
      }
      let m = entry.as_mapping_mut()?;
      let Some(key) = find_key(m, "env").cloned() else {
        return Some(());
      };
      let env = m.get_mut(&key)?;
      if env.is_null() {
        return Some(());
      }
      let env = env.as_mapping_mut()?;
      if remove(env, name) && env.is_empty() {
        m.retain(|k, _| *k != key);
      }
    }
  }
  Some(())
}

/// Equality that also compares the order of mapping keys.
fn same(a: &Value, b: &Value) -> bool {
  match (a, b) {
    (Value::Mapping(x), Value::Mapping(y)) => {
      x.len() == y.len()
        && x
          .iter()
          .zip(y.iter())
          .all(|((ka, va), (kb, vb))| same(ka, kb) && same(va, vb))
    }
    (Value::Sequence(x), Value::Sequence(y)) => {
      x.len() == y.len() && x.iter().zip(y).all(|(a, b)| same(a, b))
    }
    (Value::Tagged(x), Value::Tagged(y)) => {
      x.tag == y.tag && same(&x.value, &y.value)
    }
    _ => a == b,
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  /// Drops the newline after the opening quote of a raw string.
  fn t(s: &str) -> &str {
    s.strip_prefix('\n').unwrap()
  }

  fn yaml(s: &str) -> Value {
    serde_yaml::from_str(s).unwrap()
  }

  fn set(task: &str, field: &str, value: &str) -> Edit {
    Edit::Set {
      task: task.into(),
      field: field.into(),
      value: Some(yaml(value)),
    }
  }

  fn unset(task: &str, field: &str) -> Edit {
    Edit::Set {
      task: task.into(),
      field: field.into(),
      value: None,
    }
  }

  fn add(task: &str, entry: &str) -> Edit {
    let Value::Mapping(entry) = yaml(entry) else {
      panic!("not a mapping: {entry}");
    };
    Edit::AddTask {
      task: task.into(),
      entry,
    }
  }

  fn remove(task: &str) -> Edit {
    Edit::RemoveTask { task: task.into() }
  }

  fn rename(from: &str, to: &str) -> Edit {
    Edit::RenameTask {
      from: from.into(),
      to: to.into(),
    }
  }

  #[track_caller]
  fn check(text: &str, edits: &[Edit], expected: &str) {
    assert_eq!(apply(text, edits).unwrap(), expected);
  }

  #[track_caller]
  fn refused(text: &str, edits: &[Edit]) -> String {
    match apply(text, edits) {
      Err(WriteError::Unsupported(why)) => why,
      other => panic!("expected a refusal, got {other:?}"),
    }
  }

  const SAMPLE: &str = r#"# comment
tasks:
  db:
    cmd: ["postgres", "-D", ".data/db"]   # trailing comment
    ready: {log: "ready to accept connections"}
  api:
    cmd: [cargo, run, -p, api]
    deps: [db]
    env:
      RUST_LOG: info
      PORT: "3000"
    autostart: true
  services/web:
    cmd: npm run dev
defaults:
  env:
    RUST_LOG: info
"#;

  #[test]
  fn no_edits_keep_the_text() {
    check(SAMPLE, &[], SAMPLE);
    check("", &[], "");
    check("tasks:\n  a: {}", &[], "tasks:\n  a: {}");
  }

  #[test]
  fn set_scalar_in_place() {
    check(
      SAMPLE,
      &[set("api", "autostart", "false")],
      &SAMPLE.replace("    autostart: true", "    autostart: false"),
    );
  }

  #[test]
  fn set_flow_sequence_in_place_keeps_trailing_comment() {
    check(
      SAMPLE,
      &[set("db", "cmd", "[postgres, -D, .data/pg, 'a b']")],
      t(r#"
# comment
tasks:
  db:
    cmd: [postgres, -D, .data/pg, "a b"]   # trailing comment
    ready: {log: "ready to accept connections"}
  api:
    cmd: [cargo, run, -p, api]
    deps: [db]
    env:
      RUST_LOG: info
      PORT: "3000"
    autostart: true
  services/web:
    cmd: npm run dev
defaults:
  env:
    RUST_LOG: info
"#),
    );
  }

  #[test]
  fn set_string_cmd_over_flow_sequence() {
    check(
      SAMPLE,
      &[set("api", "cmd", "cargo run -p api")],
      &SAMPLE.replace(
        "    cmd: [cargo, run, -p, api]",
        "    cmd: cargo run -p api",
      ),
    );
  }

  #[test]
  fn set_inserts_missing_key_at_end_of_entry() {
    check(
      SAMPLE,
      &[set("db", "autostart", "true")],
      t(r#"
# comment
tasks:
  db:
    cmd: ["postgres", "-D", ".data/db"]   # trailing comment
    ready: {log: "ready to accept connections"}
    autostart: true
  api:
    cmd: [cargo, run, -p, api]
    deps: [db]
    env:
      RUST_LOG: info
      PORT: "3000"
    autostart: true
  services/web:
    cmd: npm run dev
defaults:
  env:
    RUST_LOG: info
"#),
    );
  }

  #[test]
  fn set_inserts_into_last_entry_of_tasks() {
    check(
      SAMPLE,
      &[set("services/web", "cwd", "web")],
      t(r#"
# comment
tasks:
  db:
    cmd: ["postgres", "-D", ".data/db"]   # trailing comment
    ready: {log: "ready to accept connections"}
  api:
    cmd: [cargo, run, -p, api]
    deps: [db]
    env:
      RUST_LOG: info
      PORT: "3000"
    autostart: true
  services/web:
    cmd: npm run dev
    cwd: web
defaults:
  env:
    RUST_LOG: info
"#),
    );
  }

  #[test]
  fn set_inserts_into_last_entry_of_file() {
    check(
      "tasks:\n  a:\n    cmd: x\n  b:\n    cmd: y\n",
      &[set("b", "cwd", "dir")],
      "tasks:\n  a:\n    cmd: x\n  b:\n    cmd: y\n    cwd: dir\n",
    );
    // Without a final newline, none is added.
    check(
      "tasks:\n  b:\n    cmd: y",
      &[set("b", "cwd", "dir")],
      "tasks:\n  b:\n    cmd: y\n    cwd: dir",
    );
    // Trailing blank lines stay after it.
    check(
      "tasks:\n  b:\n    cmd: y\n\n\n",
      &[set("b", "cwd", "dir")],
      "tasks:\n  b:\n    cmd: y\n    cwd: dir\n\n\n",
    );
  }

  #[test]
  fn set_inserts_before_comments_of_next_entry() {
    let text = t(r#"
tasks:
  a:
    cmd: x
    # autostart: true

  # B does things.
  b:
    cmd: y
"#);
    check(
      text,
      &[set("a", "cwd", "dir")],
      t(r#"
tasks:
  a:
    cmd: x
    cwd: dir
    # autostart: true

  # B does things.
  b:
    cmd: y
"#),
    );
  }

  #[test]
  fn set_into_entry_without_body() {
    check(
      "tasks:\n  a:\n  b: {}  # empty\n  c: ~\n",
      &[
        set("a", "cwd", "x"),
        set("b", "cwd", "w"),
        set("c", "env.A", "z"),
      ],
      t(r#"
tasks:
  a:
    cwd: x
  b: # empty
    cwd: w
  c:
    env:
      A: z
"#),
    );
  }

  #[test]
  fn remove_key() {
    check(
      SAMPLE,
      &[unset("api", "deps"), unset("api", "nothing")],
      &SAMPLE.replace("    deps: [db]\n", ""),
    );
    check(
      SAMPLE,
      &[unset("api", "env")],
      &SAMPLE
        .replace("    env:\n      RUST_LOG: info\n      PORT: \"3000\"\n", ""),
    );
  }

  #[test]
  fn remove_last_key_leaves_empty_mapping() {
    check(
      "tasks:\n  a:  # first\n    cmd: x  # c\n  b:\n    cmd: y\n",
      &[unset("a", "cmd")],
      "tasks:\n  a: {} # first\n  b:\n    cmd: y\n",
    );
  }

  #[test]
  fn env_var_replace() {
    check(
      SAMPLE,
      &[
        set("api", "env.PORT", "'4000'"),
        set("api", "env.RUST_LOG", "debug"),
      ],
      &SAMPLE
        .replace("      PORT: \"3000\"", "      PORT: \"4000\"")
        .replace(
          "      RUST_LOG: info\n      PORT",
          "      RUST_LOG: debug\n      PORT",
        ),
    );
  }

  #[test]
  fn env_var_add() {
    check(
      SAMPLE,
      &[set("api", "env.DATABASE_URL", "postgres://localhost/db")],
      &SAMPLE.replace(
        "      PORT: \"3000\"\n",
        "      PORT: \"3000\"\n      DATABASE_URL: postgres://localhost/db\n",
      ),
    );
  }

  #[test]
  fn env_var_remove() {
    check(
      SAMPLE,
      &[unset("api", "env.RUST_LOG"), unset("api", "env.MISSING")],
      &SAMPLE.replace("      RUST_LOG: info\n      PORT", "      PORT"),
    );
  }

  #[test]
  fn env_removing_last_var_removes_env() {
    check(
      SAMPLE,
      &[unset("api", "env.RUST_LOG"), unset("api", "env.PORT")],
      t(r#"
# comment
tasks:
  db:
    cmd: ["postgres", "-D", ".data/db"]   # trailing comment
    ready: {log: "ready to accept connections"}
  api:
    cmd: [cargo, run, -p, api]
    deps: [db]
    autostart: true
  services/web:
    cmd: npm run dev
defaults:
  env:
    RUST_LOG: info
"#),
    );
    // The entry is left empty when env was all it had.
    check(
      "tasks:\n  a:\n    env:\n      A: b\n",
      &[unset("a", "env.A")],
      "tasks:\n  a: {}\n",
    );
  }

  #[test]
  fn env_created_when_missing() {
    check(
      SAMPLE,
      &[set("db", "env.PGDATA", ".data/db")],
      &SAMPLE.replace(
        "    ready: {log: \"ready to accept connections\"}\n",
        "    ready: {log: \"ready to accept connections\"}\n    env:\n      \
         PGDATA: .data/db\n",
      ),
    );
    // Removing from a missing env changes nothing.
    check(SAMPLE, &[unset("db", "env.PGDATA")], SAMPLE);
  }

  #[test]
  fn env_empty_gets_variables() {
    check(
      "tasks:\n  a:\n    env: {}  # none yet\n    cwd: x\n",
      &[set("a", "env.A", "1")],
      "tasks:\n  a:\n    env: # none yet\n      A: 1\n    cwd: x\n",
    );
    check(
      "tasks:\n  a:\n    env:\n    cwd: x\n",
      &[set("a", "env.A", "'1'")],
      "tasks:\n  a:\n    env:\n      A: \"1\"\n    cwd: x\n",
    );
    check(
      "tasks:\n  a:\n    env: {}\n",
      &[unset("a", "env.A")],
      "tasks:\n  a:\n    env: {}\n",
    );
  }

  #[test]
  fn env_flow_mapping_is_edited_inline() {
    let text = "tasks:\n  a:\n    env: { A: '1', B: two }  # c\n";
    check(
      text,
      &[set("a", "env.C", "'3'")],
      "tasks:\n  a:\n    env: { A: '1', B: two, C: \"3\" }  # c\n",
    );
    check(
      text,
      &[set("a", "env.A", "x y")],
      "tasks:\n  a:\n    env: { A: \"x y\", B: two }  # c\n",
    );
    check(
      text,
      &[unset("a", "env.A")],
      "tasks:\n  a:\n    env: { B: two }  # c\n",
    );
    check(
      text,
      &[unset("a", "env.B")],
      "tasks:\n  a:\n    env: { A: '1' }  # c\n",
    );
    check(
      text,
      &[unset("a", "env.A"), unset("a", "env.B")],
      "tasks:\n  a: {}\n",
    );
    check(
      "tasks:\n  a:\n    env: {\"A B\": 1, C}\n    cwd: x\n",
      &[set("a", "env.C", "2"), set("a", "env.A B", "3")],
      "tasks:\n  a:\n    env: {\"A B\": 3, C: 2}\n    cwd: x\n",
    );
  }

  #[test]
  fn env_refusals() {
    let why = refused(
      "tasks:\n  a:\n    env: {A: 1,\n      B: 2}\n",
      &[set("a", "env.C", "3")],
    );
    assert_eq!(why, "env of task a is a multi-line flow mapping");
    let why = refused(
      "tasks:\n  a:\n    env: |\n      A=1\n",
      &[set("a", "env.C", "3")],
    );
    assert_eq!(why, "env of task a is not a mapping");
    let why = refused(
      "tasks:\n  a:\n    env:\n      - A=1\n",
      &[set("a", "env.C", "3")],
    );
    assert_eq!(why, "env of task a is not a block mapping");
  }

  #[test]
  fn block_sequence_replaced_by_flow() {
    let text = t(r#"
tasks:
  api:
    # How to run it.
    cmd:  # argv
      - cargo
      - run  # build first
    deps:
    - db
    - cache
    cwd: api
"#);
    check(
      text,
      &[
        set("api", "cmd", "[cargo, build]"),
        set("api", "deps", "[db]"),
      ],
      t(r#"
tasks:
  api:
    # How to run it.
    cmd: [cargo, build] # argv
    deps: [db]
    cwd: api
"#),
    );
  }

  #[test]
  fn long_sequence_written_as_block() {
    let long = "[cargo, run, --release, --package, my-long-package-name, --, \
                --config, config/production.toml]";
    check(
      SAMPLE,
      &[set("db", "cmd", long)],
      &SAMPLE.replace(
        "    cmd: [\"postgres\", \"-D\", \".data/db\"]   # trailing comment\n",
        t(r#"
    cmd: # trailing comment
      - cargo
      - run
      - --release
      - --package
      - my-long-package-name
      - --
      - --config
      - config/production.toml
"#),
      ),
    );
  }

  #[test]
  fn nested_values_written_in_block_style() {
    check(
      SAMPLE,
      &[
        set("db", "ready", "{log: 'listening on: 3000', timeout: 5}"),
        set(
          "services/web",
          "hooks",
          "[{cmd: x, env: {A: b}}, [a, b], s, {}]",
        ),
      ],
      t(r#"
# comment
tasks:
  db:
    cmd: ["postgres", "-D", ".data/db"]   # trailing comment
    ready:
      log: "listening on: 3000"
      timeout: 5
  api:
    cmd: [cargo, run, -p, api]
    deps: [db]
    env:
      RUST_LOG: info
      PORT: "3000"
    autostart: true
  services/web:
    cmd: npm run dev
    hooks:
      - cmd: x
        env:
          A: b
      - [a, b]
      - s
      - {}
defaults:
  env:
    RUST_LOG: info
"#),
    );
  }

  #[test]
  fn block_scalars() {
    let text = t(r#"
tasks:
  a:
    script: |
      echo one
      # not a comment

      echo two
    cwd: x
  b:
    cwd: y
    script: >-
      echo three
      # not a comment
"#);
    check(
      text,
      &[set("a", "script", "run.js"), set("b", "autostart", "true")],
      t(r#"
tasks:
  a:
    script: run.js
    cwd: x
  b:
    cwd: y
    script: >-
      echo three
      # not a comment
    autostart: true
"#),
    );
  }

  #[test]
  fn multi_line_values_and_quoted_text() {
    let text = t(r#"
tasks:
  a:
    label: "x # not a comment: really"  # real comment
    cmd: [sh, -c, 'echo "a, b" # c',
      done]
    note: plain text that
      continues here
    cwd: 'it''s'
  b:
    cmd: y
"#);
    check(
      text,
      &[
        set("a", "label", "z"),
        set("a", "cmd", "[x]"),
        set("a", "note", "short"),
        set("a", "cwd", "dir"),
        set("a", "autostart", "true"),
      ],
      t(r#"
tasks:
  a:
    label: z  # real comment
    cmd: [x]
    note: short
    cwd: dir
    autostart: true
  b:
    cmd: y
"#),
    );
    check(
      "\u{feff}tasks:\n  a:\n    cmd: x\n",
      &[set("a", "cwd", "dir")],
      "\u{feff}tasks:\n  a:\n    cmd: x\n    cwd: dir\n",
    );
  }

  #[test]
  fn values_needing_quotes() {
    let s = |v: &str| scalar(&Value::from(v), false).unwrap();
    for (value, written) in [
      ("true", r#""true""#),
      ("False", r#""False""#),
      ("yes", r#""yes""#),
      ("off", r#""off""#),
      ("null", r#""null""#),
      ("~", r#""~""#),
      ("3000", r#""3000""#),
      ("-1.5", r#""-1.5""#),
      ("0x1F", r#""0x1F""#),
      ("1_000", r#""1_000""#),
      ("12:30", r#""12:30""#),
      (".inf", r#"".inf""#),
      ("", r#""""#),
      (" lead", r#"" lead""#),
      ("a: b", r#""a: b""#),
      ("a #b", r#""a #b""#),
      ("key:", r#""key:""#),
      ("-", r#""-""#),
      ("- x", r#""- x""#),
      ("*.rs", r#""*.rs""#),
      ("&x", r#""&x""#),
      ("!x", r#""!x""#),
      ("@scope/pkg", r#""@scope/pkg""#),
      ("%x", r#""%x""#),
      ("[x]", r#""[x]""#),
      ("{x}", r#""{x}""#),
      ("'x'", r#""'x'""#),
      ("\"x\"", r#""\"x\"""#),
      ("|", r#""|""#),
      ("---", r#""---""#),
      ("<<", r#""<<""#),
      ("line\nbreak", r#""line\nbreak""#),
      ("tab\there", r#""tab\there""#),
      ("back\\slash\u{7}", r#""back\\slash\u0007""#),
    ] {
      assert_eq!(s(value), written, "{value:?}");
      let back: String = serde_yaml::from_str(written).unwrap();
      assert_eq!(back, value);
    }
    for plain in [
      "npm run dev",
      "info",
      ".data/db",
      "-p",
      "-D",
      "--release",
      "services/web",
      "a#b",
      "a:b",
      "http://localhost:3000/x",
      "café",
      "say \"hi\"",
      "it's",
      "v1.2.3",
    ] {
      assert_eq!(s(plain), plain);
    }
    // Flow context also quotes spaces and flow indicators.
    let f = |v: &str| scalar(&Value::from(v), true).unwrap();
    assert_eq!(f("c d"), r#""c d""#);
    assert_eq!(f("a,b"), r#""a,b""#);
    assert_eq!(f("x]"), r#""x]""#);
    assert_eq!(f("-p"), "-p");
    assert_eq!(scalar(&Value::from(true), false).unwrap(), "true");
    assert_eq!(scalar(&Value::from(3000), false).unwrap(), "3000");
    assert_eq!(scalar(&Value::from(1.5), false).unwrap(), "1.5");
    assert_eq!(scalar(&Value::Null, false).unwrap(), "null");

    check(
      "tasks:\n  a:\n    cmd: x\n",
      &[
        set("a", "cwd", "'yes'"),
        set("a", "label", "'a: b # c'"),
        set("a", "cmd", "[sh, -c, 'echo $X: done', '3000', '']"),
        set("a", "port", "3000"),
      ],
      t(r#"
tasks:
  a:
    cmd: [sh, -c, "echo $X: done", "3000", ""]
    cwd: "yes"
    label: "a: b # c"
    port: 3000
"#),
    );
  }

  #[test]
  fn quoted_keys() {
    let text = t(r#"
tasks:
  "api":
    cmd: x
  'web/ui':  # the UI
    cmd: y
  plain:
    cmd: z
"#);
    check(
      text,
      &[
        set("api", "cwd", "srv"),
        rename("api", "server"),
        rename("web/ui", "it's"),
        rename("plain", "8080"),
      ],
      t(r#"
tasks:
  "server":
    cmd: x
    cwd: srv
  'it''s':  # the UI
    cmd: y
  "8080":
    cmd: z
"#),
    );
    check(
      text,
      &[remove("web/ui")],
      "tasks:\n  \"api\":\n    cmd: x\n  plain:\n    cmd: z\n",
    );
  }

  #[test]
  fn four_space_indent() {
    let text = t(r#"
tasks:
    api:
        cmd: x
        env:
            A: "1"
    web:
        cmd: y
"#);
    check(
      text,
      &[
        set("api", "cwd", "dir"),
        set("api", "env.B", "'2'"),
        set("web", "env.C", "c"),
        add(
          "worker",
          "{cmd: [a, b], env: {X: y}, stop: {signal: SIGINT}}",
        ),
      ],
      t(r#"
tasks:
    api:
        cmd: x
        env:
            A: "1"
            B: "2"
        cwd: dir
    web:
        cmd: y
        env:
            C: c
    worker:
        cmd: [a, b]
        env:
            X: "y"
        stop:
            signal: SIGINT
"#),
    );
  }

  #[test]
  fn crlf_file() {
    let text = SAMPLE.replace('\n', "\r\n");
    let expected = t(r#"
# comment
tasks:
  db:
    cmd: ["postgres", "-D", ".data/db"]   # trailing comment
    ready: {log: "ready to accept connections"}
    autostart: true
  api:
    cmd: [cargo, run, -p, api]
    deps: [db]
    env:
      RUST_LOG: info
    autostart: true
  services/web:
    cmd: npm run dev
  jobs:
    cmd: [sleep, "1"]
defaults:
  env:
    RUST_LOG: info
"#)
    .replace('\n', "\r\n");
    check(
      &text,
      &[
        set("db", "autostart", "true"),
        unset("api", "env.PORT"),
        add("jobs", "{cmd: [sleep, '1']}"),
      ],
      &expected,
    );
    assert_eq!(
      refused("a: 1\r\nb: 2\n", &[]),
      "the file mixes line endings"
    );
  }

  #[test]
  fn add_task_appends_entry() {
    check(
      SAMPLE,
      &[add("worker", "{cmd: [cargo, run, -p, worker], autostart: true}")],
      &SAMPLE.replace(
        "    cmd: npm run dev\n",
        "    cmd: npm run dev\n  worker:\n    cmd: [cargo, run, -p, worker]\n    \
         autostart: true\n",
      ),
    );
    check(
      SAMPLE,
      &[add("@web", "{}")],
      &SAMPLE.replace(
        "    cmd: npm run dev\n",
        "    cmd: npm run dev\n  \"@web\": {}\n",
      ),
    );
  }

  #[test]
  fn add_task_spaced_like_the_file() {
    let text = t(r#"
tasks:
  a:
    cmd: x
    # autostart: true

  # B.
  b:
    cmd: y
    # cwd: z

defaults:
  cwd: .
"#);
    check(
      text,
      &[add("c", "{cmd: w}")],
      t(r#"
tasks:
  a:
    cmd: x
    # autostart: true

  # B.
  b:
    cmd: y
    # cwd: z

  c:
    cmd: w

defaults:
  cwd: .
"#),
    );
  }

  #[test]
  fn add_task_without_tasks() {
    check(
      "defaults:\n  env:\n    A: b\n",
      &[add("api", "{cmd: x}")],
      "defaults:\n  env:\n    A: b\ntasks:\n  api:\n    cmd: x\n",
    );
    check(
      "# config\nkernel: {}\n\ndefaults:\n    cwd: .\n",
      &[add("api", "{cmd: x}")],
      "# config\nkernel: {}\n\ndefaults:\n    cwd: .\n\ntasks:\n    api:\n        \
       cmd: x\n",
    );
    check(
      "",
      &[add("api", "{cmd: x}")],
      "tasks:\n  api:\n    cmd: x\n",
    );
    check(
      "# nothing yet\n",
      &[add("api", "{}")],
      "# nothing yet\ntasks:\n  api: {}\n",
    );
  }

  #[test]
  fn add_task_into_empty_tasks() {
    check(
      "tasks: {}\n",
      &[add("api", "{cmd: [sleep, '60']}")],
      "tasks:\n  api:\n    cmd: [sleep, \"60\"]\n",
    );
    check(
      "tasks:  # none yet\ndefaults: {}\n",
      &[add("api", "{cmd: x}")],
      "tasks:  # none yet\n  api:\n    cmd: x\ndefaults: {}\n",
    );
    check(
      "tasks:\n  # add tasks here\n\ndefaults: {}\n",
      &[add("api", "{cmd: x}")],
      "tasks:\n  # add tasks here\n  api:\n    cmd: x\n\ndefaults: {}\n",
    );
  }

  #[test]
  fn add_task_that_exists() {
    assert_eq!(
      refused(SAMPLE, &[add("api", "{cmd: x}")]),
      "task api already exists"
    );
  }

  const SPACED: &str = r#"# Tasks.
tasks:
  # The database.
  db:
    cmd: postgres

  # The API.
  # Needs db.
  api:
    cmd: cargo run
    # autostart: true

  web:
    cmd: npm run dev

defaults:
  autostart: true
"#;

  #[test]
  fn remove_first_task() {
    check(
      SPACED,
      &[remove("db")],
      t(r#"
# Tasks.
tasks:
  # The API.
  # Needs db.
  api:
    cmd: cargo run
    # autostart: true

  web:
    cmd: npm run dev

defaults:
  autostart: true
"#),
    );
  }

  #[test]
  fn remove_middle_task() {
    check(
      SPACED,
      &[remove("api")],
      t(r#"
# Tasks.
tasks:
  # The database.
  db:
    cmd: postgres

  web:
    cmd: npm run dev

defaults:
  autostart: true
"#),
    );
    check(
      SAMPLE,
      &[remove("api")],
      t(r#"
# comment
tasks:
  db:
    cmd: ["postgres", "-D", ".data/db"]   # trailing comment
    ready: {log: "ready to accept connections"}
  services/web:
    cmd: npm run dev
defaults:
  env:
    RUST_LOG: info
"#),
    );
  }

  #[test]
  fn remove_last_task() {
    check(
      SPACED,
      &[remove("web")],
      t(r#"
# Tasks.
tasks:
  # The database.
  db:
    cmd: postgres

  # The API.
  # Needs db.
  api:
    cmd: cargo run
    # autostart: true

defaults:
  autostart: true
"#),
    );
    check(
      "tasks:\n  a:\n    cmd: x\n\n  b:\n    cmd: y\n",
      &[remove("b")],
      "tasks:\n  a:\n    cmd: x\n",
    );
  }

  #[test]
  fn remove_only_task() {
    check(
      "tasks:\n  # The one.\n  a:\n    cmd: x\n\ndefaults: {}\n",
      &[remove("a")],
      "tasks: {}\n\ndefaults: {}\n",
    );
    check(
      SPACED,
      &[remove("db"), remove("web"), remove("api")],
      "# Tasks.\ntasks: {}\n\ndefaults:\n  autostart: true\n",
    );
  }

  #[test]
  fn rename_task() {
    check(
      SAMPLE,
      &[rename("api", "backend")],
      &SAMPLE.replace("  api:\n", "  backend:\n"),
    );
    check(
      SAMPLE,
      &[rename("services/web", "true")],
      &SAMPLE.replace("  services/web:\n", "  \"true\":\n"),
    );
    check(SAMPLE, &[rename("api", "api")], SAMPLE);
    assert_eq!(
      refused(SAMPLE, &[rename("api", "db")]),
      "task db already exists"
    );
  }

  #[test]
  fn several_edits() {
    check(
      SAMPLE,
      &[
        rename("api", "backend"),
        set("backend", "env.PORT", "'8080'"),
        remove("db"),
        unset("backend", "deps"),
        add("db", "{cmd: postgres}"),
      ],
      t(r#"
# comment
tasks:
  backend:
    cmd: [cargo, run, -p, api]
    env:
      RUST_LOG: info
      PORT: "8080"
    autostart: true
  services/web:
    cmd: npm run dev
  db:
    cmd: postgres
defaults:
  env:
    RUST_LOG: info
"#),
    );
  }

  #[test]
  fn missing_tasks() {
    let no = |text: &str, edit: Edit| apply(text, &[edit]).unwrap_err();
    let nope = WriteError::NoTask("nope".into());
    assert_eq!(no(SAMPLE, set("nope", "cwd", "x")), nope);
    assert_eq!(no(SAMPLE, remove("nope")), nope);
    assert_eq!(no(SAMPLE, rename("nope", "x")), nope);
    assert_eq!(no("defaults: {}\n", remove("nope")), nope);
    assert_eq!(no("tasks: {}\n", remove("nope")), nope);
    assert_eq!(no("", set("nope", "cwd", "x")), nope);
  }

  #[test]
  fn flow_mappings_refused() {
    let text = "tasks: {a: {cmd: x}}\n";
    assert_eq!(
      refused(text, &[set("a", "cwd", "y")]),
      "tasks is a flow mapping"
    );
    assert_eq!(refused(text, &[add("b", "{}")]), "tasks is a flow mapping");
    assert_eq!(
      refused("{tasks: {}}\n", &[add("b", "{}")]),
      "the top level of the file is not a block mapping"
    );

    let text = "tasks:\n  a: {cmd: x}\n  b: {cmd: y}\n";
    assert_eq!(
      refused(text, &[set("a", "cwd", "y")]),
      "task a is a flow mapping"
    );
    // The value isn't touched when removing or renaming.
    check(text, &[remove("a")], "tasks:\n  b: {cmd: y}\n");
    check(
      text,
      &[rename("a", "c")],
      "tasks:\n  c: {cmd: x}\n  b: {cmd: y}\n",
    );
    check(
      text,
      &[add("c", "{cmd: z}")],
      "tasks:\n  a: {cmd: x}\n  b: {cmd: y}\n  c:\n    cmd: z\n",
    );
  }

  #[test]
  fn yaml_features_refused() {
    let cases = [
      ("tasks:\n  a: &base\n    cmd: x\n", "task a uses an anchor"),
      (
        "x: &c [a]\ntasks:\n  a:\n    cmd: *c\n",
        "task a uses an alias",
      ),
      (
        "tasks:\n  a:\n    <<: {cwd: x}\n    cmd: y\n",
        "task a uses a merge key (<<)",
      ),
      ("tasks:\n  a:\n    cmd: !shell x\n", "task a uses a tag"),
      (
        "tasks:\n  a:\n    ? cmd\n    : x\n",
        "task a uses a complex key",
      ),
      ("tasks: !!map\n  a:\n    cmd: x\n", "tasks uses a tag"),
      ("tasks:\n  a: x\n", "task a is not a mapping"),
    ];
    for (text, why) in cases {
      assert_eq!(refused(text, &[set("a", "cwd", "y")]), why, "{text}");
    }
    // Other entries may use them, and renaming doesn't touch them.
    check(
      "x: &c [a]\ntasks:\n  a:\n    cmd: *c\n  b:\n    cmd: y\n",
      &[set("b", "cwd", "z"), rename("a", "c")],
      "x: &c [a]\ntasks:\n  c:\n    cmd: *c\n  b:\n    cmd: y\n    cwd: z\n",
    );
    // A `!js` value is never replaced by what it computed.
    assert_eq!(
      refused(
        "tasks:\n  a:\n    cmd: !js ctx.dir\n",
        &[set("a", "cwd", "y")]
      ),
      "task a uses a tag"
    );
    check(
      "vars:\n  d: !js ctx.dir\ntasks:\n  a:\n    cmd: !js '[ctx.vars.d]'\n  b:\n    cmd: y\n",
      &[set("b", "cwd", "z")],
      "vars:\n  d: !js ctx.dir\ntasks:\n  a:\n    cmd: !js '[ctx.vars.d]'\n  b:\n    cmd: y\n    cwd: z\n",
    );
    // Removing an anchor could break its aliases.
    assert_eq!(
      refused(
        "tasks:\n  a:\n    cmd: &c [x]\n  b:\n    cmd: y\n",
        &[remove("a")]
      ),
      "task a defines an anchor"
    );
  }

  #[test]
  fn file_refusals() {
    let edit = [set("a", "cwd", "dir")];
    assert_eq!(
      refused("tasks:\n  a:\n    cmd: x\n---\nb: 1\n", &edit),
      "the file has more than one YAML document"
    );
    assert_eq!(
      refused("tasks:\n  a:\n    cmd: x\n...\n", &edit),
      "the file has a document end marker (...)"
    );
    assert_eq!(
      refused("tasks:\n\ta:\n", &edit),
      "the file uses tabs for indentation"
    );
    assert!(
      refused("tasks: [\n", &edit).starts_with("the file is not valid YAML")
    );
    assert_eq!(
      refused("- a\n", &edit),
      "the top level of the file is not a mapping"
    );
    // A document start before the content is fine.
    check(
      "%YAML 1.2\n---  # config\ntasks:\n  a:\n    cmd: x\n",
      &edit,
      "%YAML 1.2\n---  # config\ntasks:\n  a:\n    cmd: x\n    cwd: dir\n",
    );
  }

  #[test]
  fn safety_net() {
    // The kept trailing lines of a `|+` scalar would end up after the
    // new key.
    let text =
      "tasks:\n  a:\n    script: |+\n      echo hi\n\n  b:\n    cmd: x\n";
    assert_eq!(
      refused(text, &[set("a", "cwd", "y")]),
      "could not edit task a safely"
    );
  }

  #[test]
  fn semantic_edits() {
    let mut v = yaml("tasks:\n  a:\n    env: {A: 1}\n    cwd: x\n  b: ~\n");
    apply_value(&mut v, &unset("a", "env.A")).unwrap();
    apply_value(&mut v, &rename("a", "c")).unwrap();
    apply_value(&mut v, &set("b", "cwd", "y")).unwrap();
    apply_value(&mut v, &add("d", "{}")).unwrap();
    assert!(same(&v, &yaml("tasks: {c: {cwd: x}, b: {cwd: y}, d: {}}")));
    assert!(!same(&v, &yaml("tasks: {b: {cwd: y}, c: {cwd: x}, d: {}}")));
    assert!(apply_value(&mut v, &add("d", "{}")).is_none());
    assert!(apply_value(&mut v, &remove("x")).is_none());
  }

  #[test]
  fn error_messages() {
    assert_eq!(
      WriteError::Unsupported("env of task api is a flow mapping".into())
        .to_string(),
      "can't edit the config file: env of task api is a flow mapping"
    );
    assert_eq!(
      WriteError::NoTask("api".into()).to_string(),
      "task api is not in the config file"
    );
  }
}
