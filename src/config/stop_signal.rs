use std::time::Duration;

use anyhow::{Result, bail};

use crate::cfg::{CfgCx, CfgNode, FromCfg};
use crate::config::task::{NO_SHELL, argv_from_cfg, refuse_removed};
pub use crate::task::process_task::{Sig, StopSignal};
use crate::term::key::Key;

/// Keys of a task's `stop` object.
pub(crate) const STOP_KEYS: &[&str] =
  &["signal", "group", "keys", "cmd", "timeout"];

/// A task's `stop`: how, and how long before the hard kill. Each part a
/// task leaves out comes from `defaults`.
#[derive(Clone, Debug)]
pub struct StopConfig {
  pub signal: Option<StopSignal>,
  pub timeout: Option<Duration>,
}

impl StopConfig {
  pub fn merged(self, over: StopConfig) -> StopConfig {
    StopConfig {
      signal: over.signal.or(self.signal),
      timeout: over.timeout.or(self.timeout),
    }
  }
}

impl FromCfg for StopConfig {
  fn from_cfg(node: &CfgNode<'_>, cx: &CfgCx) -> Result<Self> {
    if !node.is_mapping() {
      return Ok(StopConfig {
        signal: Some(StopSignal::Signal {
          sig: sig_from_cfg(node)?,
          group: true,
        }),
        timeout: None,
      });
    }
    let obj = node.as_obj()?;
    refuse_removed(
      &obj,
      &[NO_SHELL, ("send-keys", "'send-keys' is now 'keys'")],
    )?;
    obj.known_keys(STOP_KEYS)?;
    let signal = match (obj.get("signal"), obj.get("keys"), obj.get("cmd")) {
      (None, None, None) => None,
      (Some(signal), None, None) => Some(StopSignal::Signal {
        sig: sig_from_cfg(&signal)?,
        group: obj.default("group", true, cx)?,
      }),
      (None, Some(keys), None) => Some(StopSignal::Keys(
        keys
          .as_arr()?
          .iter()
          .map(|key| Key::parse(key.as_str()?).map_err(|err| key.error(err)))
          .collect::<Result<_>>()?,
      )),
      (None, None, Some(cmd)) => Some(StopSignal::Cmd(argv_from_cfg(&cmd)?)),
      _ => {
        bail!(obj.error("stop takes at most one of 'signal', 'keys', or 'cmd'"))
      }
    };
    if obj.get("signal").is_none()
      && let Some(group) = obj.get("group")
    {
      bail!(group.error("'group' goes with 'signal'"));
    }
    Ok(StopConfig {
      signal,
      timeout: obj.optional("timeout", cx)?,
    })
  }
}

fn sig_from_cfg(node: &CfgNode<'_>) -> Result<Sig> {
  let name = node.as_str()?;
  Sig::from_name(name).ok_or_else(|| {
    node.error(format!(
      "unknown signal '{name}'; expected a signal name such as SIGINT or SIGTERM"
    ))
  })
}

#[cfg(test)]
mod tests {
  use std::path::PathBuf;

  use super::*;
  use crate::cfg::CfgDoc;
  use crate::term::key::{KeyCode, KeyMods, MediaKeyCode};

  fn parse(yaml: &str) -> Result<StopConfig> {
    let value: serde_yaml::Value = serde_yaml::from_str(yaml).unwrap();
    let cx = CfgCx::new(PathBuf::from("."));
    let doc = CfgDoc::from_value(value, &cx).unwrap();
    StopConfig::from_cfg(&doc.root(), &cx)
  }

  fn stop(yaml: &str) -> StopSignal {
    parse(yaml).unwrap().signal.unwrap()
  }

  #[test]
  fn signal_name_targets_the_group() {
    match stop("SIGINT") {
      StopSignal::Signal {
        sig: Sig::Int,
        group: true,
      } => (),
      other => panic!("{other:?}"),
    }
    // Any standard signal, not just INT/TERM/KILL.
    match stop("SIGHUP") {
      StopSignal::Signal {
        sig: Sig::Hup,
        group: true,
      } => (),
      other => panic!("{other:?}"),
    }
    match stop("{signal: SIGKILL, group: false}") {
      StopSignal::Signal {
        sig: Sig::Kill,
        group: false,
      } => (),
      other => panic!("{other:?}"),
    }
    assert_eq!(parse("SIGINT").unwrap().timeout, None);
  }

  #[test]
  fn object_forms() {
    match stop("{keys: ['<C-a>', '<F13>', '<MediaPlayPause>']}") {
      StopSignal::Keys(keys) => assert_eq!(
        keys,
        [
          Key::new(KeyCode::Char('a'), KeyMods::CONTROL),
          Key::new(KeyCode::F(13), KeyMods::NONE),
          Key::new(KeyCode::Media(MediaKeyCode::PlayPause), KeyMods::NONE),
        ]
      ),
      other => panic!("{other:?}"),
    }
    match stop("{cmd: 'docker compose stop'}") {
      StopSignal::Cmd(argv) => assert_eq!(argv, ["docker", "compose", "stop"]),
      other => panic!("{other:?}"),
    }
    match stop("{cmd: \"kill -INT '1 2'\"}") {
      StopSignal::Cmd(argv) => assert_eq!(argv, ["kill", "-INT", "1 2"]),
      other => panic!("{other:?}"),
    }
    match stop("{cmd: [kill, -INT, '1 2']}") {
      StopSignal::Cmd(argv) => assert_eq!(argv, ["kill", "-INT", "1 2"]),
      other => panic!("{other:?}"),
    }
    // Only a timeout: the signal comes from `defaults`.
    let config = parse("{timeout: 30s}").unwrap();
    assert!(config.signal.is_none(), "{config:?}");
    assert_eq!(config.timeout, Some(Duration::from_secs(30)));
  }

  #[test]
  fn bad_forms() {
    for (yaml, expected) in [
      ("shutdown", "unknown signal 'shutdown'"),
      ("kill", "unknown signal 'kill'"),
      ("{signal: TERM}", "unknown signal 'TERM'"),
      ("{cmd: x, keys: []}", "stop takes at most one of"),
      ("{signal: SIGINT, cmd: x}", "stop takes at most one of"),
      ("{shell: x}", "'shell' is not supported"),
      ("{cmd: x, group: true}", "'group' goes with 'signal'"),
      ("{group: false}", "'group' goes with 'signal'"),
      ("{send-keys: ['<C-c>']}", "'send-keys' is now 'keys'"),
      ("{keys: ['.exit']}", "Expected \"<\""),
      ("{cmd: x, timeout: 5}", "expected a duration"),
    ] {
      let err = match parse(yaml) {
        Ok(config) => panic!("{yaml}: accepted {config:?}"),
        Err(err) => err.to_string(),
      };
      assert!(err.contains(expected), "{yaml}: {err}");
    }
  }
}
