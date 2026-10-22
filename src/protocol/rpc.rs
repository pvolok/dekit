use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::command::Command;
use crate::kernel::task::TaskId;
use crate::protocol::caps::TermCaps;
use crate::protocol::ctl::{GeometryParams, RpcError, codes};
use crate::target::Target;

/// Mutations travel as one `command` method whose params are a `Command`
/// verbatim; queries and screen attachment are methods of their own.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(tag = "method", content = "params", rename_all = "snake_case")]
pub enum RpcRequest {
  Command(Command),
  Ls {
    /// Defaults to every task in the default space (`**`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    target: Option<Target>,
  },
  /// Explain why one task is (not) running.
  Why {
    target: Target,
  },
  /// The current screen of one task, rendered as ANSI text.
  Screen {
    target: Target,
  },
  /// Attach to one task's screen for the rest of the connection.
  Attach {
    target: Target,
    /// The client's size in cells and, with `atch2`, the size of a cell
    /// in pixels when it measured one; later values come with `resize`.
    #[serde(flatten)]
    geometry: GeometryParams,
    /// End the session (`task_exited` bye) when the task's execution
    /// finishes; the foreground `run` verb sets this.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    until_exit: bool,
    /// What the client's terminal can show, as the client measured it.
    /// Absent from an older client: the painter assumes a plain
    /// true-color terminal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    caps: Option<TermCaps>,
  },
  /// Merge the config on disk into the tasks: with `apply`, carry out
  /// the default choices; either way, answer with the plan.
  Reload {
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    apply: bool,
  },
  /// Wait until tasks meet a condition, for the rest of the connection:
  /// `target` observes the tasks it matches; `command` carries out a
  /// verb, then waits on what it acted on, for what the verb implies.
  /// One of the two is given; `for` only with `target`.
  Wait {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    target: Option<Target>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    command: Option<Command>,
    #[serde(default, rename = "for", skip_serializing_if = "Option::is_none")]
    wait_for: Option<WaitFor>,
    /// None waits for as long as it takes; 0 checks once.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    timeout_ms: Option<u64>,
  },
  /// Replace the runner live with the given binary.
  /// The reply comes from the new binary after activation, as
  /// `{version}`; `unsupported` where live replacement is not available,
  /// `busy` while one is in progress.
  Upgrade {
    binary: String,
  },
}

/// Gate for `from_wire`: methods not listed here are `unknown_method`
/// instead of `invalid_params`. Kept in sync with the enum by tests.
const METHODS: &[&str] = &[
  "command", "ls", "why", "screen", "attach", "reload", "upgrade", "wait",
];

impl RpcRequest {
  pub fn to_wire(&self) -> (String, Value) {
    let value = serde_json::to_value(self).expect("serialize request");
    let Value::Object(mut map) = value else {
      unreachable!("requests serialize to objects")
    };
    let Some(Value::String(method)) = map.remove("method") else {
      unreachable!("requests carry a method tag")
    };
    let params = match map.remove("params") {
      Some(Value::Object(fields)) if fields.is_empty() => Value::Null,
      Some(params) => params,
      None => Value::Null,
    };
    (method, params)
  }

  pub fn from_wire(
    method: &str,
    params: Value,
  ) -> Result<RpcRequest, RpcError> {
    if !METHODS.contains(&method) {
      return Err(RpcError::new(
        codes::UNKNOWN_METHOD,
        format!("unknown method '{method}'"),
      ));
    }
    let params = match params {
      Value::Null => Value::Object(serde_json::Map::new()),
      params => params,
    };
    let mut wire = serde_json::Map::new();
    wire.insert("method".to_string(), method.into());
    wire.insert("params".to_string(), params);
    serde_json::from_value(Value::Object(wire))
      .map_err(|err| RpcError::new(codes::INVALID_PARAMS, err.to_string()))
  }
}

pub fn ok_result() -> Value {
  json!({})
}

/// Result of a task-directed command: how many tasks it acted on. Zero
/// is a normal outcome (the target matched nothing), not an error, so the
/// client can decide how to report it.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ActResult {
  pub matched: usize,
}

