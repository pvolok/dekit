use super::ir::{Block, DocsIr, FieldKind, Inline};
use super::{compile, load};

const MANIFEST: &str = "sections:
  - dir: start
    title: Start
  - dir: cli
    title: CLI
";

fn topic(frontmatter: &str, body: &str) -> String {
  format!("---\n{frontmatter}\n---\n\n{body}")
}

fn home(body: &str) -> String {
  topic("title: Home\nsummary: Home page.", body)
}

const UP: &str = "---
title: dekit up
cli: dekit up
related: [index]
---

`dekit up` waits on `ready_log`.

:::usage
";

fn compile_ok(files: &[(&str, &str)], manifest: &str) -> DocsIr {
  match compile(files, manifest, &crate::dekit::main::cli()) {
    Ok(ir) => ir,
    Err(errors) => panic!("{}", super::error::join(&errors)),
  }
}

fn compile_err(files: &[(&str, &str)], manifest: &str) -> String {
  match compile(files, manifest, &crate::dekit::main::cli()) {
    Ok(_) => panic!("expected compile errors"),
    Err(errors) => super::error::join(&errors),
  }
}

fn one(files: &[(&str, &str)]) -> DocsIr {
  compile_ok(files, "sections:\n  - dir: start\n    title: Start\n")
}

fn one_err(body: &str) -> String {
  let home = home(body);
  let getting_started =
    topic("title: Getting started\nsummary: Install.", "Hello.\n");
  compile_err(
    &[
      ("index.md", &home),
      ("start/getting-started.md", &getting_started),
    ],
    "sections:\n  - dir: start\n    title: Start\n",
  )
}

fn first_paragraph(ir: &DocsIr, id: &str) -> Vec<Inline> {
  match ir.topic(id).unwrap().blocks.first() {
    Some(Block::Paragraph { inlines }) => inlines.clone(),
    other => panic!("expected a paragraph, got {other:?}"),
  }
}

fn text(text: &str) -> Inline {
  Inline::Text {
    text: text.to_string(),
  }
}

fn code(text: &str) -> Inline {
  Inline::Code {
    text: text.to_string(),
  }
}

#[test]
fn ids_come_from_paths() {
  let home = home("Welcome to |cli/up|.\n");
  let cli = topic("title: CLI\ncli: dekit", ":::usage\n");
  let getting_started =
    topic("title: Getting started\nsummary: Install.", "Hello.\n");
  let ir = compile_ok(
    &[
      ("index.md", &home),
      ("start/getting-started.md", &getting_started),
      ("cli/index.md", &cli),
      ("cli/up.md", UP),
    ],
    MANIFEST,
  );
  let mut ids: Vec<&str> = ir.topics.iter().map(|t| t.id.as_str()).collect();
  ids.sort();
  assert_eq!(ids, ["cli", "cli/up", "index", "start/getting-started"]);
  let up = ir.topic("cli/up").unwrap();
  assert_eq!(up.slug, "cli/up");
  assert_eq!(up.section, "cli");
  assert_eq!(ir.topic("index").unwrap().section, "");
  assert_eq!(ir.home, "index");
  assert_eq!(ir.nav.len(), 2);
  assert_eq!(ir.nav[1].topics, ["cli", "cli/up"]);
}

#[test]
fn flanking_underscores_and_code_spans() {
  let home = home("Uses ready_log, _ready_, and `ready_log`.\n");
  let ir = one(&[
    ("index.md", &home),
    ("start/a.md", &topic("title: A\nsummary: A.", "A.\n")),
  ]);
  assert_eq!(
    first_paragraph(&ir, "index"),
    [
      text("Uses ready_log, "),
      Inline::Em {
        children: vec![text("ready")],
      },
      text(", and "),
      code("ready_log"),
      text("."),
    ]
  );
}

#[test]
fn whole_line_tag_is_a_paragraph() {
  let home = home("|cli/up|\n");
  let cli = topic("title: CLI\ncli: dekit", ":::usage\n");
  let ir = compile_ok(
    &[
      ("index.md", &home),
      ("start/a.md", &topic("title: A\nsummary: A.", "A.\n")),
      ("cli/index.md", &cli),
      ("cli/up.md", UP),
    ],
    MANIFEST,
  );
  assert_eq!(
    first_paragraph(&ir, "index"),
    [Inline::TagLink {
      tag: "cli/up".to_string()
    }]
  );
}

