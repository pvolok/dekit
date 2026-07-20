use std::path::PathBuf;

use anyhow::Result;
use serde_yaml::Value;

pub use crate::config::task_log::{LogMode, TaskLogConfig as LogConfig};
use crate::mprocs::yaml_val::Val;

pub fn parse_log_config<F>(
  val: &Val<'_>,
  mut resolve_path: F,
) -> Result<Option<LogConfig>>
where
  F: FnMut(&str) -> Result<PathBuf>,
{
  match val.raw() {
    Value::Null => Ok(None),
    Value::Bool(false) => Ok(Some(LogConfig::disabled())),
    Value::Bool(true) => Ok(Some(LogConfig {
      enabled: Some(true),
      dir: None,
      file: None,
      mode: None,
    })),
    Value::Number(_) => {
      Err(val.error_at("Expected bool, string, object, or null"))
    }
    Value::String(dir) => Ok(Some(LogConfig::with_dir(resolve_path(dir)?))),
    Value::Sequence(_) => {
      Err(val.error_at("Expected bool, string, object, or null"))
    }
    Value::Mapping(_) => {
      let map = val.as_object()?;

      let enabled = map
        .get(&Value::from("enabled"))
        .map_or(Ok(true), |v| v.as_bool())?;

      let dir = match map.get(&Value::from("dir")) {
        Some(dir) => match dir.raw() {
          Value::Null => None,
          Value::String(path) => Some(resolve_path(path)?),
          _ => return Err(dir.error_at("Expected string or null")),
        },
        None => None,
      };

      let file = match map.get(&Value::from("file")) {
        Some(file) => match file.raw() {
          Value::Null => None,
          Value::String(path) => Some(resolve_path(path)?),
          _ => return Err(file.error_at("Expected string or null")),
        },
        None => None,
      };

      let mode = match (
        map.get(&Value::from("mode")),
        map.get(&Value::from("append")),
      ) {
        (Some(_), Some(append)) => {
          return Err(
            append.error_at("Use either `mode` or `append`, not both"),
          );
        }
        (Some(mode), None) => Some(match mode.as_str()? {
          "append" => LogMode::Append,
          "truncate" => LogMode::Truncate,
          _ => return Err(mode.error_at("Expected `append` or `truncate`")),
        }),
        (None, Some(append)) => Some(if append.as_bool()? {
          LogMode::Append
        } else {
          LogMode::Truncate
        }),
        (None, None) => None,
      };

      Ok(Some(LogConfig {
        enabled: Some(enabled),
        dir,
        file,
        mode,
      }))
    }
    Value::Tagged(_) => anyhow::bail!("Yaml tags are not supported"),
  }
}
