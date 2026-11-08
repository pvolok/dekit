use std::{borrow::Cow, fmt};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::term::{
  Geometry, Size, TermEvent,
  key::{Key, KeyCode, KeyEventKind, KeyEventState, KeyMods},
  mouse::{MouseEvent, MouseEventKind},
};

/// Bumped only if the framing or envelope breaks; additive changes
/// (new methods, events, fields, error codes) keep it at 1.
pub const PROTOCOL_VERSION: u32 = 1;

/// Feature `atch2`: attach sessions in their second dialect. A server
/// that says it takes the named input events (`key`, `mouse`, `paste`,
/// `focus`, `resize`) in place of `input`, and the cell size on `attach`
/// and `resize`. PROTOCOL.md "Attach sessions".
pub const FEATURE_ATCH2: &str = "atch2";

/// What this binary says of itself as a client; `server_hello` adds what
/// it does as a server.
pub fn local_hello() -> Hello {
  Hello {
    protocol: PROTOCOL_VERSION,
    version: env!("CARGO_PKG_VERSION").to_string(),
    app: format!("dekit {}", env!("CARGO_PKG_VERSION")),
    features: Vec::new(),
  }
}

impl Hello {
  /// Whether the peer advertised `feature`.
  pub fn has(&self, feature: &str) -> bool {
    self.features.iter().any(|f| f == feature)
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
  /// The peer's semver, for machines; `app` is for humans.
  #[serde(default)]
  pub version: String,
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
  /// `task_exited` byes carry the task's final state, so a foreground
  /// client needs no follow-up query for the exit code.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub state: Option<crate::protocol::rpc::RpcState>,
  /// `task_exited` byes carry the task's final screen as ANSI text, so a
  /// foreground client can reprint it onto the main screen after leaving
  /// the alternate screen (which the terminal would otherwise restore).
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub screen: Option<String>,
  /// A `quit` bye from the console's runner selector: the runner the
  /// client attaches to next.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub switch: Option<ByeSwitch>,
}

impl Bye {
  pub fn new(code: &str, message: impl Into<String>) -> Self {
    Bye {
      code: code.to_string(),
      message: message.into(),
      state: None,
      screen: None,
      switch: None,
    }
  }
}

/// A runner on the same machine, as its record names it: `kind` is
/// `project` or `host`, `root` its absolute root directory.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct ByeSwitch {
  pub kind: String,
  pub root: String,
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
  /// More than one task matched where exactly one is needed.
  pub const AMBIGUOUS: &str = "ambiguous";
  pub const BAD_TARGET: &str = "bad_target";
  pub const PATH_TAKEN: &str = "path_taken";
  /// The task is no longer what the request was made from.
  pub const CHANGED: &str = "changed";
  /// The config on disk does not load, so there is nothing to read it
  /// against.
  pub const BAD_CONFIG: &str = "bad_config";
  pub const NO_SCREEN: &str = "no_screen";
  pub const INTERNAL: &str = "internal";
  pub const UNSUPPORTED_PROTOCOL: &str = "unsupported_protocol";
  pub const QUIT: &str = "quit";
  /// Attach-with-`until_exit` bye: the attached task's execution ended.
  pub const TASK_EXITED: &str = "task_exited";
  /// The method exists but this runner/platform cannot perform it.
  pub const UNSUPPORTED: &str = "unsupported";
  /// The operation is already in progress.
  pub const BUSY: &str = "busy";
  /// Bye: the runner is being replaced live, and this session can't be
  /// carried across.
  pub const UPGRADE: &str = "upgrade";
}

