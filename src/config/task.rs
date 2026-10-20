use std::{
  ffi::OsString,
  path::{Path, PathBuf},
};

use anyhow::{Result, bail};
use indexmap::IndexMap;
use serde_yaml::Value;

use crate::cfg::{CfgCx, CfgNode, CfgObj, FromCfg};
use crate::config::stop_signal::StopConfig;
use crate::config::task_log::TaskLogConfig;
use crate::kernel::task::{RestartMode, TaskKind};
use crate::parse_shell::split_argv;
use crate::process::process_spec::ProcessSpec;
use crate::task::ready::ReadyConfig;

const DEFAULT_SCROLLBACK_LEN: usize = 1000;
const DEFAULT_MOUSE_SCROLL_SPEED: usize = 5;

/// Keys allowed under `defaults:` (shared process settings, no cmd/deps).
pub(crate) const TASK_SETTING_KEYS: &[&str] = &[
  "cwd",
  "env",
  "add_path",
  "autostart",
  "autorestart",
  "stop",
  "log",
  "scrollback_len",
  "mouse_scroll_speed",
];

/// Keys allowed on a full task entry (settings + cmd form + graph fields).
pub(crate) const TASK_KEYS: &[&str] = &[
  "label",
  "type",
  "cmd",
  "script",
  "deps",
  "tags",
  "ready",
  "cwd",
  "env",
  "add_path",
  "autostart",
  "autorestart",
  "stop",
  "log",
  "scrollback_len",
  "mouse_scroll_speed",
];

/// Tag for tasks started on `dekit up` / at launch.
pub const AUTOSTART_TAG: &str = "autostart";

pub fn is_script(path: &Path) -> bool {
  match path.extension().and_then(|ext| ext.to_str()) {
    Some("js" | "mjs") => true,
    _ => false,
  }
}
/// Tag for tasks added at runtime rather than from config.
pub const DYNAMIC_TAG: &str = "dynamic";

#[derive(Clone, Default)]
pub struct TaskConfig {
  pub path: String,
  /// Display name; defaults to the path.
  pub label: Option<String>,
  pub cmd: Option<CmdConfig>,
  pub deps: Vec<String>,
  pub tags: Vec<String>,
  /// `type`: a job is done once it exits 0.
  pub kind: TaskKind,
  pub ready: Option<ReadyConfig>,

  pub cwd: Option<OsString>,
  pub env: Option<IndexMap<String, Option<String>>>,
  pub add_path: Option<Vec<PathBuf>>,
  pub autostart: Option<bool>,
  pub autorestart: Option<RestartMode>,
  pub stop: Option<StopConfig>,
  pub log: Option<TaskLogConfig>,
  pub scrollback_len: Option<usize>,
  pub mouse_scroll_speed: Option<usize>,
  /// Where in the config the task is written; none for a task added at
  /// runtime.
  pub source: Option<crate::spec::Source>,
}

impl TaskConfig {
  pub fn overlay(self, over: TaskConfig) -> TaskConfig {
    TaskConfig {
      path: if over.path.is_empty() {
        self.path
      } else {
        over.path
      },
      label: over.label.or(self.label),
      cmd: over.cmd.or(self.cmd),
      deps: if over.deps.is_empty() {
        self.deps
      } else {
        over.deps
      },
      tags: if over.tags.is_empty() {
        self.tags
      } else {
        over.tags
      },
      // `defaults` can't set `type`.
      kind: over.kind,
      ready: over.ready.or(self.ready),
      cwd: over.cwd.or(self.cwd),
      env: over.env.or(self.env),
      add_path: over.add_path.or(self.add_path),
      autostart: over.autostart.or(self.autostart),
      autorestart: over.autorestart.or(self.autorestart),
      stop: match (over.stop, self.stop) {
        (Some(over), Some(base)) => Some(base.merged(over)),
        (over, base) => over.or(base),
      },
      log: match (over.log, self.log) {
        (Some(over), Some(base)) => Some(base.merged(&over)),
        (over, base) => over.or(base),
      },
      scrollback_len: over.scrollback_len.or(self.scrollback_len),
      mouse_scroll_speed: over.mouse_scroll_speed.or(self.mouse_scroll_speed),
      source: over.source.or(self.source),
    }
  }

