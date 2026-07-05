use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Bumped only if the framing or envelope breaks; additive changes
/// (new methods, events, fields, error codes) keep it at 1.
pub const PROTOCOL_VERSION: u32 = 1;

pub fn local_hello() -> Hello {
  Hello {
    protocol: PROTOCOL_VERSION,
    app: format!("dekit {}", env!("CARGO_PKG_VERSION")),
    features: Vec::new(),
  }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum CtlMsg {
  Hello(Hello),
  Request(Request),
  Response(Response),
  Event(Event),
  Bye(Bye),
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct Hello {
  pub protocol: u32,
  #[serde(default)]
  pub app: String,
  #[serde(default)]
  pub features: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct Request {
  pub id: u64,
  pub method: String,
  #[serde(default, skip_serializing_if = "Value::is_null")]
  pub params: Value,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct Response {
  pub id: u64,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub result: Option<Value>,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub error: Option<RpcError>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct Event {
  pub name: String,
  #[serde(default, skip_serializing_if = "Value::is_null")]
  pub params: Value,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct Bye {
  pub code: String,
  #[serde(default, skip_serializing_if = "String::is_empty")]
  pub message: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct RpcError {
  pub code: String,
  #[serde(default)]
  pub message: String,
}

impl RpcError {
  pub fn new(code: &str, message: impl Into<String>) -> Self {
    RpcError {
      code: code.to_string(),
      message: message.into(),
    }
  }

  pub fn internal(err: impl fmt::Display) -> Self {
    RpcError::new(codes::INTERNAL, err.to_string())
  }
}

impl fmt::Display for RpcError {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    if self.message.is_empty() {
      f.write_str(&self.code)
    } else {
      f.write_str(&self.message)
    }
  }
}

impl std::error::Error for RpcError {}

impl CtlMsg {
  pub fn ok(id: u64, result: Value) -> CtlMsg {
    CtlMsg::Response(Response {
      id,
      result: Some(result),
      error: None,
    })
  }

  pub fn err(id: u64, error: RpcError) -> CtlMsg {
    CtlMsg::Response(Response {
      id,
      result: None,
      error: Some(error),
    })
  }
}

/// Error and bye codes are wire API: never renamed, never reused.
pub mod codes {
  pub const UNKNOWN_METHOD: &str = "unknown_method";
  pub const INVALID_PARAMS: &str = "invalid_params";
  pub const NO_MATCH: &str = "no_match";
  pub const NO_AUTOSTART_TARGET: &str = "no_autostart_target";
  pub const BAD_PATH: &str = "bad_path";
  pub const PATH_TAKEN: &str = "path_taken";
  pub const NO_SCREEN: &str = "no_screen";
  pub const INTERNAL: &str = "internal";
  pub const UNSUPPORTED_PROTOCOL: &str = "unsupported_protocol";
  pub const QUIT: &str = "quit";
}

/// Client-to-server event carrying a terminal input event.
pub const EVENT_INPUT: &str = "input";

#[cfg(test)]
mod tests {
  use super::*;
  use crate::term::TermEvent;
  use crate::term::key::{Key, KeyCode, KeyMods};
  use crate::term::mouse::{MouseButton, MouseEvent, MouseEventKind};

  fn input_event(event: &TermEvent) -> CtlMsg {
    CtlMsg::Event(Event {
      name: EVENT_INPUT.to_string(),
      params: serde_json::to_value(event).unwrap(),
    })
  }

  /// Append-only: every entry is wire API. A failing entry means a
  /// protocol break that older peers will not understand.
  fn golden() -> Vec<(CtlMsg, &'static str)> {
    vec![
      (
        CtlMsg::Hello(Hello {
          protocol: 1,
          app: "dekit 0.9.6".to_string(),
          features: vec![],
        }),
        r#"{"type":"hello","protocol":1,"app":"dekit 0.9.6","features":[]}"#,
      ),
      (
        CtlMsg::Request(Request {
          id: 1,
          method: "start".to_string(),
          params: serde_json::json!({"pattern": "/web*"}),
        }),
        r#"{"type":"request","id":1,"method":"start","params":{"pattern":"/web*"}}"#,
      ),
      (
        CtlMsg::ok(1, serde_json::json!({})),
        r#"{"type":"response","id":1,"result":{}}"#,
      ),
      (
        CtlMsg::err(1, RpcError::new(codes::NO_MATCH, "no tasks match '/x'")),
        r#"{"type":"response","id":1,"error":{"code":"no_match","message":"no tasks match '/x'"}}"#,
      ),
      (
        input_event(&TermEvent::Key(Key::new(
          KeyCode::Char('c'),
          KeyMods::CONTROL | KeyMods::ALT,
        ))),
        r#"{"type":"event","name":"input","params":{"Key":{"code":{"Char":"c"},"kind":"Press","mods":"CONTROL | ALT","state":""}}}"#,
      ),
      (
        input_event(&TermEvent::Key(Key::new(KeyCode::Esc, KeyMods::NONE))),
        r#"{"type":"event","name":"input","params":{"Key":{"code":"Esc","kind":"Press","mods":"","state":""}}}"#,
      ),
      (
        input_event(&TermEvent::Mouse(MouseEvent {
          kind: MouseEventKind::Down(MouseButton::Left),
          x: 3,
          y: 7,
          mods: KeyMods::NONE,
        })),
        r#"{"type":"event","name":"input","params":{"Mouse":{"kind":{"Down":"Left"},"mods":"","x":3,"y":7}}}"#,
      ),
      (
        input_event(&TermEvent::Resize(120, 40)),
        r#"{"type":"event","name":"input","params":{"Resize":[120,40]}}"#,
      ),
      (
        input_event(&TermEvent::Paste("hi".to_string())),
        r#"{"type":"event","name":"input","params":{"Paste":"hi"}}"#,
      ),
      (
        CtlMsg::Bye(Bye {
          code: codes::QUIT.to_string(),
          message: String::new(),
        }),
        r#"{"type":"bye","code":"quit"}"#,
      ),
      (
        CtlMsg::Bye(Bye {
          code: codes::UNSUPPORTED_PROTOCOL.to_string(),
          message: "speak 1".to_string(),
        }),
        r#"{"type":"bye","code":"unsupported_protocol","message":"speak 1"}"#,
      ),
    ]
  }

  #[test]
  fn golden_messages_encode_exactly() {
    for (msg, expected) in golden() {
      assert_eq!(serde_json::to_string(&msg).unwrap(), expected);
    }
  }

  #[test]
  fn golden_messages_decode_back() {
    for (msg, encoded) in golden() {
      let decoded: CtlMsg = serde_json::from_str(encoded).unwrap();
      assert_eq!(decoded, msg);
    }
  }

  #[test]
  fn unknown_fields_are_ignored() {
    let decoded: CtlMsg = serde_json::from_str(
      r#"{"type":"bye","code":"quit","future_field":{"a":1}}"#,
    )
    .unwrap();
    assert_eq!(
      decoded,
      CtlMsg::Bye(Bye {
        code: "quit".to_string(),
        message: String::new(),
      })
    );
  }

  #[test]
  fn missing_optional_fields_are_defaulted() {
    let decoded: CtlMsg =
      serde_json::from_str(r#"{"type":"hello","protocol":1}"#).unwrap();
    assert_eq!(
      decoded,
      CtlMsg::Hello(Hello {
        protocol: 1,
        app: String::new(),
        features: vec![],
      })
    );
    let decoded: CtlMsg =
      serde_json::from_str(r#"{"type":"request","id":4,"method":"shutdown"}"#)
        .unwrap();
    assert_eq!(
      decoded,
      CtlMsg::Request(Request {
        id: 4,
        method: "shutdown".to_string(),
        params: serde_json::Value::Null,
      })
    );
  }
}
