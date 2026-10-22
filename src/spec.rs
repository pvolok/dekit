//! A task's definition as written: an entry of `tasks:` in dekit.yaml
//! with `defaults` applied and every path made absolute. Runtime tasks
//! have one too, built from what `add` was given. The kernel keeps it
//! next to the task so it can be shown, edited, saved, compared with the
//! config, and written back to it.

use std::path::Path;
use std::time::Duration;

use anyhow::Result;
use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use serde_yaml::{Mapping, Value};

use crate::cfg::{CfgCx, CfgDoc};
use crate::config::stop_signal::StopSignal;
use crate::config::task::{CmdConfig, TASK_KEYS, TaskConfig, task_from_cfg};
use crate::config::task_log::{LogMode, TaskLogConfig};
use crate::config::writer;
use crate::kernel::task::TaskKind;
use crate::parse_shell::split_argv;
use crate::task::ready::{DEFAULT_INTERVAL, Probe, ReadyCheck};

#[derive(
  Clone, Debug, Default, Deserialize, Eq, Hash, PartialEq, Serialize,
)]
#[serde(transparent)]
pub struct TaskSpec(pub Mapping);

impl TaskSpec {
  /// A task that runs this argv.
  pub fn with_cmd(argv: Vec<String>) -> TaskSpec {
    let mut spec = TaskSpec::default();
    spec.set_strings("cmd", argv);
    spec
  }

  /// The entry a task config reads back as.
  pub fn from_config(cfg: &TaskConfig) -> TaskSpec {
    let mut spec = TaskSpec::default();
    let s = |text: &str| Value::String(text.to_string());
    let strings = |items: &[String]| {
      Value::Sequence(items.iter().map(|item| s(item)).collect())
    };
    if let Some(label) = &cfg.label {
      spec.set("label", Some(s(label)));
    }
    if cfg.kind == TaskKind::Job {
      spec.set("type", Some(s("job")));
    }
    match &cfg.cmd {
      Some(CmdConfig::Cmd { cmd }) => spec.set("cmd", Some(strings(cmd))),
      Some(CmdConfig::Script { script }) => {
        spec.set("script", Some(s(&written_path(script))))
      }
      None => (),
    }
    spec.set_deps(cfg.deps.clone());
    spec.set_strings("tags", cfg.tags.clone());
    if let Some(ready) = &cfg.ready {
      let mut map = Mapping::new();
      match &ready.check {
        ReadyCheck::Log(text) => {
          map.insert(s("log"), s(text));
        }
        ReadyCheck::SdNotify => {
          map.insert(s("sd_notify"), Value::Bool(true));
        }
        ReadyCheck::Probe { probe, interval } => {
          let (key, value) = match probe {
            Probe::Tcp { host: None, port } => ("tcp", Value::from(*port)),
            Probe::Tcp {
              host: Some(host),
              port,
            } => {
              let host = if host.contains(':') {
                format!("[{host}]")
              } else {
                host.clone()
              };
              ("tcp", s(&format!("{host}:{port}")))
            }
            Probe::Http(url) => ("http", s(&url.url)),
            Probe::Cmd { argv } => ("cmd", strings(argv)),
            Probe::File { path } => ("file", s(&written_path(path))),
          };
          map.insert(s(key), value);
          if *interval != DEFAULT_INTERVAL {
            map.insert(s("interval"), s(&format_duration(*interval)));
          }
        }
      }
      if let Some(timeout) = ready.timeout {
        map.insert(s("timeout"), s(&format_duration(timeout)));
      }
      spec.set("ready", Some(Value::Mapping(map)));
    }
    if let Some(cwd) = &cfg.cwd {
      spec.set("cwd", Some(s(&written_path(Path::new(cwd)))));
    }
    if let Some(env) = &cfg.env {
      let map = env
        .iter()
        .map(|(k, v)| (s(k), v.as_deref().map_or(Value::Null, s)))
        .collect();
      spec.set("env", Some(Value::Mapping(map)));
    }
    if let Some(add_path) = &cfg.add_path {
      let items = add_path.iter().map(|path| s(&written_path(path))).collect();
      spec.set("add_path", Some(Value::Sequence(items)));
    }
    if let Some(autostart) = cfg.autostart {
      spec.set("autostart", Some(Value::Bool(autostart)));
    }
    if let Some(restart) = cfg.autorestart {
      spec.set("autorestart", Some(s(restart.name())));
    }
    if let Some(stop) = &cfg.stop {
      let mut map = Mapping::new();
      match &stop.signal {
        Some(StopSignal::Signal { sig, group }) => {
          map.insert(s("signal"), s(sig.name()));
          if !group {
            map.insert(s("group"), Value::Bool(false));
          }
        }
        Some(StopSignal::Keys(keys)) => {
          let keys = keys.iter().map(|key| s(&key.spec().to_string()));
          map.insert(s("keys"), Value::Sequence(keys.collect()));
        }
        Some(StopSignal::Cmd(argv)) => {
          map.insert(s("cmd"), strings(argv));
        }
        None => (),
      }
      if let Some(timeout) = stop.timeout {
        map.insert(s("timeout"), s(&format_duration(timeout)));
      }
      if !map.is_empty() {
        spec.set("stop", Some(Value::Mapping(map)));
      }
    }
    spec.set_log(cfg.log.as_ref());
    if let Some(n) = cfg.scrollback_len {
      spec.set("scrollback_len", Some(Value::from(n as u64)));
    }
    if let Some(n) = cfg.mouse_scroll_speed {
      spec.set("mouse_scroll_speed", Some(Value::from(n as u64)));
    }
    spec
  }