  pub fn autostart(&self) -> bool {
    self.autostart.unwrap_or(false)
  }
  pub fn scrollback_len(&self) -> usize {
    self.scrollback_len.unwrap_or(DEFAULT_SCROLLBACK_LEN)
  }
  pub fn mouse_scroll_speed(&self) -> usize {
    self
      .mouse_scroll_speed
      .unwrap_or(DEFAULT_MOUSE_SCROLL_SPEED)
  }
}

pub(crate) fn parse_task_settings(
  obj: &CfgObj<'_>,
  cx: &CfgCx,
) -> Result<TaskConfig> {
  // Callers that allow extra keys (full task entries) must validate first.
  // `defaults:` uses this directly and only permits setting keys.
  refuse_removed(obj, REFUSED)?;
  refuse_removed(
    obj,
    &[
      ("ready", "a ready check belongs to a task, not 'defaults'"),
      (
        "ready_log",
        "'ready_log' is now 'ready: {log: ...}', and a ready check belongs \
         to a task, not 'defaults'",
      ),
    ],
  )?;
  obj.known_keys(TASK_SETTING_KEYS)?;
  parse_task_settings_unchecked(obj, cx)
}

/// `shell` keys, anywhere: dekit.yaml runs no shell.
pub(crate) const NO_SHELL: (&str, &str) = (
  "shell",
  "'shell' is not supported: 'cmd' runs a program without a shell; to use \
   a shell, run it in 'cmd', as in [\"bash\", \"-c\", \"...\"] or, on \
   Windows, [\"cmd\", \"/c\", \"...\"]",
);

/// Keys refused in a task and in `defaults`. `health` is kept for checks
/// on a running task, `restart` for how to restart one.
const REFUSED: &[(&str, &str)] = &[
  NO_SHELL,
  (
    "health",
    "'health' is not supported yet; 'ready' checks a starting task",
  ),
  (
    "restart",
    "'restart' would set how to restart a task and is not supported; to \
     restart after a failure, use 'autorestart: on-failure'",
  ),
];

/// Keys that were removed or are not supported, with what to write
/// instead.
pub(crate) fn refuse_removed(
  obj: &CfgObj<'_>,
  removed: &[(&str, &str)],
) -> Result<()> {
  for (key, message) in removed {
    if let Some(node) = obj.get(key) {
      bail!(node.error(message));
    }
  }
  Ok(())
}

fn parse_task_settings_unchecked(
  obj: &CfgObj<'_>,
  cx: &CfgCx,
) -> Result<TaskConfig> {
  let mut p = TaskConfig::default();
  if let Some(cwd) = obj.get("cwd") {
    p.cwd = Some(cx.resolve_path(cwd.as_str()?).into_os_string());
  }
  p.env = obj.optional("env", cx)?;
  p.add_path = obj.optional("add_path", cx)?;
  p.autostart = obj.optional("autostart", cx)?;
  p.autorestart = obj.optional("autorestart", cx)?;
  p.stop = obj.optional("stop", cx)?;
  p.log = obj.optional("log", cx)?;
  p.scrollback_len = obj.optional("scrollback_len", cx)?;
  p.mouse_scroll_speed = obj.optional("mouse_scroll_speed", cx)?;
  Ok(p)
}

pub(crate) fn task_from_cfg(
  path: String,
  node: &CfgNode<'_>,
  cx: &CfgCx,
) -> Result<TaskConfig> {
  let obj = node.as_obj()?;
  refuse_removed(&obj, REFUSED)?;
  refuse_removed(
    &obj,
    &[("ready_log", "'ready_log' is now 'ready: {log: ...}'")],
  )?;
  obj.known_keys(TASK_KEYS)?;
  let mut p = parse_task_settings_unchecked(&obj, cx)?;
  if let Err(err) = crate::kernel::task_path::TaskPath::new(path.as_str()) {
    bail!("task '{}': {}", path, err);
  }
  p.path = path;
  p.label = obj.optional("label", cx)?;
  p.cmd = Some(cmd_from_cfg(node, cx)?);
  p.deps = obj.default("deps", Vec::new(), cx)?;
  p.tags = obj.default("tags", Vec::new(), cx)?;
  if let Some(tags) = obj.get("tags")
    && p.tags.iter().any(|tag| tag == DYNAMIC_TAG)
  {
    bail!(tags.error(
      "'dynamic' is the runner's tag: every task not from the config has it"
    ));
  }
  p.ready = obj.optional("ready", cx)?;
  if let Some(kind) = obj.get("type") {
    p.kind = match kind.as_str()? {
      "service" => TaskKind::Service,
      "job" => TaskKind::Job,
      other => bail!(kind.error(format!(
        "unknown type '{other}'; expected 'service' or 'job'"
      ))),
    };
  }
  if let Some(ready) = obj.get("ready")
    && !p.kind.takes_ready()
  {
    bail!(
      ready
        .error("a job takes no 'ready': its dependents wait until it exits 0")
    );
  }
  if let (Some(restart), Some(autorestart)) =
    (p.autorestart, obj.get("autorestart"))
    && !p.kind.takes_restart(restart)
  {
    bail!(autorestart.error("a job can't use 'autorestart: always'"));
  }
  Ok(p)
}

