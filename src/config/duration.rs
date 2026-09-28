use std::time::Duration;

use anyhow::{Result, bail};

use crate::cfg::{CfgCx, CfgNode, FromCfg};

/// The longest duration a config may set, so a deadline built from it
/// can't overflow.
const MAX: Duration = Duration::from_secs(24 * 3600);

/// `500ms`, `10s`, `2m`, `1h`: a positive whole number and a unit, at
/// most 24h.
pub fn parse_duration(text: &str) -> Result<Duration> {
  let split = text
    .find(|c: char| !c.is_ascii_digit())
    .unwrap_or(text.len());
  let (number, unit) = text.split_at(split);
  let n = number
    .parse::<u64>()
    .ok()
    .filter(|&n| n > 0 && !number.starts_with('0'));
  let duration = match (n, unit) {
    (Some(n), "ms") => Duration::from_millis(n),
    (Some(n), "s") => Duration::from_secs(n),
    (Some(n), "m") => Duration::from_secs(n.saturating_mul(60)),
    (Some(n), "h") => Duration::from_secs(n.saturating_mul(3600)),
    _ => {
      bail!("expected a duration such as 500ms, 10s, 2m, or 1h, got '{text}'")
    }
  };
  if duration > MAX {
    bail!("'{text}' is longer than 24h");
  }
  Ok(duration)
}

impl FromCfg for Duration {
  fn from_cfg(node: &CfgNode<'_>, _cx: &CfgCx) -> Result<Self> {
    let Some(text) = node.raw().as_str() else {
      bail!(node.error("expected a duration such as 500ms, 10s, 2m, or 1h"));
    };
    parse_duration(text).map_err(|err| node.error(err))
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn durations() {
    assert_eq!(parse_duration("500ms").unwrap(), Duration::from_millis(500));
    assert_eq!(parse_duration("10s").unwrap(), Duration::from_secs(10));
    assert_eq!(parse_duration("2m").unwrap(), Duration::from_secs(120));
    assert_eq!(parse_duration("1h").unwrap(), Duration::from_secs(3600));
    assert_eq!(parse_duration("24h").unwrap(), Duration::from_secs(86400));
    for bad in [
      "",
      "10",
      "s",
      "0s",
      "05s",
      "1.5s",
      "10 s",
      "-1s",
      "3d",
      "25h",
      "86401s",
      "18446744073709551615s",
    ] {
      assert!(parse_duration(bad).is_err(), "{bad}");
    }
  }
}
