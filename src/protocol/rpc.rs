use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::protocol::ctl::{RpcError, codes};

/// Variants without fields stay `{}`-style so foreign clients sending
/// `params: {}` still parse.
#[derive(Clone, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(tag = "method", content = "params", rename_all = "snake_case")]
pub enum RpcRequest {
  Attach {
    width: u16,
    height: u16,
  },
  Spawn {
    path: String,
    cmd: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    cwd: Option<String>,
  },
  Ls {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    glob: Option<String>,
  },
  /// Start the autostart target.
  Up {},
  /// Pin matching tasks to init and start them.
  ///
  /// All pattern verbs resolve the pattern and act on the matches in a
  /// single kernel dispatch; `no_match` means zero matches at act time.
  Start {
    pattern: String,
  },
  /// Unpin matching tasks and stop their running instances; each comes
  /// back if something still wants it.
  Stop {
    pattern: String,
  },
  /// Unpin matching tasks; each stops only if nothing else wants it.
  /// Without a pattern, unpin every task.
  Down {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pattern: Option<String>,
  },
  /// Like `Stop` but with an immediate hard kill.
  Kill {
    pattern: String,
  },
  /// Veto matching tasks: they stay down until started again.
  Veto {
    pattern: String,
  },
  Restart {
    pattern: String,
  },
  /// Explain why a task is (not) running.
  Why {
    path: String,
  },
  Screen {
    path: String,
  },
  Shutdown {},
}

/// Gate for `from_wire`: methods not listed here are `unknown_method`
/// instead of `invalid_params`. Kept in sync with the enum by tests.
const METHODS: &[&str] = &[
  "attach",
  "spawn",
  "ls",
  "up",
  "start",
  "stop",
  "down",
  "kill",
  "veto",
  "restart",
  "why",
  "screen",
  "shutdown",
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

/// Result of a selector verb: how many tasks it acted on. Zero is a
/// normal outcome (the pattern matched nothing), not an error, so the
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
  pub screen: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct RpcTaskInfo {
  pub path: String,
  pub state: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct RpcWhy {
  pub path: String,
  pub state: String,
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
  pub state: String,
  pub wanted: bool,
  pub satisfied: bool,
}

#[cfg(test)]
mod tests {
  use super::*;

  fn samples() -> Vec<RpcRequest> {
    vec![
      RpcRequest::Attach {
        width: 80,
        height: 24,
      },
      RpcRequest::Spawn {
        path: "/web".to_string(),
        cmd: vec!["npm".to_string(), "start".to_string()],
        cwd: Some("/repo".to_string()),
      },
      RpcRequest::Ls { glob: None },
      RpcRequest::Ls {
        glob: Some("/web*".to_string()),
      },
      RpcRequest::Up {},
      RpcRequest::Start {
        pattern: "/web".to_string(),
      },
      RpcRequest::Stop {
        pattern: "/web".to_string(),
      },
      RpcRequest::Down { pattern: None },
      RpcRequest::Down {
        pattern: Some("/web".to_string()),
      },
      RpcRequest::Kill {
        pattern: "/web".to_string(),
      },
      RpcRequest::Veto {
        pattern: "/web".to_string(),
      },
      RpcRequest::Restart {
        pattern: "/web".to_string(),
      },
      RpcRequest::Why {
        path: "/web".to_string(),
      },
      RpcRequest::Screen {
        path: "/web".to_string(),
      },
      RpcRequest::Shutdown {},
    ]
  }

  /// Append-only: method names and param shapes are wire API.
  #[test]
  fn golden_methods_encode_exactly() {
    let expected = [
      ("attach", r#"{"height":24,"width":80}"#),
      (
        "spawn",
        r#"{"cmd":["npm","start"],"cwd":"/repo","path":"/web"}"#,
      ),
      ("ls", r#"null"#),
      ("ls", r#"{"glob":"/web*"}"#),
      ("up", r#"null"#),
      ("start", r#"{"pattern":"/web"}"#),
      ("stop", r#"{"pattern":"/web"}"#),
      ("down", r#"null"#),
      ("down", r#"{"pattern":"/web"}"#),
      ("kill", r#"{"pattern":"/web"}"#),
      ("veto", r#"{"pattern":"/web"}"#),
      ("restart", r#"{"pattern":"/web"}"#),
      ("why", r#"{"path":"/web"}"#),
      ("screen", r#"{"path":"/web"}"#),
      ("shutdown", r#"null"#),
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
    let err = RpcRequest::from_wire("start", serde_json::json!({"pattern": 5}))
      .unwrap_err();
    assert_eq!(err.code, codes::INVALID_PARAMS);
  }

  #[test]
  fn missing_params_object_is_tolerated() {
    assert_eq!(
      RpcRequest::from_wire("ls", Value::Null).unwrap(),
      RpcRequest::Ls { glob: None }
    );
    assert_eq!(
      RpcRequest::from_wire("up", serde_json::json!({})).unwrap(),
      RpcRequest::Up {}
    );
  }

  #[test]
  fn unknown_param_fields_are_ignored() {
    let req = RpcRequest::from_wire(
      "start",
      serde_json::json!({"pattern": "/x", "future_field": true}),
    )
    .unwrap();
    assert_eq!(
      req,
      RpcRequest::Start {
        pattern: "/x".to_string()
      }
    );
  }
}