  /// Reads the entry as dekit.yaml would, at `path`. Relative paths in it
  /// resolve from `dir`.
  pub fn parse(&self, path: &str, dir: &Path) -> Result<TaskConfig> {
    let cx = CfgCx::new(dir.to_path_buf());
    let doc = CfgDoc::from_value(Value::Mapping(self.0.clone()));
    task_from_cfg(path.to_string(), &doc.root(), &cx)
  }

  /// The entry as `parse` reads it and `from_config` writes it again:
  /// absolute paths, a string `cmd` split, keys in order.
  pub fn normalized(&self, path: &str, dir: &Path) -> Result<TaskSpec> {
    Ok(TaskSpec::from_config(&self.parse(path, dir)?))
  }

  pub fn get(&self, key: &str) -> Option<&Value> {
    self.0.get(key)
  }

  /// Sets or, with `None`, removes a key, keeping the keys in order.
  pub fn set(&mut self, key: &str, value: Option<Value>) {
    let key = Value::String(key.to_string());
    let Some(value) = value else {
      self.0.retain(|k, _| *k != key);
      return;
    };
    let rank = |key: &Value| {
      key
        .as_str()
        .and_then(|key| TASK_KEYS.iter().position(|k| *k == key))
        .unwrap_or(usize::MAX)
    };
    // One there keeps its place, and one ranked last goes last.
    let in_order = self.0.contains_key(&key)
      || self
        .0
        .keys()
        .last()
        .is_none_or(|last| rank(last) <= rank(&key));
    self.0.insert(key, value);
    if !in_order {
      let mut entries: Vec<(Value, Value)> =
        std::mem::take(&mut self.0).into_iter().collect();
      entries.sort_by_key(|(key, _)| rank(key));
      self.0 = entries.into_iter().collect();
    }
  }

  pub fn label(&self) -> Option<&str> {
    self.get("label").and_then(Value::as_str)
  }

  pub fn set_label(&mut self, label: Option<&str>) {
    self.set("label", label.map(|label| Value::String(label.to_string())));
  }

  pub fn kind(&self) -> TaskKind {
    match self.get("type").and_then(Value::as_str) {
      Some("job") => TaskKind::Job,
      _ => TaskKind::Service,
    }
  }

  pub fn strings(&self, key: &str) -> Vec<String> {
    match self.get(key) {
      Some(Value::Sequence(items)) => items
        .iter()
        .filter_map(|item| item.as_str().map(String::from))
        .collect(),
      _ => Vec::new(),
    }
  }

  pub fn set_strings(&mut self, key: &str, items: Vec<String>) {
    let value = (!items.is_empty())
      .then(|| Value::Sequence(items.into_iter().map(Value::String).collect()));
    self.set(key, value);
  }