impl FromCfg for RestartMode {
  fn from_cfg(node: &CfgNode<'_>, _cx: &CfgCx) -> Result<Self> {
    if node.is_mapping() {
      bail!(
        node.error("the object form of 'autorestart' is not supported yet")
      );
    }
    match node.raw().as_str().and_then(RestartMode::from_name) {
      Some(restart) => Ok(restart),
      None => bail!(node.error("expected never, on-failure, or always")),
    }
  }
}

pub(crate) fn argv_from_cfg(node: &CfgNode<'_>) -> Result<Vec<String>> {
  argv_from_value(node.raw()).map_err(|err| node.error(err))
}

/// A `cmd` as written: an array is the argv as it is, a string is split
/// into one (`split_argv`).
fn argv_from_value(value: &Value) -> Result<Vec<String>, String> {
  let found = |value: &Value| match value {
    Value::Null => "null",
    Value::Bool(_) => "a boolean",
    Value::Number(_) => "a number",
    Value::String(_) => "a string",
    Value::Sequence(_) => "an array",
    Value::Mapping(_) => "an object",
    Value::Tagged(_) => "a tagged value",
  };
  match value {
    Value::String(line) => split_argv(line).map_err(|err| err.to_string()),
    Value::Sequence(items) if items.is_empty() => Err("cmd is empty".into()),
    Value::Sequence(items) => items
      .iter()
      .enumerate()
      .map(|(i, item)| match item {
        Value::String(arg) => Ok(arg.clone()),
        Value::Bool(_) | Value::Number(_) => Err(format!(
          "cmd[{i}] is {}, not a string; put it in quotes",
          found(item)
        )),
        other => Err(format!("cmd[{i}] is {}, not a string", found(other))),
      })
      .collect(),
    other => Err(format!(
      "expected a string or an array of strings, found {}",
      found(other)
    )),
  }
}

fn cmd_from_cfg(node: &CfgNode<'_>, cx: &CfgCx) -> Result<CmdConfig> {
  let obj = node.as_obj()?;
  match (obj.get("cmd"), obj.get("script")) {
    (Some(cmd), None) => Ok(CmdConfig::Cmd {
      cmd: argv_from_cfg(&cmd)?,
    }),
    (None, Some(script)) => {
      let path = cx.resolve_path(script.as_str()?);
      if !is_script(&path) {
        bail!(script.error("script must be a .js or .mjs file"));
      }
      // One that is missing fails when the task starts, as a missing
      // program does: a definition reads the same whatever is on disk.
      Ok(CmdConfig::Script { script: path })
    }
    (None, None) => bail!(obj.error("task must define 'cmd' or 'script'")),
    (Some(_), Some(_)) => {
      bail!(obj.error("task must define only one of 'cmd' or 'script'"))
    }
  }
}

/// What a task runs. `cmd` is always an argv; a string where one is
/// read is split into words (`split_argv`).
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub enum CmdConfig {
  Cmd { cmd: Vec<String> },
  Script { script: PathBuf },
}