#[test]
fn every_container_compiles() {
  let body = ":::callout note
A note about the runner.
:::

:::callout tip title=\"From another repo\"
dekit up -C ~/src/api
:::

## Commands

:::commands
- cmd: dekit up
  desc: Start autostart tasks.
  tag: cmd-up
:::

:::fields kind=config
- key: cmd
  type: \"string | string[]\"
  required: true
  desc: Command to run.
  tag: cfg-cmd
:::

:::fields kind=js
- key: std.dekit.start
  signature: \"(target: string) => Promise<number>\"
  desc: Start matching tasks.
  tag: js-dekit-start
:::

:::fields kind=cli-flag
- key: \"-C, --chdir <dir>\"
  type: path
  desc: Explicit project root.
:::

:::footnote
See |cmd-up| and |index#commands|.
:::
";
  let home = home(body);
  let ir = one(&[
    ("index.md", &home),
    ("start/a.md", &topic("title: A\nsummary: A.", "A.\n")),
  ]);
  assert_eq!(ir.commands[0].cmd, "dekit up");
  let mut kinds: Vec<FieldKind> = ir.fields.iter().map(|f| f.kind).collect();
  kinds.sort_by_key(|kind| kind.name());
  assert_eq!(
    kinds,
    [FieldKind::CliFlag, FieldKind::Config, FieldKind::Js]
  );
  assert_eq!(ir.tags["cmd-up"].topic, "index");
  assert_eq!(
    ir.tags["index#commands"].heading.as_deref(),
    Some("commands")
  );
  let home = ir.topic("index").unwrap();
  assert!(matches!(home.blocks.last(), Some(Block::Footnote { .. })));
  match &home.blocks[1] {
    Block::Callout { title, .. } => {
      assert_eq!(title.as_deref(), Some("From another repo"))
    }
    other => panic!("expected callout, got {other:?}"),
  }
}

#[test]
fn heading_ids() {
  let home = home(
    "## Usage\n\n## What it actually does {#semantics}\n\n## 1 overview\n",
  );
  let ir = one(&[
    ("index.md", &home),
    ("start/a.md", &topic("title: A\nsummary: A.", "A.\n")),
  ]);
  let ids: Vec<&str> = ir
    .topic("index")
    .unwrap()
    .headings
    .iter()
    .map(|h| h.id.as_str())
    .collect();
  assert_eq!(ids, ["usage", "semantics", "h-1-overview"]);
  assert_eq!(
    ir.tags["index#semantics"].heading.as_deref(),
    Some("semantics")
  );
}

#[test]
fn forbidden_constructs_fail() {
  let cases = [
    ("<div>nope</div>\n", "HTML"),
    ("> quoted\n", "blockquotes"),
    ("Hello\n===\n", "setext"),
    ("```\nno close\n", "unclosed code fence"),
    (":::tabs\n:::\n", "unknown container \"tabs\""),
    ("## ???\n", "slugifiable"),
    ("See |missing-tag|.\n", "unknown tag"),
    (
      "A ![alt](https://x.com/i.png) picture.\n",
      "images are a block",
    ),
    (":::callout note\nstill open\n", "unclosed container"),
    ("| cmd | desc |\n", "GFM tables"),
    ("* star\n", "use '- '"),
    ("---\n", "thematic"),
    ("[x]: https://x.com\n", "reference-style"),
    ("See <https://x.com>.\n", "autolinks"),
    ("See [x](docs/x).\n", "http or https"),
    ("**open\n", "unclosed **strong**"),
    ("An _open em\n", "unclosed _em_"),
    (
      ":::callout\n:::commands\n- cmd: x\n  desc: y\n:::\n:::\n",
      "nested containers",
    ),
    (
      "Text\n\n:::footnote\nnote\n:::\n\nAfter.\n",
      "must be the last block",
    ),
    (":::image src=/img/x.png alt=x\n", "https://"),
  ];
  for (body, needle) in cases {
    let message = one_err(body);
    assert!(message.contains(needle), "{body:?}: {message}");
  }
}