/// Result of `quit`: the runner exits within this, having stopped each
/// task (grace, then hard kill) and saved them if asked.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct QuitResult {
  pub stop_within_ms: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct TaskListResult {
  pub tasks: Vec<RpcTaskInfo>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ScreenResult {
  pub screen: String,
}

/// A task's lifecycle state on the wire: a stable token, plus the exit
/// detail for `done`/`exited`/`backoff`. `state` is one of `idle`,
/// `starting`, `running`, `ready`, `stopping`, `backoff`, `done`,
/// `exited`. `reason` says why dekit ended it: `ready_timeout` (not ready
/// in time), or on an idle task `kill_timeout` (its stop was given up
/// on: the process outlived a hard kill).
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct RpcState {
  pub state: String,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub exit_code: Option<i32>,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub signal: Option<i32>,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub reason: Option<String>,
}

/// `path` is the space-qualified target of the task (`@dekit/console`),
/// or `<task:id>` for a task without one.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct RpcTaskInfo {
  pub id: TaskId,
  pub path: String,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub label: Option<String>,
  /// `service` or `job`; absent from an older runner.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub kind: Option<String>,
  #[serde(flatten)]
  pub state: RpcState,
}

/// What a `wait` on a target waits for; a command's verb implies its own.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WaitFor {
  /// Ready, or done for a job.
  Ready,
  /// No process, and won't start again by itself; the last run is
  /// judged.
  Exit,
}

/// Feature `wait`: the server takes the `wait` method.
pub const FEATURE_WAIT: &str = "wait";

/// Events of a wait session, server to client. `task-changed` and
/// `task-removed` carry one task each.
pub const EVENT_TASKS: &str = "tasks";
pub const EVENT_TASK_CHANGED: &str = "task-changed";
pub const EVENT_TASK_REMOVED: &str = "task-removed";
pub const EVENT_WAIT_DONE: &str = "wait-done";

/// How a wait ended.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WaitOutcome {
  Met,
  Failed,
  /// `timeout_ms` ran out with tasks still pending.
  Timeout,
}

/// Where a task stands in a wait.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WaitMark {
  Pending,
  Met,
  Failed,
}

impl From<crate::kernel::kernel_message::WaitMark> for WaitMark {
  fn from(mark: crate::kernel::kernel_message::WaitMark) -> Self {
    use crate::kernel::kernel_message::WaitMark as Mark;
    match mark {
      Mark::Pending => WaitMark::Pending,
      Mark::Met => WaitMark::Met,
      Mark::Failed => WaitMark::Failed,
    }
  }
}

/// A task of a wait's set: its task-info, where it stands, and for a
/// failed one why.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct RpcWaitTask {
  #[serde(flatten)]
  pub info: RpcTaskInfo,
  pub wait: WaitMark,
  /// The task at the root of a failure that came from a dep.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub blocked_by: Option<String>,
  /// Removed while waited on; the rest is how it last was.
  #[serde(default, skip_serializing_if = "std::ops::Not::not")]
  pub removed: bool,
  /// Why it is (not) running, as `why` answers, when it failed; in
  /// `wait-done` only.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub why: Option<RpcWhy>,
}

/// Params of `tasks`: the wait's set as it is, in list order.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct WaitTasksEvent {
  pub tasks: Vec<RpcWaitTask>,
}

/// Params of `wait-done`, the session's last event: how the wait ended,
/// and `tasks` the set as it ended. `quitting`: the runner is stopping,
/// so nothing is wanted any more.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct WaitDoneEvent {
  pub wait: WaitOutcome,
  pub tasks: Vec<RpcWaitTask>,
  #[serde(default, skip_serializing_if = "std::ops::Not::not")]
  pub quitting: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct RpcWhy {
  pub id: TaskId,
  pub path: String,
  #[serde(flatten)]
  pub state: RpcState,
  pub wanted: bool,
  pub supported: bool,
  pub vetoed: bool,
  pub pinned: bool,
  /// Started at the last `down`; `up` starts it again.
  #[serde(default, skip_serializing_if = "std::ops::Not::not")]
  pub saved_pin: bool,
  pub required_by: Vec<String>,
  pub deps: Vec<RpcWhyDep>,
  pub attempts: u32,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct RpcWhyDep {
  pub path: String,
  #[serde(flatten)]
  pub state: RpcState,
  pub wanted: bool,
  pub satisfied: bool,
}

/// Result of `upgrade`, from the binary that took over.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct UpgradeResult {
  pub version: String,
}

#[cfg(test)]
mod tests {
  use crate::term::{Geometry, Size};

  use super::*;