/// Client-to-server event carrying a terminal input event in the first
/// dialect: one event, the kind of input inside (`LegacyInput`). A
/// server takes it from any client; a client sends it to a server
/// without `atch2`.
pub const EVENT_INPUT: &str = "input";
/// Client-to-server input events of `atch2`: one name per kind, each
/// an object that grows by fields.
pub const EVENT_KEY: &str = "key";
pub const EVENT_MOUSE: &str = "mouse";
pub const EVENT_PASTE: &str = "paste";
pub const EVENT_FOCUS: &str = "focus";
pub const EVENT_RESIZE: &str = "resize";
/// Client-to-server event carrying a `ScreenCommand`.
pub const EVENT_SCREEN: &str = "screen";
/// Server-to-client event in an attach session: text copied from the
/// attached screen, for the client to put on the user's clipboard.
pub const EVENT_COPY: &str = "copy";

/// Params of a `copy` event.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct CopyEvent {
  pub text: String,
}

/// Geometry on the wire: columns and rows, and the size of a cell in
/// pixels when the terminal said. Flattened into `attach` and the
/// `resize` event. Zero pixels mean unknown, as in a winsize.
#[derive(Clone, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
pub struct GeometryParams {
  pub width: u16,
  pub height: u16,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub cell_width: Option<u16>,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub cell_height: Option<u16>,
}

impl GeometryParams {
  pub fn of(geometry: Geometry) -> Self {
    GeometryParams {
      width: geometry.size.width,
      height: geometry.size.height,
      cell_width: geometry.cell_pixels.map(|(w, _)| w),
      cell_height: geometry.cell_pixels.map(|(_, h)| h),
    }
  }

  pub fn geometry(&self) -> Geometry {
    Geometry {
      size: Size {
        width: self.width,
        height: self.height,
      },
      cell_pixels: self
        .cell_width
        .zip(self.cell_height)
        .filter(|(w, h)| *w > 0 && *h > 0),
    }
  }
}

/// Whether `name` is an input event of either dialect.
pub fn is_input_event(name: &str) -> bool {
  matches!(
    name,
    EVENT_INPUT
      | EVENT_KEY
      | EVENT_MOUSE
      | EVENT_PASTE
      | EVENT_FOCUS
      | EVENT_RESIZE
  )
}

/// How a server takes terminal input (PROTOCOL.md "Attach sessions").
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InputDialect {
  /// The first dialect: one `input` event with the kind of input
  /// inside, which has no room for a cell size. For a server without
  /// `atch2`.
  Input,
  /// `atch2`: one event per kind of input, each an object.
  Named,
}

impl InputDialect {
  /// The dialect a server takes, from its hello.
  pub fn of(server: &Hello) -> Self {
    if server.has(FEATURE_ATCH2) {
      InputDialect::Named
    } else {
      InputDialect::Input
    }
  }

  /// `event` as the server takes it.
  pub fn input_event(self, event: &TermEvent) -> Event {
    fn named<T: Serialize>(name: &str, params: T) -> Event {
      Event {
        name: name.to_string(),
        params: serde_json::to_value(params).expect("input serializes"),
      }
    }
    if self == InputDialect::Input {
      return named(EVENT_INPUT, LegacyInput::of(event));
    }
    match event {
      TermEvent::Key(key) => named(EVENT_KEY, KeyParams::from(key)),
      TermEvent::Mouse(mouse) => named(EVENT_MOUSE, MouseParams::from(mouse)),
      TermEvent::Paste(text) => named(
        EVENT_PASTE,
        PasteParams {
          text: Cow::Borrowed(text),
        },
      ),
      TermEvent::FocusGained => {
        named(EVENT_FOCUS, FocusParams { gained: true })
      }
      TermEvent::FocusLost => named(EVENT_FOCUS, FocusParams { gained: false }),
      TermEvent::Resize(geometry) => {
        named(EVENT_RESIZE, GeometryParams::of(*geometry))
      }
    }
  }
}