/// The spec a task runs as. `runner` is the identity script tasks
/// inherit; registration paths validate it exists before a script task
/// can reach here.
pub fn process_spec(
  cfg: &TaskConfig,
  runner: Option<&crate::runner::RunnerSpec>,
) -> ProcessSpec {
  let mut cmd = match &cfg.cmd {
    Some(CmdConfig::Cmd { cmd }) => ProcessSpec::from_argv(cmd.clone()),
    Some(CmdConfig::Script { script }) => {
      let runner = runner.expect("script tasks require a runner identity");
      // Runner identity travels in env, not argv, so the script sees
      // the same `[exe, script]` argv as `dekit script.js` on the CLI.
      let mut spec = ProcessSpec::from_argv(vec![
        std::env::current_exe()
          .expect("current executable is available")
          .to_string_lossy()
          .into_owned(),
        script.to_string_lossy().into_owned(),
      ]);
      spec.env(
        crate::runner::ENV_RUNNER_ROOT,
        runner.root.to_str().expect("validated runner root"),
      );
      spec.env(crate::runner::ENV_RUNNER_KIND, runner.kind.as_str());
      spec
    }
    None => ProcessSpec::from_argv(Vec::new()),
  };

  if let Some(env) = &cfg.env {
    for (k, v) in env {
      if let Some(v) = v {
        cmd.env(k, v);
      } else {
        cmd.env_remove(k);
      }
    }
  }

  if let Some(add_path) = cfg.add_path.as_ref().filter(|p| !p.is_empty()) {
    // Base PATH is the task's own `env` override if it sets one, otherwise
    // the ambient PATH resolved at spawn time.
    let base = cfg
      .env
      .as_ref()
      .and_then(|env| env.get("PATH").cloned().flatten())
      .or_else(|| std::env::var("PATH").ok());
    let mut paths: Vec<PathBuf> = add_path.clone();
    if let Some(base) = base {
      paths.extend(std::env::split_paths(&base));
    }
    if let Ok(joined) = std::env::join_paths(&paths) {
      cmd.env("PATH", joined.to_string_lossy().into_owned());
    }
  }

  if let Some(cwd) = &cfg.cwd {
    cmd.cwd(cwd.to_string_lossy());
  } else if let Ok(cwd) = std::env::current_dir() {
    cmd.cwd(cwd.to_string_lossy());
  }

  cmd
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::cfg::{CfgCx, CfgDoc};
  use std::path::PathBuf;

  #[test]
  fn task_rejects_unknown_keys_with_suggestion() {
    let yaml = r#"
cmd: ["echo", "hi"]
auto_restart: always
"#;
    let value: serde_yaml::Value = serde_yaml::from_str(yaml).unwrap();
    let cx = CfgCx::new(PathBuf::from("."));
    let doc = CfgDoc::from_value(value);
    let err = match task_from_cfg("web".into(), &doc.root(), &cx) {
      Ok(_) => panic!("expected unknown key error"),
      Err(e) => e.to_string(),
    };
    assert!(err.contains("unknown field 'auto_restart'"), "err={err}");
    assert!(err.contains("did you mean 'autorestart'?"), "err={err}");
  }

  #[test]
  fn defaults_reject_cmd_key() {
    let yaml = r#"
cmd: ["echo", "nope"]
cwd: /tmp
"#;
    let value: serde_yaml::Value = serde_yaml::from_str(yaml).unwrap();
    let cx = CfgCx::new(PathBuf::from("."));
    let doc = CfgDoc::from_value(value);
    let err = match parse_task_settings(&doc.root().as_obj().unwrap(), &cx) {
      Ok(_) => panic!("expected unknown key error"),
      Err(e) => e.to_string(),
    };
    assert!(err.contains("unknown field 'cmd'"), "err={err}");
  }

  #[test]
  fn defaults_take_no_ready_check() {
    for (yaml, expected) in [
      (
        "ready: {log: a}",
        "a ready check belongs to a task, not 'defaults' at <config>.ready",
      ),
      (
        "ready_log: a",
        "'ready_log' is now 'ready: {log: ...}', and a ready check belongs \
         to a task",
      ),
      ("restart: on-failure", "use 'autorestart: on-failure'"),
      ("shell: x", "'shell' is not supported"),
    ] {
      let value: serde_yaml::Value = serde_yaml::from_str(yaml).unwrap();
      let cx = CfgCx::new(PathBuf::from("."));
      let doc = CfgDoc::from_value(value);
      match parse_task_settings(&doc.root().as_obj().unwrap(), &cx) {
        Ok(_) => panic!("{yaml}: accepted"),
        Err(err) => {
          let err = err.to_string();
          assert!(err.contains(expected), "{yaml}: {err}");
        }
      }
    }
  }

  fn parse(yaml: &str) -> Result<TaskConfig> {
    let value: serde_yaml::Value = serde_yaml::from_str(yaml).unwrap();
    let cx = CfgCx::new(PathBuf::from("."));
    let doc = CfgDoc::from_value(value);
    task_from_cfg("web".into(), &doc.root(), &cx)
  }

  /// The runner gives `dynamic` to its own tasks; written in the config
  /// it would be stripped, so it is refused instead.
  #[test]
  fn the_dynamic_tag_is_refused() {
    let err = match parse("cmd: x\ntags: [web, dynamic]") {
      Ok(_) => panic!("accepted"),
      Err(err) => err.to_string(),
    };
    assert!(err.contains("'dynamic' is the runner's tag"), "{err}");
    assert_eq!(parse("cmd: x\ntags: [web]").unwrap().tags, ["web"]);
  }

  fn parse_err(yaml: &str) -> String {
    match parse(yaml) {
      Ok(_) => panic!("expected an error for {yaml}"),
      Err(err) => err.to_string(),
    }
  }

  #[test]
  fn ready_takes_one_check() {
    use crate::task::ready::{Probe, ReadyCheck};
    use std::time::Duration;

    let task = parse("cmd: x\nready: {log: listening, timeout: 1m}").unwrap();
    let ready = task.ready.unwrap();
    match ready.check {
      ReadyCheck::Log(text) => assert_eq!(text, "listening"),
      other => panic!("{other:?}"),
    }
    assert_eq!(ready.timeout, Some(Duration::from_secs(60)));

    let task = parse("cmd: x\nready: {tcp: 'db:5432'}").unwrap();
    match task.ready.unwrap().check {
      ReadyCheck::Probe {
        probe: Probe::Tcp { host, port },
        interval,
      } => {
        assert_eq!((host.as_deref(), port), (Some("db"), 5432));
        assert_eq!(interval, Duration::from_secs(1));
      }
      other => panic!("{other:?}"),
    }
    let task =
      parse("cmd: x\nready: {cmd: 'pg_isready -q', interval: 250ms}").unwrap();
    match task.ready.unwrap().check {
      ReadyCheck::Probe {
        probe: Probe::Cmd { argv },
        interval,
      } => {
        assert_eq!(argv, ["pg_isready", "-q"]);
        assert_eq!(interval, Duration::from_millis(250));
      }
      other => panic!("{other:?}"),
    }

    #[cfg(unix)]
    {
      let task =
        parse("cmd: x\nready: {sd_notify: true, timeout: 5s}").unwrap();
      let ready = task.ready.unwrap();
      assert!(
        matches!(ready.check, ReadyCheck::SdNotify),
        "{:?}",
        ready.check
      );
      assert_eq!(ready.timeout, Some(Duration::from_secs(5)));
    }

    for (yaml, expected) in [
      ("ready: listening", "expected an object"),
      ("ready: {}", "ready needs a check"),
      ("ready: {sd_notify: false}", "expected true"),
      ("ready: {sd_notify: 1}", "expected true"),
      (
        "ready: {sd_notify: true, interval: 1s}",
        "'interval' does not apply",
      ),
      ("ready: {log: a, tcp: 1}", "ready takes one check"),
      ("ready: {log: a, interval: 1s}", "'interval' does not apply"),
      ("ready: {http: 'https://x'}", "https is not supported"),
      ("ready: {tcp: 'db'}", "expected a port or host:port"),
      ("ready: {file: x, timeout: 10}", "expected a duration"),
      ("ready: {file: x, timeout: 0s}", "expected a duration"),
      ("ready: {log: a, retries: 3}", "unknown field 'retries'"),
      ("ready: {shell: 'curl x'}", "'shell' is not supported"),
    ] {
      let err = parse_err(&format!("cmd: x\n{yaml}"));
      assert!(err.contains(expected), "{yaml}: {err}");
    }
  }

  #[test]
  fn type_autorestart_and_reserved_keys() {
    let task = parse("cmd: x\ntype: job\nautorestart: on-failure").unwrap();
    assert_eq!(task.kind, TaskKind::Job);
    assert_eq!(task.autorestart, Some(RestartMode::OnFailure));
    assert_eq!(
      parse("cmd: x\ntype: service").unwrap().kind,
      TaskKind::Service
    );

    for (yaml, expected) in [
      ("type: job\nready: {log: a}", "a job takes no 'ready'"),
      (
        "type: job\nautorestart: always",
        "a job can't use 'autorestart: always'",
      ),
      ("type: batch", "unknown type 'batch'"),
      (
        "autorestart: sometimes",
        "expected never, on-failure, or always",
      ),
      ("autorestart: true", "expected never, on-failure, or always"),
      ("autorestart: {when: always}", "not supported yet"),
      (
        "restart: on-failure",
        "'restart' would set how to restart a task and is not supported; \
         to restart after a failure, use 'autorestart: on-failure'",
      ),
      (
        "health: {http: 'http://x'}",
        "'health' is not supported yet",
      ),
      ("ready_log: x", "'ready_log' is now 'ready: {log: ...}'"),
    ] {
      let err = parse_err(&format!("cmd: x\n{yaml}"));
      assert!(err.contains(expected), "{yaml}: {err}");
    }
  }

  #[test]
  fn cmd_is_an_argv_or_a_string() {
    let argv = |yaml: &str| match parse(yaml).unwrap().cmd {
      Some(CmdConfig::Cmd { cmd }) => cmd,
      other => panic!("{yaml}: {other:?}"),
    };
    assert_eq!(
      argv("cmd: npm run 'my script'"),
      ["npm", "run", "my script"]
    );
    assert_eq!(
      argv("cmd: [npm, run, '*.js', 'a b']"),
      ["npm", "run", "*.js", "a b"]
    );
    // Windows paths, as the docs write them.
    assert_eq!(
      argv(r"cmd: server.exe --data 'C:\data'"),
      ["server.exe", "--data", r"C:\data"]
    );
    assert_eq!(
      argv(r"cmd: ['C:\tools\server.exe', --port, '80']"),
      [r"C:\tools\server.exe", "--port", "80"]
    );
    // An npm shim on Windows, as the docs write it.
    assert_eq!(
      argv(r#"cmd: ["cmd", "/c", "npm run dev"]"#),
      ["cmd", "/c", "npm run dev"]
    );
    for (yaml, expected) in [
      ("cmd: ''", "cmd string is empty at <config>.cmd"),
      ("cmd: []", "cmd is empty at <config>.cmd"),
      ("cmd: npm run *.js", "`*` needs quotes"),
      (
        "shell: npm start",
        "'shell' is not supported: 'cmd' runs a program",
      ),
      ("label: x", "task must define 'cmd' or 'script'"),
      (
        "cmd: [npm, run, 3000]",
        "cmd[2] is a number, not a string; put it in quotes at <config>.cmd",
      ),
      ("cmd: [npm, true]", "cmd[1] is a boolean"),
      ("cmd: [npm, null]", "cmd[1] is null, not a string at"),
      (
        "cmd: [npm, [run, dev]]",
        "cmd[1] is an array, not a string at",
      ),
      (
        "cmd: {npm: run}",
        "expected a string or an array of strings, found an object",
      ),
    ] {
      let err = parse_err(yaml);
      assert!(err.contains(expected), "{yaml}: {err}");
    }
  }

  #[test]
  fn stop_merges_key_by_key_over_defaults() {
    use crate::config::stop_signal::{Sig, StopSignal};
    use std::time::Duration;

    let settings = |yaml: &str| {
      let value: serde_yaml::Value = serde_yaml::from_str(yaml).unwrap();
      let cx = CfgCx::new(PathBuf::from("."));
      let doc = CfgDoc::from_value(value);
      parse_task_settings(&doc.root().as_obj().unwrap(), &cx).unwrap()
    };
    for (defaults, task) in [
      ("stop: SIGINT", "stop: {timeout: 60s}"),
      ("stop: {timeout: 60s}", "stop: SIGINT"),
      (
        "stop: {signal: SIGTERM, timeout: 5s}",
        "stop: {signal: SIGINT, timeout: 60s}",
      ),
    ] {
      let stop = settings(defaults).overlay(settings(task)).stop.unwrap();
      match stop.signal {
        Some(StopSignal::Signal {
          sig: Sig::Int,
          group: true,
        }) => (),
        other => panic!("{defaults} + {task}: {other:?}"),
      }
      assert_eq!(
        stop.timeout,
        Some(Duration::from_secs(60)),
        "{defaults} + {task}"
      );
    }
  }

  #[test]
  fn add_path_takes_priority_over_base_path() {
    let mut env = IndexMap::new();
    env.insert("PATH".to_string(), Some("/base/bin".to_string()));

    let cfg = TaskConfig {
      cmd: Some(CmdConfig::Cmd {
        cmd: vec!["true".to_string()],
      }),
      env: Some(env),
      add_path: Some(vec![PathBuf::from("/custom/bin")]),
      ..Default::default()
    };

    let spec = process_spec(&cfg, None);
    let path = spec.env.get("PATH").cloned().flatten().unwrap();

    let expected = std::env::join_paths([
      PathBuf::from("/custom/bin"),
      PathBuf::from("/base/bin"),
    ])
    .unwrap()
    .to_string_lossy()
    .into_owned();

    assert_eq!(path, expected);
  }
}
