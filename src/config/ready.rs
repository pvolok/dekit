use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Result, bail};

use crate::cfg::{CfgCx, CfgNode, FromCfg};
use crate::config::task::{NO_SHELL, argv_from_cfg, refuse_removed};
use crate::task::ready::{
  DEFAULT_INTERVAL, Probe, ReadyCheck, ReadyConfig, parse_http_url, parse_tcp,
};

/// A task's `ready` object takes one of these checks, and these settings.
pub(crate) const READY_CHECKS: &[&str] = &["log", "tcp", "http", "cmd", "file"];
pub(crate) const READY_SETTINGS: &[&str] = &["interval", "timeout"];

impl FromCfg for ReadyConfig {
  fn from_cfg(node: &CfgNode<'_>, cx: &CfgCx) -> Result<Self> {
    if !node.is_mapping() {
      bail!(node.error(
        "expected an object with one check, such as {log: \"listening\"}"
      ));
    }
    let obj = node.as_obj()?;
    refuse_removed(&obj, &[NO_SHELL])?;
    obj.known_keys(&[READY_CHECKS, READY_SETTINGS].concat())?;
    let checks: Vec<(&str, CfgNode<'_>)> = READY_CHECKS
      .iter()
      .filter_map(|key| Some((*key, obj.get(key)?)))
      .collect();
    let (name, check) = match checks.as_slice() {
      [one] => one.clone(),
      [] => {
        bail!(obj.error("ready needs a check: log, tcp, http, cmd, or file"))
      }
      [..] => bail!(obj.error(format!(
        "ready takes one check, found {}",
        checks
          .iter()
          .map(|(key, _)| format!("'{key}'"))
          .collect::<Vec<_>>()
          .join(", ")
      ))),
    };
    let interval: Option<Duration> = obj.optional("interval", cx)?;
    let probe = |probe| ReadyCheck::Probe {
      probe,
      interval: interval.unwrap_or(DEFAULT_INTERVAL),
    };
    let check = match name {
      "log" => {
        if let Some(interval) = obj.get("interval") {
          bail!(interval.error("'interval' does not apply to a log check"));
        }
        let text = check.as_str()?;
        if text.is_empty() {
          bail!(check.error("the log text is empty"));
        }
        ReadyCheck::Log(text.to_string())
      }
      "tcp" => {
        let (host, port) = match check.raw().as_u64() {
          Some(port) => match u16::try_from(port) {
            Ok(port) if port != 0 => (None, port),
            _ => bail!(check.error(format!("bad port {port}"))),
          },
          None => parse_tcp(check.as_str()?).map_err(|err| check.error(err))?,
        };
        probe(Probe::Tcp { host, port })
      }
      "http" => probe(Probe::Http(
        parse_http_url(check.as_str()?).map_err(|err| check.error(err))?,
      )),
      "cmd" => probe(Probe::Cmd {
        argv: argv_from_cfg(&check)?,
      }),
      "file" => probe(Probe::File {
        path: PathBuf::from(check.as_str()?),
      }),
      _ => unreachable!("every check is listed"),
    };
    Ok(ReadyConfig {
      check,
      timeout: obj.optional("timeout", cx)?,
    })
  }
}