#[test]
fn indented_closer_is_accepted() {
  let home = home(
    ":::commands\n- cmd: dekit up\n  desc: Start.\n  :::\n\nAfter the list.\n",
  );
  let ir = one(&[
    ("index.md", &home),
    ("start/a.md", &topic("title: A\nsummary: A.", "A.\n")),
  ]);
  let blocks = &ir.topic("index").unwrap().blocks;
  assert!(
    matches!(&blocks[0], Block::Commands { items } if items[0].cmd == "dekit up")
  );
  assert!(matches!(&blocks[1], Block::Paragraph { .. }));
}

#[test]
fn frontmatter_rules() {
  let a = topic("title: A\nsummary: A.", "A.\n");
  let manifest = "sections:\n  - dir: start\n    title: Start\n";
  let cases: [(&str, &str); 6] = [
    ("---\ntitle: Home\n---\n\nx\n", "summary is required"),
    (
      "---\ntitle: Home\nsummary: x\nslug: index\n---\n\nx\n",
      "unknown frontmatter key",
    ),
    (
      "---\ntitle: Home\nsummary: x\ncli: dekit\n---\n\n:::usage\n",
      "summary is forbidden",
    ),
    (
      "---\ntitle: Home\ncli: dekit frobnicate\n---\n\n:::usage\n",
      "not a dekit command",
    ),
    (
      "---\ntitle: Home\ncli: dekit\n---\n\nno usage\n",
      "must contain :::usage",
    ),
    (
      "---\ntitle: Home\nsummary: x\n---\n\n:::usage\n",
      "needs a cli: key",
    ),
  ];
  for (home, needle) in cases {
    let message =
      compile_err(&[("index.md", home), ("start/a.md", &a)], manifest);
    assert!(message.contains(needle), "{home:?}: {message}");
  }
}

#[test]
fn layout_rules() {
  let home = home("x\n");
  let a = topic("title: A\nsummary: A.", "A.\n");
  let manifest = "sections:\n  - dir: start\n    title: Start\n";
  let message =
    compile_err(&[("index.md", &home), ("targets.md", &a)], manifest);
  assert!(
    message.contains("only index.md may sit at the top level"),
    "{message}"
  );
  let message =
    compile_err(&[("index.md", &home), ("guide/a.md", &a)], manifest);
  assert!(
    message.contains("section \"guide\" is not in index.yaml"),
    "{message}"
  );
  let message = compile_err(
    &[("index.md", &home), ("start/a.md", &a)],
    "sections:\n  - dir: start\n    title: Start\n  - dir: js\n    title: JS\n",
  );
  assert!(
    message.contains("section \"js\" has no topics"),
    "{message}"
  );
  let message = compile_err(&[("start/a.md", &a)], manifest);
  assert!(message.contains("index.md is missing"), "{message}");
}

#[test]
fn hidden_topics() {
  let secret = topic(
    "title: Secret\nsummary: Hidden.\nhidden: true",
    "Secret |start/other|.\n",
  );
  let other = topic(
    "title: Other\nsummary: Hidden too.\nhidden: true",
    "Other.\n",
  );
  let a = topic("title: A\nsummary: A.", "A.\n");
  let manifest = "sections:\n  - dir: start\n    title: Start\n  - dir: rpc\n    title: RPC\n";
  let index = home("Hello.\n");
  let ir = compile_ok(
    &[
      ("index.md", &index),
      ("start/a.md", &a),
      ("start/other.md", &other),
      ("rpc/secret.md", &secret),
    ],
    manifest,
  );
  assert!(ir.topic("rpc/secret").unwrap().hidden);
  assert_eq!(ir.nav.len(), 1, "an all-hidden section leaves the nav");
  assert_eq!(ir.nav[0].topics, ["start/a"]);
  let export = ir.export();
  assert!(export.topic("rpc/secret").is_none());
  assert!(!export.tags.contains_key("rpc/secret"));
  assert!(ir.tags.contains_key("rpc/secret"));

  let linked = home("See |rpc/secret|.\n");
  let message = compile_err(
    &[
      ("index.md", &linked),
      ("start/a.md", &a),
      ("rpc/secret.md", &secret),
      ("start/other.md", &other),
    ],
    manifest,
  );
  assert!(
    message.contains("is hidden; a visible page cannot link to it"),
    "{message}"
  );
}