  pub fn deps(&self) -> Vec<String> {
    self.strings("deps")
  }

  pub fn set_deps(&mut self, mut deps: Vec<String>) {
    deps.sort();
    deps.dedup();
    self.set_strings("deps", deps);
  }

  pub fn set_log(&mut self, log: Option<&TaskLogConfig>) {
    let s = |text: &str| Value::String(text.to_string());
    let log = log.map(|log| {
      let mut map = Mapping::new();
      if let Some(enabled) = log.enabled {
        map.insert(s("enabled"), Value::Bool(enabled));
      }
      if let Some(dir) = &log.dir {
        map.insert(s("dir"), s(&written_path(dir)));
      }
      if let Some(file) = &log.file {
        map.insert(s("file"), s(&written_path(file)));
      }
      if let Some(mode) = log.mode {
        let mode = match mode {
          LogMode::Append => "append",
          LogMode::Truncate => "truncate",
        };
        map.insert(s("mode"), s(mode));
      }
      Value::Mapping(map)
    });
    self.set("log", log);
  }

  /// The definition for a copy of the task at `path`, its log as
  /// `TaskLogConfig::for_copy` makes it.
  pub fn for_copy(&self, path: &str, dir: &Path) -> TaskSpec {
    let mut spec = self.clone();
    if let Ok(TaskConfig { log: Some(log), .. }) = self.parse(path, dir) {
      spec.set_log(Some(&log.for_copy()));
    }
    spec
  }

  pub fn argv(&self) -> Option<Vec<String>> {
    match self.get("cmd")? {
      Value::Sequence(_) => Some(self.strings("cmd")),
      Value::String(line) => crate::parse_shell::split_argv(line).ok(),
      _ => None,
    }
  }

  /// The command as one line a shell would split back into the argv.
  pub fn cmd_line(&self) -> Option<String> {
    if let Some(script) = self.get("script").and_then(Value::as_str) {
      return Some(script.to_string());
    }
    Some(join_argv(&self.argv()?))
  }

  /// Each key that differs, with `env` compared variable by variable:
  /// `cmd`, `env.RUST_LOG`.
  pub fn changed_keys(&self, other: &TaskSpec) -> Vec<String> {
    let mut keys: Vec<String> = Vec::new();
    let mut seen = Vec::new();
    for key in self.0.keys().chain(other.0.keys()) {
      let Some(name) = key.as_str() else {
        continue;
      };
      if seen.contains(&name) {
        continue;
      }
      seen.push(name);
      let (a, b) = (self.0.get(key), other.0.get(key));
      if a == b {
        continue;
      }
      match (name, a, b) {
        ("env", a, b) => {
          let empty = Mapping::new();
          let a = a.and_then(Value::as_mapping).unwrap_or(&empty);
          let b = b.and_then(Value::as_mapping).unwrap_or(&empty);
          let mut vars: Vec<String> = Vec::new();
          for var in a.keys().chain(b.keys()) {
            let Some(var) = var.as_str() else { continue };
            if a.get(var) != b.get(var) && !vars.iter().any(|v| v == var) {
              vars.push(var.to_string());
            }
          }
          keys.extend(vars.into_iter().map(|var| format!("env.{var}")));
        }
        _ => keys.push(name.to_string()),
      }
    }
    keys.sort_by_key(|key| {
      let top = key.split('.').next().unwrap_or(key);
      TASK_KEYS
        .iter()
        .position(|k| *k == top)
        .unwrap_or(usize::MAX)
    });
    keys
  }

  /// The value at a key `changed_keys` names.
  pub fn value_at(&self, key: &str) -> Option<&Value> {
    match key.split_once('.') {
      Some(("env", var)) => self.get("env")?.as_mapping()?.get(var),
      _ => self.get(key),
    }
  }

  /// Sets the value at a key `changed_keys` names; `None` removes it.
  pub fn set_at(&mut self, key: &str, value: Option<Value>) {
    match key.split_once('.') {
      Some(("env", var)) => {
        let mut env = self
          .get("env")
          .and_then(Value::as_mapping)
          .cloned()
          .unwrap_or_default();
        match value {
          Some(value) => {
            env.insert(Value::String(var.to_string()), value);
          }
          None => {
            env.retain(|k, _| k.as_str() != Some(var));
          }
        }
        self.set("env", (!env.is_empty()).then_some(Value::Mapping(env)));
      }
      _ => self.set(key, value),
    }
  }
}