/// The terminal input an event of either dialect carries. The caller
/// checked `is_input_event`; an error is a peer bug, and the event is
/// dropped.
pub fn input_of(event: Event) -> Result<TermEvent, serde_json::Error> {
  let Event { name, params } = event;
  Ok(match name.as_str() {
    EVENT_KEY => {
      TermEvent::Key(serde_json::from_value::<KeyParams>(params)?.into())
    }
    EVENT_MOUSE => {
      TermEvent::Mouse(serde_json::from_value::<MouseParams>(params)?.into())
    }
    EVENT_PASTE => {
      let PasteParams { text } = serde_json::from_value(params)?;
      TermEvent::Paste(text.into_owned())
    }
    EVENT_FOCUS => {
      let FocusParams { gained } = serde_json::from_value(params)?;
      if gained {
        TermEvent::FocusGained
      } else {
        TermEvent::FocusLost
      }
    }
    EVENT_RESIZE => TermEvent::Resize(
      serde_json::from_value::<GeometryParams>(params)?.geometry(),
    ),
    _ => serde_json::from_value::<LegacyInput>(params)?.into(),
  })
}

#[derive(Deserialize, Serialize)]
struct PasteParams<'a> {
  text: Cow<'a, str>,
}

#[derive(Deserialize, Serialize)]
struct FocusParams {
  gained: bool,
}

/// The `key` event's params: a `Key` with its flags as lists of names
/// (`flag_names`), left out when empty. A name this build does not know
/// is dropped, and the key still reads.
#[derive(Deserialize, Serialize)]
struct KeyParams {
  code: KeyCode,
  #[serde(
    default = "KeyMods::empty",
    with = "flag_names",
    skip_serializing_if = "KeyMods::is_empty"
  )]
  mods: KeyMods,
  kind: KeyEventKind,
  #[serde(
    default = "KeyEventState::empty",
    with = "flag_names",
    skip_serializing_if = "KeyEventState::is_empty"
  )]
  state: KeyEventState,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  unshifted: Option<char>,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  base: Option<char>,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  text: Option<String>,
}

impl From<&Key> for KeyParams {
  fn from(key: &Key) -> KeyParams {
    KeyParams {
      code: key.code,
      mods: key.mods,
      kind: key.kind,
      state: key.state,
      unshifted: key.unshifted,
      base: key.base,
      text: key.text.clone(),
    }
  }
}

impl From<KeyParams> for Key {
  fn from(params: KeyParams) -> Key {
    Key {
      code: params.code,
      mods: params.mods,
      kind: params.kind,
      state: params.state,
      unshifted: params.unshifted,
      base: params.base,
      text: params.text,
    }
  }
}

/// The `mouse` event's params: a `MouseEvent` with `mods` as for `key`.
#[derive(Deserialize, Serialize)]
struct MouseParams {
  kind: MouseEventKind,
  x: i32,
  y: i32,
  #[serde(
    default = "KeyMods::empty",
    with = "flag_names",
    skip_serializing_if = "KeyMods::is_empty"
  )]
  mods: KeyMods,
}

impl From<&MouseEvent> for MouseParams {
  fn from(mouse: &MouseEvent) -> MouseParams {
    MouseParams {
      kind: mouse.kind,
      x: mouse.x,
      y: mouse.y,
      mods: mouse.mods,
    }
  }
}

impl From<MouseParams> for MouseEvent {
  fn from(params: MouseParams) -> MouseEvent {
    MouseEvent {
      kind: params.kind,
      x: params.x,
      y: params.y,
      mods: params.mods,
    }
  }
}

/// Flags on the wire: the names of the ones set, `["control", "alt"]`,
/// which dekit owns (`NamedFlags`), so no crate's text form is.
mod flag_names {
  use serde::{Deserialize, Deserializer, Serializer};

  use crate::term::key::NamedFlags;

  pub fn serialize<F: NamedFlags, S: Serializer>(
    flags: &F,
    serializer: S,
  ) -> Result<S::Ok, S::Error> {
    serializer.collect_seq(flags.names())
  }

