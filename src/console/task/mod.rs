pub mod view;

use anyhow::bail;

use crate::cfg::{CfgCx, CfgNode, FromCfg};
use crate::mprocs::yaml_val::Val;
pub use crate::task::process_task::{Sig, StopSignal};
use crate::term::key::KeySpec;

impl StopSignal {
  pub fn from_val(val: &Val) -> anyhow::Result<Self> {
    match val.raw() {
      serde_yaml::Value::String(str) => match str.as_str() {
        "shutdown" => return Ok(StopSignal::Shutdown),
        "kill" => return Ok(StopSignal::Kill),
        _ => {
          if let Some(sig) = Sig::from_name(str) {
            return Ok(Self::Signal { sig, group: true });
          }
        }
      },
      serde_yaml::Value::Mapping(map) => {
        if let Some(keys) = map.get("send-keys") {
          let keys: Vec<KeySpec> = serde_yaml::from_value(keys.clone())?;
          let keys = keys.into_iter().map(KeySpec::key).collect();
          return Ok(Self::SendKeys(keys));
        }
        if let Some(cmd) = map.get("cmd") {
          if let serde_yaml::Value::String(shell) = cmd {
            return Ok(Self::Cmd(shell.clone()));
          }
          bail!("Expected 'cmd' to be a string");
        }
        if let Some(signal) = map.get("signal") {
          let serde_yaml::Value::String(name) = signal else {
            bail!("Expected 'signal' to be a string");
          };
          let group = match map.get("group") {
            None => true,
            Some(serde_yaml::Value::Bool(group)) => *group,
            Some(_) => bail!("Expected 'group' to be a boolean"),
          };
          return Ok(Self::Signal {
            sig: Sig::from_name(name)
              .ok_or_else(|| anyhow::format_err!("Unknown signal: {name:?}"))?,
            group,
          });
        }
      }
      _ => (),
    }
    bail!("Unexpected 'stop' value: {:?}.", val.raw());
  }
}

impl FromCfg for StopSignal {
  fn from_cfg(node: &CfgNode<'_>, _cx: &CfgCx) -> anyhow::Result<Self> {
    StopSignal::from_val(&Val::new(node.raw())?).map_err(|err| node.error(err))
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::term::key::{Key, KeyCode, KeyMods};

  #[test]
  fn stop_signal_send_keys_uses_key_specs() {
    let raw: serde_yaml::Value = serde_yaml::from_str(
      "send-keys:\n  - <C-a>\n  - <F13>\n  - <MediaPlayPause>\n",
    )
    .unwrap();
    let val = Val::new(&raw).unwrap();

    let keys = match StopSignal::from_val(&val).unwrap() {
      StopSignal::SendKeys(keys) => keys,
      other => panic!("Expected SendKeys, got {other:?}"),
    };

    assert_eq!(
      keys,
      vec![
        Key::new(KeyCode::Char('a'), KeyMods::CONTROL),
        Key::new(KeyCode::F(13), KeyMods::NONE),
        Key::new(
          KeyCode::Media(crate::term::key::MediaKeyCode::PlayPause),
          KeyMods::NONE,
        ),
      ]
    );
  }

  #[test]
  fn stop_signal_shutdown_and_kill_are_first_class() {
    let raw: serde_yaml::Value = serde_yaml::from_str("shutdown").unwrap();
    let val = Val::new(&raw).unwrap();
    match StopSignal::from_val(&val).unwrap() {
      StopSignal::Shutdown => {}
      other => panic!("Expected Shutdown, got {other:?}"),
    }

    let raw: serde_yaml::Value = serde_yaml::from_str("kill").unwrap();
    let val = Val::new(&raw).unwrap();
    match StopSignal::from_val(&val).unwrap() {
      StopSignal::Kill => {}
      other => panic!("Expected Kill, got {other:?}"),
    }
  }

  #[test]
  fn stop_signal_bare_signal_name_targets_the_group() {
    let raw: serde_yaml::Value = serde_yaml::from_str("SIGINT").unwrap();
    let val = Val::new(&raw).unwrap();
    match StopSignal::from_val(&val).unwrap() {
      StopSignal::Signal {
        sig: Sig::Int,
        group: true,
      } => {}
      other => panic!("Expected group SIGINT, got {other:?}"),
    }

    // Any standard signal is accepted, not just INT/TERM/KILL.
    let raw: serde_yaml::Value = serde_yaml::from_str("SIGHUP").unwrap();
    let val = Val::new(&raw).unwrap();
    match StopSignal::from_val(&val).unwrap() {
      StopSignal::Signal {
        sig: Sig::Hup,
        group: true,
      } => {}
      other => panic!("Expected group SIGHUP, got {other:?}"),
    }
  }

  #[test]
  fn stop_signal_object_group_overrides_default() {
    let raw: serde_yaml::Value =
      serde_yaml::from_str("signal: SIGKILL\ngroup: false\n").unwrap();
    let val = Val::new(&raw).unwrap();
    // Explicit group:false wins over the whole-group default.
    match StopSignal::from_val(&val).unwrap() {
      StopSignal::Signal {
        sig: Sig::Kill,
        group: false,
      } => {}
      other => panic!("Expected leader-only SIGKILL, got {other:?}"),
    }
  }
}