/// The config as the runner last took it in: each task entry with what
/// it said, in the config's order. A reload and a write-back compare the
/// config on disk and the tasks with it; a reload carried out and a
/// write-back read back are what change it.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Base(IndexMap<Source, TaskSpec>);

impl Base {
  /// What the entry said.
  pub fn get(&self, source: &Source) -> Option<&TaskSpec> {
    self.0.get(source)
  }

  pub fn contains(&self, source: &Source) -> bool {
    self.0.contains_key(source)
  }

  /// The entries, in order.
  pub fn sources(&self) -> impl Iterator<Item = &Source> {
    self.0.keys()
  }

  /// What the entry says now; a new one goes after the nearest entry
  /// before it in `order`, the config's.
  pub fn set(&mut self, source: Source, spec: TaskSpec, order: &[Source]) {
    if let Some(at) = self.0.get_mut(&source) {
      *at = spec;
      return;
    }
    let before = order
      .iter()
      .position(|s| *s == source)
      .map_or(&[][..], |i| &order[..i]);
    let at = before
      .iter()
      .rev()
      .find_map(|s| self.0.get_index_of(s))
      .map_or(0, |at| at + 1);
    let (index, _) = self.0.insert_full(source, spec);
    self.0.move_index(index, at);
  }

  pub fn remove(&mut self, source: &Source) {
    self.0.shift_remove(source);
  }

  /// The entries with what they said, in order.
  pub fn iter(&self) -> impl Iterator<Item = (&Source, &TaskSpec)> {
    self.0.iter()
  }
}

/// A `Base` entry as saved: where it is, and what it said.
#[derive(Deserialize, Serialize)]
struct BaseEntry {
  #[serde(flatten)]
  source: Source,
  spec: TaskSpec,
}

impl Serialize for Base {
  fn serialize<S: serde::Serializer>(&self, out: S) -> Result<S::Ok, S::Error> {
    let entries = self.0.iter().map(|(source, spec)| BaseEntry {
      source: source.clone(),
      spec: spec.clone(),
    });
    out.collect_seq(entries)
  }
}

impl<'de> Deserialize<'de> for Base {
  fn deserialize<D: serde::Deserializer<'de>>(
    input: D,
  ) -> Result<Self, D::Error> {
    let entries = Vec::<BaseEntry>::deserialize(input)?;
    Ok(entries.into_iter().map(|e| (e.source, e.spec)).collect())
  }
}

/// Entries in that order.
impl FromIterator<(Source, TaskSpec)> for Base {
  fn from_iter<I: IntoIterator<Item = (Source, TaskSpec)>>(entries: I) -> Self {
    Base(entries.into_iter().collect())
  }
}

/// A task's or folder's entry in the config: the file, relative to the
/// project root, and its key under `tasks:` there. `mount` is the path the file's
/// tasks are under (`load: {at: ...}`).
#[derive(
  Clone,
  Debug,
  Default,
  Deserialize,
  Eq,
  Hash,
  Ord,
  PartialEq,
  PartialOrd,
  Serialize,
)]
pub struct Source {
  pub file: String,
  pub key: String,
  #[serde(default, skip_serializing_if = "String::is_empty")]
  pub mount: String,
}

impl Source {
  /// The path of the task or folder the entry is at.
  pub fn path(&self) -> String {
    let key = self.key.strip_suffix('/').unwrap_or(&self.key);
    crate::kernel::task_path::join(&self.mount, key)
  }

  /// Whether it is a folder's entry: its key ends in `/`.
  pub fn is_folder(&self) -> bool {
    self.key.ends_with('/')
  }
}

/// A path as a config writes it: `/` between its parts on every platform,
/// so a file written on Windows reads the same elsewhere.
pub fn written_path(path: &Path) -> String {
  let path = path.to_string_lossy();
  if cfg!(windows) {
    path.replace('\\', "/")
  } else {
    path.into_owned()
  }
}

