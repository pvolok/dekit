use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::command::Command;
use crate::kernel::task::TaskId;
use crate::protocol::ctl::{RpcError, codes};
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
    width: u16,
    height: u16,
    /// End the session (`task_exited` bye) when the task's execution
    /// finishes; the foreground `run` verb sets this.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    until_exit: bool,
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
const METHODS: &[&str] =
  &["command", "ls", "why", "screen", "attach", "upgrade"];

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

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct TaskListResult {
  pub tasks: Vec<RpcTaskInfo>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ScreenResult {
  pub screen: String,
}

/// A task's lifecycle state on the wire: a stable token, plus the exit
/// detail for `done`/`exited`. `state` is one of `idle`, `starting`,
/// `running`, `ready`, `stopping`, `backoff`, `done`, `exited`.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct RpcState {
  pub state: String,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub exit_code: Option<i32>,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub signal: Option<i32>,
}

/// `path` is the space-qualified target of the task (`@dekit/console`),
/// or `<task:id>` for a task without one.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct RpcTaskInfo {
  pub id: TaskId,
  pub path: String,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub label: Option<String>,
  #[serde(flatten)]
  pub state: RpcState,
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

#[cfg(test)]
mod tests {
  use crate::config::task::CmdConfig;

  use super::*;

  fn samples() -> Vec<RpcRequest> {
    vec![
      RpcRequest::Command(Command::Start {
        target: Target::glob("web"),
      }),
      RpcRequest::Command(Command::Add {
        target: Target::glob("api"),
        label: None,
        cmd: CmdConfig::Cmd {
          cmd: vec!["./api".to_string()],
        },
        cwd: Some("/repo".to_string()),
        env: None,
        deps: vec![Target::glob("db")],
        tags: vec!["backend".to_string()],
      }),
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
        width: 80,
        height: 24,
        until_exit: false,
      },
      RpcRequest::Attach {
        target: Target::glob("web/dev"),
        width: 80,
        height: 24,
        until_exit: true,
      },
      RpcRequest::Upgrade {
        binary: "/opt/dekit/bin/dekit".to_string(),
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
        r#"{"cmd":["./api"],"command":"add","cwd":"/repo","deps":["db"],"tags":["backend"],"target":"api"}"#,
      ),
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
      ("upgrade", r#"{"binary":"/opt/dekit/bin/dekit"}"#),
    ];
    let samples = samples();
    assert_eq!(samples.len(), expected.len());
    for (req, (method, params)) in samples.iter().zip(expected) {
      let (m, p) = req.to_wire();
      assert_eq!(m, method);
      assert_eq!(serde_json::to_string(&p).unwrap(), params);
    }
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