#[test]
fn commands_dedupe_by_nav_order() {
  let home = home(":::commands\n- cmd: dekit up\n  desc: From index.\n:::\n");
  let a = topic(
    "title: A\nsummary: A.",
    ":::commands\n- cmd: dekit up\n  desc: From a.\n:::\n",
  );
  let ir = one(&[("index.md", &home), ("start/a.md", &a)]);
  assert_eq!(ir.commands.len(), 1);
  assert_eq!(ir.commands[0].desc, "From index.");
}

#[test]
fn oversize_file_fails() {
  let big = "x".repeat(64 * 1024 + 1);
  let message = one_err(&big);
  assert!(message.contains("byte cap"), "{message}");
}

#[test]
fn usage_expands_from_clap() {
  let home = home("Hello.\n");
  let cli = topic("title: CLI\ncli: dekit", "Intro.\n\n:::usage\n");
  let a = topic("title: A\nsummary: A.", "A.\n");
  let ir = compile_ok(
    &[
      ("index.md", &home),
      ("start/a.md", &a),
      ("cli/index.md", &cli),
      ("cli/up.md", UP),
    ],
    MANIFEST,
  );
  let up = ir.topic("cli/up").unwrap();
  assert_eq!(up.summary, "Start the runner if needed and the autostart tasks");
  match &up.blocks[1] {
    Block::Code { text, .. } => assert_eq!(text, "dekit up [OPTIONS]"),
    other => panic!("expected the usage line, got {other:?}"),
  }
  let root = ir.topic("cli").unwrap();
  match &root.blocks[2] {
    Block::Fields { items, .. } => {
      let keys: Vec<&str> = items.iter().map(|f| f.key.as_str()).collect();
      assert!(keys.contains(&"[files]..."), "{keys:?}");
      assert!(keys.contains(&"-C, --chdir <chdir>"), "{keys:?}");
      assert!(keys.contains(&"--json"), "{keys:?}");
      assert!(!keys.iter().any(|k| k.contains("--help")), "{keys:?}");
    }
    other => panic!("expected flags, got {other:?}"),
  }
  match &root.blocks[3] {
    Block::Commands { items } => {
      assert!(items.iter().any(|c| c.cmd == "dekit up"));
      assert!(items.iter().any(|c| c.cmd == "dekit runner"));
    }
    other => panic!("expected subcommands, got {other:?}"),
  }
}

#[test]
fn export_json_shape() {
  let home = home("Hello.\n");
  let a = topic("title: A\nsummary: A.", "A.\n");
  let ir = one(&[("index.md", &home), ("start/a.md", &a)]);
  // Field order is the wire shape dekit-web reads; pin it as text.
  let json = serde_json::to_string(&ir.export()).unwrap();
  assert!(json.starts_with("{\"version\":1,\"dekit\":\""), "{json}");
  let home = "\"home\":\"index\",\"topics\":[{\"id\":\"index\",\"title\":\"Home\",\
    \"slug\":\"index\",\"section\":\"\",\"summary\":\"Home page.\",\"tags\":[\"index\"],\
    \"related\":[],\"headings\":[],\"blocks\":[{\"type\":\"paragraph\",\"inlines\":\
    [{\"type\":\"text\",\"text\":\"Hello.\"}]}],\"source\":\"index.md\"}";
  assert!(json.contains(home), "{json}");
  assert!(json.contains("\"nav\":[{\"id\":\"start\",\"title\":\"Start\",\"topics\":[\"start/a\"]}]"), "{json}");
  assert!(json.ends_with("\"commands\":[],\"fields\":[]}"), "{json}");
}

#[test]
fn embedded_docs_compile() {
  if let Err(errors) = load() {
    panic!("{}", super::error::join(&errors));
  }
}

fn command_paths(cmd: &clap::Command, path: String, out: &mut Vec<String>) {
  out.push(path.clone());
  for sub in cmd.get_subcommands().filter(|sub| !sub.is_hide_set()) {
    command_paths(sub, format!("{path} {}", sub.get_name()), out);
  }
}