/// The largest unit that states it exactly: `90s`, `2m`, `250ms`.
pub fn format_duration(duration: Duration) -> String {
  let ms = duration.as_millis();
  if ms == 0 || !ms.is_multiple_of(1000) {
    return format!("{ms}ms");
  }
  let s = ms / 1000;
  if s.is_multiple_of(3600) {
    format!("{}h", s / 3600)
  } else if s.is_multiple_of(60) {
    format!("{}m", s / 60)
  } else {
    format!("{s}s")
  }
}

/// An argv as one line `split_argv` reads back, as a shell would: words
/// with spaces or specials quoted.
pub fn join_argv(argv: &[String]) -> String {
  argv
    .iter()
    .enumerate()
    .map(|(i, arg)| {
      let plain = !arg.is_empty()
        && !arg.starts_with('~')
        && arg.chars().all(|c| {
          c.is_alphanumeric() || "-_./:=,+@%^~".contains(c) || !c.is_ascii()
        })
        // The program can be a keyword or an assignment too.
        && (i > 0 || split_argv(arg).is_ok_and(|words| words == [arg.as_str()]));
      if plain {
        arg.clone()
      } else if !arg.contains('\'') {
        format!("'{arg}'")
      } else {
        let mut quoted = String::from('"');
        for c in arg.chars() {
          if matches!(c, '"' | '\\' | '$' | '`') {
            quoted.push('\\');
          }
          quoted.push(c);
        }
        quoted.push('"');
        quoted
      }
    })
    .collect::<Vec<_>>()
    .join(" ")
}