  /// Every method and every command is in the requests page.
  #[test]
  fn the_requests_page_names_every_method_and_command() {
    let page = include_str!("../docs/rpc/requests.md");
    for method in METHODS {
      assert!(page.contains(&format!("- cmd: {method}\n")), "{method}");
    }
    let commands = [
      Command::Batch {
        commands: Vec::new(),
      },
      Command::Down,
      Command::Quit,
      Command::Up,
      Command::Start {
        target: Target::glob("a"),
      },
      Command::Stop {
        target: Target::glob("a"),
      },
      Command::Kill {
        target: Target::glob("a"),
      },
      Command::Veto {
        target: Target::glob("a"),
      },
      Command::Restart {
        target: Target::glob("a"),
      },
      Command::ForceRestart {
        target: Target::glob("a"),
      },
      Command::Add {
        target: Target::glob("a"),
        spec: Default::default(),
        place: Default::default(),
        start: true,
        defaults: true,
        origin: None,
      },
      Command::Update {
        target: Target::glob("a"),
        spec: Default::default(),
        was: None,
        restart: false,
      },
      Command::Move {
        target: Target::glob("a"),
        to: None,
        place: Default::default(),
      },
      Command::Remove {
        target: Target::glob("a"),
      },
      Command::Rename {
        target: Target::glob("a"),
        name: String::new(),
      },
      Command::Duplicate {
        target: Target::glob("a"),
        name: None,
        path: None,
        start: true,
      },
      Command::SetOrigin {
        target: Target::glob("a"),
        origin: None,
      },
    ];
    for command in &commands {
      // A new command fails here until it is listed above.
      match command {
        Command::Batch { .. }
        | Command::Down
        | Command::Quit
        | Command::Up
        | Command::Start { .. }
        | Command::Stop { .. }
        | Command::Kill { .. }
        | Command::Veto { .. }
        | Command::Restart { .. }
        | Command::ForceRestart { .. }
        | Command::Add { .. }
        | Command::Update { .. }
        | Command::Move { .. }
        | Command::Remove { .. }
        | Command::Rename { .. }
        | Command::Duplicate { .. }
        | Command::SetOrigin { .. } => (),
      }
      let json = serde_json::to_value(command).unwrap();
      let tag = json["command"].as_str().unwrap();
      assert!(page.contains(&format!("`{tag}")), "{tag}");
    }
  }

  /// What `reload` replies is wire API: its shape is pinned here.
  fn samples() -> Vec<RpcRequest> {
    vec![
      RpcRequest::Command(Command::Start {
        target: Target::glob("web"),
      }),
      RpcRequest::Command(Command::Add {
        target: Target::glob("api"),
        spec: serde_yaml::from_str(
          "{cmd: [./api], cwd: /repo, deps: [db], tags: [backend]}",
        )
        .unwrap(),
        place: Default::default(),
        start: true,
        defaults: true,
        origin: None,
      }),
      RpcRequest::Command(Command::Down),
      RpcRequest::Reload { apply: true },
      RpcRequest::Command(Command::Quit),
      RpcRequest::Ls { target: None },
      RpcRequest::Ls {
        target: Some(Target::glob("services/*")),
      },
      RpcRequest::Why {
        target: Target::glob("web"),
      },
      RpcRequest::Screen {
        target: Target::Id(TaskId(3)),
      },
      RpcRequest::Attach {
        target: "@dekit/console".parse().unwrap(),
        geometry: GeometryParams::of(Geometry::cells(80, 24)),
        until_exit: false,
        caps: None,
      },
      RpcRequest::Attach {
        target: Target::glob("web/dev"),
        geometry: GeometryParams::of(Geometry::cells(80, 24)),
        until_exit: true,
        caps: None,
      },
      RpcRequest::Attach {
        target: Target::glob("web/dev"),
        geometry: GeometryParams::of(Geometry {
          size: Size {
            width: 80,
            height: 24,
          },
          cell_pixels: Some((10, 20)),
        }),
        until_exit: false,
        caps: Some(TermCaps::full()),
      },
      RpcRequest::Upgrade {
        binary: "/opt/dekit/bin/dekit".to_string(),
      },
      RpcRequest::Command(Command::Up),
      RpcRequest::Wait {
        target: Some(Target::glob("**")),
        command: None,
        wait_for: None,
        timeout_ms: Some(0),
      },
      RpcRequest::Wait {
        target: Some(Target::tag("ci")),
        command: None,
        wait_for: Some(WaitFor::Exit),
        timeout_ms: None,
      },
      RpcRequest::Wait {
        target: None,
        command: Some(Command::Restart {
          target: Target::glob("api"),
        }),
        wait_for: None,
        timeout_ms: Some(90_000),
      },
    ]
  }