#[test]
fn every_command_has_a_topic() {
  let docs =
    load().unwrap_or_else(|errors| panic!("{}", super::error::join(&errors)));
  let mut commands = Vec::new();
  command_paths(
    &crate::dekit::main::cli(),
    "dekit".to_string(),
    &mut commands,
  );
  commands.sort();
  let mut documented: Vec<String> =
    docs.topics.iter().filter_map(|t| t.cli.clone()).collect();
  documented.sort();
  assert_eq!(commands, documented);
}

#[test]
fn config_keys_match_code_schema_and_docs() {
  use std::collections::BTreeSet;

  use crate::config::config::{PRESENTATION_KEYS, ROOT_KEYS};
  use crate::config::task::{TASK_KEYS, TASK_SETTING_KEYS};

  let docs =
    load().unwrap_or_else(|errors| panic!("{}", super::error::join(&errors)));
  let mut docs_root = BTreeSet::new();
  let mut docs_task = BTreeSet::new();
  for topic in &docs.topics {
    for block in &topic.blocks {
      let Block::Fields {
        kind: FieldKind::Config,
        items,
      } = block
      else {
        continue;
      };
      for item in items {
        if let Some(key) = item.key.strip_prefix("tasks.*.") {
          docs_task.insert(key.to_string());
        } else if !item.key.contains('.') {
          docs_root.insert(item.key.clone());
        }
      }
    }
  }
  let set = |keys: &[&str]| -> BTreeSet<String> {
    keys.iter().map(|k| k.to_string()).collect()
  };
  let code_root: BTreeSet<String> = ROOT_KEYS
    .iter()
    .filter(|key| !PRESENTATION_KEYS.contains(key))
    .map(|key| key.to_string())
    .collect();
  assert_eq!(docs_root, code_root, "project keys: docs vs code");
  assert_eq!(docs_task, set(TASK_KEYS), "task keys: docs vs code");

  let schema: serde_json::Value =
    serde_json::from_str(include_str!("../../schemas/dekit.json")).unwrap();
  let keys = |value: &serde_json::Value| -> BTreeSet<String> {
    value.as_object().unwrap().keys().cloned().collect()
  };
  assert_eq!(
    keys(&schema["properties"]),
    code_root,
    "project keys: schema vs code"
  );
  let settings = keys(&schema["$defs"]["taskSettings"]["properties"]);
  assert_eq!(
    settings,
    set(TASK_SETTING_KEYS),
    "task settings: schema vs code"
  );
  let mut schema_task = settings;
  schema_task.extend(keys(&schema["$defs"]["task"]["allOf"][1]["properties"]));
  assert_eq!(schema_task, set(TASK_KEYS), "task keys: schema vs code");
}

const JS_MEMBERS: &str = r#"(() => {
  const out = [];
  const walk = (obj, prefix) => {
    for (const key of Object.keys(obj)) {
      const value = obj[key];
      const name = prefix + "." + key;
      if (value !== null && typeof value === "object" && !Array.isArray(value)) {
        walk(value, name);
      } else {
        out.push(name);
      }
    }
  };
  walk(std, "std");
  return JSON.stringify(out.sort());
})()"#;

#[tokio::test]
async fn every_js_member_has_a_record() {
  use std::collections::BTreeSet;

  let docs =
    load().unwrap_or_else(|errors| panic!("{}", super::error::join(&errors)));
  let vm = crate::js::js_vm::JsVm::new(None).await.unwrap();
  let json: String =
    rquickjs::AsyncContext::async_with(&vm.context, async |ctx| {
      ctx
        .eval::<String, _>(JS_MEMBERS)
        .map_err(|err| err.to_string())
    })
    .await
    .unwrap();
  let runtime: BTreeSet<String> = serde_json::from_str::<Vec<String>>(&json)
    .unwrap()
    .into_iter()
    .collect();
  let documented: BTreeSet<String> = docs
    .topics
    .iter()
    .flat_map(|topic| topic.blocks.iter())
    .filter_map(|block| match block {
      Block::Fields {
        kind: FieldKind::Js,
        items,
      } => Some(items),
      _ => None,
    })
    .flatten()
    .map(|field| field.key.clone())
    .filter(|key| key.starts_with("std."))
    .collect();
  assert_eq!(runtime, documented, "std members: runtime vs docs");
}