  pub fn deserialize<'de, F: NamedFlags, D: Deserializer<'de>>(
    deserializer: D,
  ) -> Result<F, D::Error> {
    let names = Vec::<String>::deserialize(deserializer)?;
    Ok(F::from_names(names.iter().map(String::as_str)))
  }
}

/// The `input` event's params: the first dialect's one enum, as it
/// shipped. Its tuple variant is why the second dialect exists.
#[derive(Deserialize, Serialize)]
enum LegacyInput<'a> {
  FocusGained,
  FocusLost,
  Key(Cow<'a, Key>),
  Mouse(Cow<'a, MouseEvent>),
  Paste(Cow<'a, str>),
  Resize(u16, u16),
}

impl<'a> LegacyInput<'a> {
  fn of(event: &'a TermEvent) -> LegacyInput<'a> {
    match event {
      TermEvent::FocusGained => LegacyInput::FocusGained,
      TermEvent::FocusLost => LegacyInput::FocusLost,
      TermEvent::Key(key) => LegacyInput::Key(Cow::Borrowed(key)),
      TermEvent::Mouse(mouse) => LegacyInput::Mouse(Cow::Borrowed(mouse)),
      TermEvent::Paste(text) => LegacyInput::Paste(Cow::Borrowed(text)),
      TermEvent::Resize(geometry) => {
        LegacyInput::Resize(geometry.size.width, geometry.size.height)
      }
    }
  }
}

impl From<LegacyInput<'_>> for TermEvent {
  fn from(input: LegacyInput<'_>) -> TermEvent {
    match input {
      LegacyInput::FocusGained => TermEvent::FocusGained,
      LegacyInput::FocusLost => TermEvent::FocusLost,
      LegacyInput::Key(key) => TermEvent::Key(key.into_owned()),
      LegacyInput::Mouse(mouse) => TermEvent::Mouse(mouse.into_owned()),
      LegacyInput::Paste(text) => TermEvent::Paste(text.into_owned()),
      LegacyInput::Resize(width, height) => TermEvent::resize(width, height),
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::term::TermEvent;
  use crate::term::key::{Key, KeyCode, KeyMods};
  use crate::term::mouse::{MouseButton, MouseEvent, MouseEventKind};

  fn legacy_input(event: &TermEvent) -> CtlMsg {
    CtlMsg::Event(InputDialect::Input.input_event(event))
  }

  fn named_input(event: &TermEvent) -> CtlMsg {
    CtlMsg::Event(InputDialect::Named.input_event(event))
  }

  fn with_cells(width: u16, height: u16, cell: (u16, u16)) -> TermEvent {
    TermEvent::Resize(Geometry {
      size: Size { width, height },
      cell_pixels: Some(cell),
    })
  }

  fn ctrl_alt_c() -> TermEvent {
    TermEvent::Key(Key::new(
      KeyCode::Char('c'),
      KeyMods::CONTROL | KeyMods::ALT,
    ))
  }

  fn left_down() -> TermEvent {
    TermEvent::Mouse(MouseEvent {
      kind: MouseEventKind::Down(MouseButton::Left),
      x: 3,
      y: 7,
      mods: KeyMods::NONE,
    })
  }

  /// Append-only: every entry is wire API. A failing entry means a
  /// protocol break that older peers will not understand.
  fn golden() -> Vec<(CtlMsg, &'static str)> {
    vec![
      (
        CtlMsg::Hello(Hello {
          protocol: 1,
          version: "0.9.6".to_string(),
          app: "dekit 0.9.6".to_string(),
          features: vec![],
        }),
        r#"{"type":"hello","protocol":1,"version":"0.9.6","app":"dekit 0.9.6","features":[]}"#,
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
        legacy_input(&TermEvent::Key(Key::new(
          KeyCode::Char('c'),
          KeyMods::CONTROL | KeyMods::ALT,
        ))),
        r#"{"type":"event","name":"input","params":{"Key":{"code":{"Char":"c"},"kind":"Press","mods":"CONTROL | ALT","state":""}}}"#,
      ),
      (
        legacy_input(&TermEvent::Key(Key::new(KeyCode::Esc, KeyMods::NONE))),
        r#"{"type":"event","name":"input","params":{"Key":{"code":"Esc","kind":"Press","mods":"","state":""}}}"#,
      ),
      (
        legacy_input(&TermEvent::Mouse(MouseEvent {
          kind: MouseEventKind::Down(MouseButton::Left),
          x: 3,
          y: 7,
          mods: KeyMods::NONE,
        })),
        r#"{"type":"event","name":"input","params":{"Mouse":{"kind":{"Down":"Left"},"mods":"","x":3,"y":7}}}"#,
      ),
      (
        legacy_input(&TermEvent::resize(120, 40)),
        r#"{"type":"event","name":"input","params":{"Resize":[120,40]}}"#,
      ),
      (
        legacy_input(&TermEvent::Paste("hi".to_string())),
        r#"{"type":"event","name":"input","params":{"Paste":"hi"}}"#,
      ),
      (
        CtlMsg::Bye(Bye {
          code: codes::QUIT.to_string(),
          message: String::new(),
          state: None,
          screen: None,
          switch: None,
        }),
        r#"{"type":"bye","code":"quit"}"#,
      ),
      (
        CtlMsg::Bye(Bye {
          code: codes::UNSUPPORTED_PROTOCOL.to_string(),
          message: "speak 1".to_string(),
          state: None,
          screen: None,
          switch: None,
        }),
        r#"{"type":"bye","code":"unsupported_protocol","message":"speak 1"}"#,
      ),
      (
        CtlMsg::Bye(Bye {
          code: codes::TASK_EXITED.to_string(),
          message: String::new(),
          state: Some(crate::protocol::rpc::RpcState {
            state: "exited".to_string(),
            exit_code: Some(3),
            signal: None,
            reason: None,
          }),
          screen: Some("build ok".to_string()),
          switch: None,
        }),
        r#"{"type":"bye","code":"task_exited","state":{"state":"exited","exit_code":3},"screen":"build ok"}"#,
      ),
      (
        CtlMsg::Bye(Bye {
          switch: Some(ByeSwitch {
            kind: "project".to_string(),
            root: "/home/me/app".to_string(),
          }),
          ..Bye::new(codes::QUIT, "")
        }),
        r#"{"type":"bye","code":"quit","switch":{"kind":"project","root":"/home/me/app"}}"#,
      ),
      (
        CtlMsg::Event(Event {
          name: EVENT_COPY.to_string(),
          params: serde_json::to_value(CopyEvent {
            text: "hi".to_string(),
          })
          .unwrap(),
        }),
        r#"{"type":"event","name":"copy","params":{"text":"hi"}}"#,
      ),
      (
        legacy_input(&TermEvent::FocusGained),
        r#"{"type":"event","name":"input","params":"FocusGained"}"#,
      ),
      // `atch2`: the server says so, and input comes as named events.
      (
        CtlMsg::Hello(Hello {
          protocol: 1,
          version: "0.11.0".to_string(),
          app: "dekit 0.11.0".to_string(),
          features: vec![FEATURE_ATCH2.to_string()],
        }),
        r#"{"type":"hello","protocol":1,"version":"0.11.0","app":"dekit 0.11.0","features":["atch2"]}"#,
      ),
      (
        named_input(&ctrl_alt_c()),
        r#"{"type":"event","name":"key","params":{"code":{"Char":"c"},"kind":"Press","mods":["control","alt"]}}"#,
      ),
      (
        named_input(&left_down()),
        r#"{"type":"event","name":"mouse","params":{"kind":{"Down":"Left"},"x":3,"y":7}}"#,
      ),
      (
        named_input(&TermEvent::Paste("hi".to_string())),
        r#"{"type":"event","name":"paste","params":{"text":"hi"}}"#,
      ),
      (
        named_input(&TermEvent::FocusGained),
        r#"{"type":"event","name":"focus","params":{"gained":true}}"#,
      ),
      (
        named_input(&TermEvent::FocusLost),
        r#"{"type":"event","name":"focus","params":{"gained":false}}"#,
      ),
      (
        named_input(&TermEvent::resize(120, 40)),
        r#"{"type":"event","name":"resize","params":{"height":40,"width":120}}"#,
      ),
      (
        named_input(&with_cells(120, 40, (10, 20))),
        r#"{"type":"event","name":"resize","params":{"cell_height":20,"cell_width":10,"height":40,"width":120}}"#,
      ),
    ]
  }

  #[test]
  fn input_decodes_in_either_dialect() {
    let events = [
      ctrl_alt_c(),
      left_down(),
      TermEvent::Paste("hi".to_string()),
      TermEvent::FocusGained,
      TermEvent::FocusLost,
      TermEvent::resize(120, 40),
      with_cells(120, 40, (10, 20)),
    ];
    for event in events {
      for dialect in [InputDialect::Input, InputDialect::Named] {
        let wire = dialect.input_event(&event);
        assert!(is_input_event(&wire.name), "{}", wire.name);
        let back = input_of(wire).unwrap();
        // The first dialect has no room for a cell size.
        let expected = match (&event, dialect) {
          (TermEvent::Resize(geometry), InputDialect::Input) => {
            TermEvent::Resize(Geometry {
              cell_pixels: None,
              ..*geometry
            })
          }
          _ => event.clone(),
        };
        assert_eq!(back, expected);
      }
    }
    for name in [EVENT_SCREEN, EVENT_COPY, "frobnicate"] {
      assert!(!is_input_event(name), "{name}");
    }
    // A zero cell size is a terminal that does not know, as in a winsize.
    let zero = Event {
      name: EVENT_RESIZE.to_string(),
      params: serde_json::json!({"width":1,"height":2,"cell_width":0,"cell_height":0}),
    };
    assert_eq!(input_of(zero).unwrap(), TermEvent::resize(1, 2));
    // Fields a later dialect adds are ignored, as everywhere.
    let more = Event {
      name: EVENT_PASTE.to_string(),
      params: serde_json::json!({"text":"hi","more":true}),
    };
    assert_eq!(input_of(more).unwrap(), TermEvent::Paste("hi".to_string()));
    // A flag a later build names is dropped, and the key still reads.
    let newer = Event {
      name: EVENT_KEY.to_string(),
      params: serde_json::json!({"code":"Esc","kind":"Press","mods":["control","wheel"],"state":["num-lock"]}),
    };
    let mut expected = Key::new(KeyCode::Esc, KeyMods::CONTROL);
    expected.state = KeyEventState::NUM_LOCK;
    assert_eq!(input_of(newer).unwrap(), TermEvent::Key(expected));
    // The first dialect's shapes are fixed: more in the tuple is a bug.
    let grown = Event {
      name: EVENT_INPUT.to_string(),
      params: serde_json::json!({"Resize":[1,2,3,4]}),
    };
    assert!(input_of(grown).is_err());
  }

  #[test]
  fn hello_says_whether_the_peer_speaks_atch2() {
    // A client advertises nothing; a server says `atch2`.
    assert_eq!(InputDialect::of(&local_hello()), InputDialect::Input);
    let server = crate::protocol::server_hello(&local_hello()).unwrap();
    assert!(server.has(FEATURE_ATCH2));
    assert_eq!(InputDialect::of(&server), InputDialect::Named);
    let old: CtlMsg = serde_json::from_str(
      r#"{"type":"hello","protocol":1,"version":"0.10.2","app":"dekit 0.10.2","features":[]}"#,
    )
    .unwrap();
    let CtlMsg::Hello(old) = old else { panic!() };
    assert_eq!(InputDialect::of(&old), InputDialect::Input);
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
        state: None,
        screen: None,
        switch: None,
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
        version: String::new(),
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