/// The value as a short line for a diff: a string as it is, anything
/// else as the writer writes it in flow style, `[sh, -c, "a b"]`.
pub fn show_value(value: Option<&Value>) -> String {
  match value {
    None => "—".to_string(),
    Some(Value::String(text)) => text.clone(),
    Some(value) => writer::flow(value).unwrap_or_default(),
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn a_copy_does_not_share_a_log_file() {
    let copy = |log: &str| {
      let spec: TaskSpec =
        serde_yaml::from_str(&format!("{{cmd: [x], log: {log}}}")).unwrap();
      spec
        .for_copy("api", Path::new("/p"))
        .get("log")
        .cloned()
        .unwrap()
    };
    let yaml = |text: &str| serde_yaml::from_str::<Value>(text).unwrap();
    assert_eq!(
      copy("{file: /p/api.log, mode: truncate}"),
      yaml("{mode: truncate, enabled: false}")
    );
    assert_eq!(
      copy("{dir: /p/logs, file: api.log}"),
      yaml("{enabled: true, dir: /p/logs}")
    );
    assert_eq!(
      copy("{file: '/p/{name}.log'}"),
      yaml("{enabled: true, file: '/p/{name}.log'}")
    );
  }

  #[test]
  fn removing_a_key_keeps_the_others_in_order() {
    // In the order a spec lists keys, so only the removal moves any.
    let mut spec: TaskSpec = serde_yaml::from_str(
      "{cmd: [a], deps: [x], tags: [t], cwd: /x, env: {A: '1', B: '2', C: '3'}}",
    )
    .unwrap();
    spec.set("cmd", None);
    let keys: Vec<&str> = spec.0.keys().filter_map(Value::as_str).collect();
    assert_eq!(keys, ["deps", "tags", "cwd", "env"]);
    spec.set_at("env.A", None);
    let env = spec.get("env").and_then(Value::as_mapping).unwrap();
    let vars: Vec<&str> = env.keys().filter_map(Value::as_str).collect();
    assert_eq!(vars, ["B", "C"]);
  }

  fn round_trip(yaml: &str) -> TaskSpec {
    let spec: TaskSpec = serde_yaml::from_str(yaml).unwrap();
    let cfg = spec.parse("web", Path::new("/proj")).unwrap();
    let written = TaskSpec::from_config(&cfg);
    let again =
      TaskSpec::from_config(&written.parse("web", Path::new("/x")).unwrap());
    assert_eq!(written, again, "{yaml}");
    written
  }

  #[test]
  fn reads_back_what_it_writes() {
    let spec = round_trip(
      "
label: Web
type: job
cmd: npm run 'my script'
deps: [db, cache]
tags: [front]
cwd: web
env: {A: '1', B: null}
add_path: [bin]
autostart: true
autorestart: on-failure
stop: {signal: SIGINT, group: false, timeout: 90s}
log: {dir: logs, mode: truncate}
scrollback_len: 5000
",
    );
    assert_eq!(spec.get("cwd").unwrap(), "/proj/web");
    assert_eq!(spec.deps(), ["cache", "db"]);
    assert_eq!(
      spec.argv().unwrap(),
      ["npm", "run", "my script"].map(String::from)
    );
    assert_eq!(spec.cmd_line().unwrap(), "npm run 'my script'");
    assert_eq!(spec.kind(), TaskKind::Job);

    for yaml in [
      "cmd: x\nready: {log: listening, timeout: 1m}",
      "cmd: x\nready: {tcp: 5432}",
      "cmd: x\nready: {tcp: 'db:5432', interval: 250ms}",
      "cmd: x\nready: {tcp: '[::1]:80'}",
      "cmd: x\nready: {http: 'http://localhost:3000/health'}",
      "cmd: x\nready: {cmd: [pg_isready]}",
      "cmd: x\nready: {file: /tmp/ok}",
      #[cfg(unix)]
      "cmd: x\nready: {sd_notify: true, timeout: 30s}",
      "cmd: x\nstop: SIGHUP",
      "cmd: x\nstop: {keys: ['<C-c>', '<Enter>']}",
      "cmd: x\nstop: {cmd: [podman, compose, down]}",
      "cmd: x\nlog: false",
      "cmd: x\nlog: true",
      "script: /proj/a.js",
    ] {
      round_trip(yaml);
    }
  }

  #[test]
  fn keys_stay_in_order_and_diffs_name_env_vars() {
    let mut a: TaskSpec = serde_yaml::from_str("cmd: [x]").unwrap();
    a.set(
      "env",
      Some(serde_yaml::from_str("{A: '1', B: '2'}").unwrap()),
    );
    a.set_label(Some("X"));
    let keys: Vec<&str> = a.0.keys().filter_map(Value::as_str).collect();
    assert_eq!(keys, ["label", "cmd", "env"]);

    let mut b = a.clone();
    b.set_at("env.B", Some(Value::from("3")));
    b.set_at("env.C", Some(Value::from("4")));
    b.set("cmd", Some(serde_yaml::from_str("[y]").unwrap()));
    assert_eq!(a.changed_keys(&b), ["cmd", "env.B", "env.C"]);
    b.set_at("env.A", None);
    b.set_at("env.B", None);
    b.set_at("env.C", None);
    assert_eq!(b.get("env"), None);
  }

  #[test]
  fn shows_values_as_flow_yaml() {
    let value: Value =
      serde_yaml::from_str("[sleep, '60', a b, 'yes']").unwrap();
    assert_eq!(show_value(Some(&value)), r#"[sleep, "60", "a b", "yes"]"#);
    assert_eq!(show_value(None), "—");
  }

  #[test]
  fn durations_and_argv_lines() {
    assert_eq!(format_duration(Duration::from_millis(250)), "250ms");
    assert_eq!(format_duration(Duration::from_secs(90)), "90s");
    assert_eq!(format_duration(Duration::from_secs(120)), "2m");
    assert_eq!(format_duration(Duration::from_secs(7200)), "2h");
    for argv in [
      &["a", "b c", "it's", "", "$HOME"][..],
      &["sh", "-c", "echo 'hi' $HOME `id`"],
      &["ls", "~/x", "a~b"],
      &["FOO=1", "x"],
      &["if", "then"],
    ] {
      let argv: Vec<String> = argv.iter().map(|arg| arg.to_string()).collect();
      let line = join_argv(&argv);
      assert_eq!(split_argv(&line).unwrap(), argv, "{line}");
    }
    let words = |line: &str| split_argv(line).unwrap();
    assert_eq!(join_argv(&words("make CC=gcc a~b")), "make CC=gcc a~b");
    assert_eq!(join_argv(&words("'if' then")), "'if' then");
  }
}