  /// Append-only: method names and param shapes are wire API.
  #[test]
  fn golden_methods_encode_exactly() {
    let expected = [
      ("command", r#"{"command":"start","target":"web"}"#),
      (
        "command",
        r#"{"command":"add","spec":{"cmd":["./api"],"cwd":"/repo","deps":["db"],"tags":["backend"]},"target":"api"}"#,
      ),
      ("command", r#"{"command":"down"}"#),
      ("reload", r#"{"apply":true}"#),
      ("command", r#"{"command":"quit"}"#),
      ("ls", r#"null"#),
      ("ls", r#"{"target":"services/*"}"#),
      ("why", r#"{"target":"web"}"#),
      ("screen", r#"{"target":{"id":3}}"#),
      (
        "attach",
        r#"{"height":24,"target":"@dekit/console","width":80}"#,
      ),
      (
        "attach",
        r#"{"height":24,"target":"web/dev","until_exit":true,"width":80}"#,
      ),
      // `atch2`: the terminal's capabilities and cell size.
      (
        "attach",
        r#"{"caps":{"colors":"true-color","features":["italic","strikethrough","underline-styles","underline-colors","hyperlinks","sync","focus"],"kitty":{"graphics":["placeholders","animation"],"keyboard":31},"widths":{"flags_wide":true,"vs16_wide":true,"zwj_joins":true}},"cell_height":20,"cell_width":10,"height":24,"target":"web/dev","width":80}"#,
      ),
      ("upgrade", r#"{"binary":"/opt/dekit/bin/dekit"}"#),
      ("command", r#"{"command":"up"}"#),
      ("wait", r#"{"target":"**","timeout_ms":0}"#),
      ("wait", r#"{"for":"exit","target":"+ci"}"#),
      (
        "wait",
        r#"{"command":{"command":"restart","target":"api"},"timeout_ms":90000}"#,
      ),
    ];
    let samples = samples();
    assert_eq!(samples.len(), expected.len());
    for (req, (method, params)) in samples.iter().zip(expected) {
      let (m, p) = req.to_wire();
      assert_eq!(m, method);
      assert_eq!(serde_json::to_string(&p).unwrap(), params);
    }
  }

  /// Append-only: result shapes are wire API too.
  #[test]
  fn golden_results_encode_exactly() {
    let ready = RpcState {
      state: "ready".to_string(),
      exit_code: None,
      signal: None,
      reason: None,
    };
    let exited = RpcState {
      state: "exited".to_string(),
      exit_code: Some(1),
      signal: None,
      reason: Some("ready_timeout".to_string()),
    };
    let list = TaskListResult {
      tasks: vec![
        RpcTaskInfo {
          id: TaskId(3),
          path: "db".to_string(),
          label: None,
          kind: None,
          state: ready.clone(),
        },
        RpcTaskInfo {
          id: TaskId(7),
          path: "web/dev".to_string(),
          label: Some("Web".to_string()),
          kind: None,
          state: exited.clone(),
        },
      ],
    };
    assert_eq!(
      serde_json::to_string(&list).unwrap(),
      r#"{"tasks":[{"id":3,"path":"db","state":"ready"},{"id":7,"path":"web/dev","label":"Web","state":"exited","exit_code":1,"reason":"ready_timeout"}]}"#
    );
    let why = RpcWhy {
      id: TaskId(7),
      path: "web/dev".to_string(),
      state: exited,
      wanted: true,
      supported: true,
      vetoed: false,
      pinned: false,
      saved_pin: false,
      required_by: vec!["@dekit/console".to_string()],
      deps: vec![RpcWhyDep {
        path: "db".to_string(),
        state: ready,
        wanted: true,
        satisfied: true,
      }],
      attempts: 2,
    };
    assert_eq!(
      serde_json::to_string(&why).unwrap(),
      r#"{"id":7,"path":"web/dev","state":"exited","exit_code":1,"reason":"ready_timeout","wanted":true,"supported":true,"vetoed":false,"pinned":false,"required_by":["@dekit/console"],"deps":[{"path":"db","state":"ready","wanted":true,"satisfied":true}],"attempts":2}"#
    );
    assert_eq!(
      serde_json::to_string(&ActResult { matched: 2 }).unwrap(),
      r#"{"matched":2}"#
    );
    assert_eq!(
      serde_json::to_string(&QuitResult {
        stop_within_ms: 1500
      })
      .unwrap(),
      r#"{"stop_within_ms":1500}"#
    );
    assert_eq!(
      serde_json::to_string(&ScreenResult {
        screen: "ok".to_string()
      })
      .unwrap(),
      r#"{"screen":"ok"}"#
    );
    assert_eq!(ok_result().to_string(), "{}");

    let api = RpcTaskInfo {
      id: TaskId(4),
      path: "api".to_string(),
      label: None,
      kind: Some("service".to_string()),
      state: RpcState {
        state: "idle".to_string(),
        exit_code: None,
        signal: None,
        reason: None,
      },
    };
    let pending = RpcWaitTask {
      info: api.clone(),
      wait: WaitMark::Pending,
      blocked_by: None,
      removed: false,
      why: None,
    };
    assert_eq!(
      serde_json::to_string(&WaitTasksEvent {
        tasks: vec![pending]
      })
      .unwrap(),
      r#"{"tasks":[{"id":4,"path":"api","kind":"service","state":"idle","wait":"pending"}]}"#
    );
    // `task-removed` carries the task as it last was, marked removed.
    let removed = RpcWaitTask {
      info: api.clone(),
      wait: WaitMark::Met,
      blocked_by: None,
      removed: true,
      why: None,
    };
    assert_eq!(
      serde_json::to_string(&removed).unwrap(),
      r#"{"id":4,"path":"api","kind":"service","state":"idle","wait":"met","removed":true}"#
    );
    let failed = RpcWaitTask {
      info: api,
      wait: WaitMark::Failed,
      blocked_by: Some("db".to_string()),
      removed: false,
      why: None,
    };
    assert_eq!(
      serde_json::to_string(&WaitDoneEvent {
        wait: WaitOutcome::Failed,
        tasks: vec![failed],
        quitting: false,
      })
      .unwrap(),
      r#"{"wait":"failed","tasks":[{"id":4,"path":"api","kind":"service","state":"idle","wait":"failed","blocked_by":"db"}]}"#
    );
    assert_eq!(
      serde_json::to_string(&WaitDoneEvent {
        wait: WaitOutcome::Failed,
        tasks: Vec::new(),
        quitting: true,
      })
      .unwrap(),
      r#"{"wait":"failed","tasks":[],"quitting":true}"#
    );
    let stopped = RpcState {
      state: "idle".to_string(),
      exit_code: None,
      signal: None,
      reason: Some("kill_timeout".to_string()),
    };
    assert_eq!(
      serde_json::to_string(&stopped).unwrap(),
      r#"{"state":"idle","reason":"kill_timeout"}"#
    );
  }

  #[test]
  fn every_request_round_trips_through_wire() {
    for req in samples() {
      let (method, params) = req.to_wire();
      let back = RpcRequest::from_wire(&method, params)
        .unwrap_or_else(|e| panic!("{method}: {e}"));
      assert_eq!(back, req);
    }
  }

  #[test]
  fn methods_list_matches_the_enum() {
    let from_samples: std::collections::HashSet<String> =
      samples().iter().map(|req| req.to_wire().0).collect();
    let listed: std::collections::HashSet<String> =
      METHODS.iter().map(|m| m.to_string()).collect();
    assert_eq!(from_samples, listed);
  }

  #[test]
  fn unknown_method_is_reported_as_such() {
    let err = RpcRequest::from_wire("frobnicate", Value::Null).unwrap_err();
    assert_eq!(err.code, codes::UNKNOWN_METHOD);
  }

  #[test]
  fn bad_params_are_reported_as_such() {
    let err = RpcRequest::from_wire("why", serde_json::json!({"target": 5}))
      .unwrap_err();
    assert_eq!(err.code, codes::INVALID_PARAMS);
    let err = RpcRequest::from_wire(
      "command",
      serde_json::json!({"command": "start", "target": "*::x"}),
    )
    .unwrap_err();
    assert_eq!(err.code, codes::INVALID_PARAMS);
  }

  #[test]
  fn missing_params_object_is_tolerated() {
    assert_eq!(
      RpcRequest::from_wire("ls", Value::Null).unwrap(),
      RpcRequest::Ls { target: None }
    );
  }

  #[test]
  fn unknown_param_fields_are_ignored() {
    let req = RpcRequest::from_wire(
      "why",
      serde_json::json!({"target": "x", "future_field": true}),
    )
    .unwrap();
    assert_eq!(
      req,
      RpcRequest::Why {
        target: Target::glob("x")
      }
    );
  }
}
