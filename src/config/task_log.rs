use std::{
  path::PathBuf,
  time::{SystemTime, UNIX_EPOCH},
};

use anyhow::Result;
use serde_yaml::Value;

use crate::cfg::{CfgCx, CfgNode, FromCfg};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum LogMode {
  #[default]
  Append,
  Truncate,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TaskLogConfig {
  pub enabled: Option<bool>,
  pub dir: Option<PathBuf>,
  pub file: Option<PathBuf>,
  pub mode: Option<LogMode>,
}

impl TaskLogConfig {
  pub fn disabled() -> Self {
    Self {
      enabled: Some(false),
      dir: None,
      file: None,
      mode: None,
    }
  }

  pub fn with_dir(dir: PathBuf) -> Self {
    Self {
      enabled: Some(true),
      dir: Some(dir),
      file: None,
      mode: None,
    }
  }

  pub fn enabled(&self) -> bool {
    self.enabled.unwrap_or(true)
  }

  pub fn mode(&self) -> LogMode {
    self.mode.unwrap_or(LogMode::Append)
  }

  pub fn merged(&self, child: &TaskLogConfig) -> TaskLogConfig {
    TaskLogConfig {
      enabled: child.enabled.or(self.enabled),
      dir: child.dir.clone().or_else(|| self.dir.clone()),
      file: child.file.clone().or_else(|| self.file.clone()),
      mode: child.mode.or(self.mode),
    }
  }

  pub fn file_path(
    &self,
    task_name: &str,
    task_id: usize,
    pid: u32,
  ) -> Option<PathBuf> {
    if !self.enabled() {
      return None;
    }

    let dir = self.dir.as_ref().map(|dir| {
      PathBuf::from(expand_template(
        &dir.to_string_lossy(),
        task_name,
        task_id,
        pid,
      ))
    });
    let file = self.file.as_ref().map(|file| {
      PathBuf::from(expand_template(
        &file.to_string_lossy(),
        task_name,
        task_id,
        pid,
      ))
    });

    match (dir, file) {
      (None, None) => None,
      (Some(dir), None) => Some(dir.join(default_log_filename(task_name))),
      (None, Some(file)) => Some(file),
      (Some(dir), Some(file)) if file.is_relative() => Some(dir.join(file)),
      (Some(_dir), Some(file)) => Some(file),
    }
  }
}

pub fn default_log_filename(name: &str) -> String {
  let mut out = String::new();
  for ch in name.chars() {
    let is_safe = ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.');
    if is_safe {
      out.push(ch);
    } else {
      out.push('_');
    }
  }

  let trimmed = out.trim_matches(|c| c == '.' || c == ' ').to_string();
  if trimmed.is_empty() {
    "task.log".to_string()
  } else {
    format!("{}.log", trimmed)
  }
}

impl FromCfg for TaskLogConfig {
  fn from_cfg(node: &CfgNode<'_>, cx: &CfgCx) -> Result<Self> {
    Ok(parse_log_config(node, cx)?.unwrap_or_else(TaskLogConfig::disabled))
  }
}

impl FromCfg for LogMode {
  fn from_cfg(node: &CfgNode<'_>, _cx: &CfgCx) -> Result<Self> {
    match node.as_str()? {
      "append" => Ok(LogMode::Append),
      "truncate" => Ok(LogMode::Truncate),
      _ => Err(node.error("Expected `append` or `truncate`")),
    }
  }
}

fn parse_log_config(
  node: &CfgNode<'_>,
  cx: &CfgCx,
) -> Result<Option<TaskLogConfig>> {
  match node.raw() {
    Value::Null => Ok(None),
    Value::Bool(false) => Ok(Some(TaskLogConfig::disabled())),
    Value::Bool(true) => Ok(Some(TaskLogConfig {
      enabled: Some(true),
      dir: None,
      file: None,
      mode: None,
    })),
    Value::String(dir) => {
      Ok(Some(TaskLogConfig::with_dir(cx.resolve_path(dir))))
    }
    Value::Mapping(_) => {
      let obj = node.as_obj()?;

      let enabled = obj.default("enabled", true, cx)?;

      let dir = match obj.get("dir") {
        Some(dir) if !dir.is_null() => Some(cx.resolve_path(dir.as_str()?)),
        _ => None,
      };

      let file = match obj.get("file") {
        Some(file) if !file.is_null() => Some(cx.resolve_path(file.as_str()?)),
        _ => None,
      };

      let mode = match (obj.get("mode"), obj.get("append")) {
        (Some(_), Some(append)) => {
          return Err(append.error("Use either `mode` or `append`, not both"));
        }
        (Some(mode), None) => Some(mode.parse::<LogMode>(cx)?),
        (None, Some(append)) => Some(if append.as_bool()? {
          LogMode::Append
        } else {
          LogMode::Truncate
        }),
        (None, None) => None,
      };

      Ok(Some(TaskLogConfig {
        enabled: Some(enabled),
        dir,
        file,
        mode,
      }))
    }
    Value::Number(_) | Value::Sequence(_) | Value::Tagged(_) => {
      Err(node.error("Expected bool, string, object, or null"))
    }
  }
}

fn expand_template(
  template: &str,
  task_name: &str,
  task_id: usize,
  pid: u32,
) -> String {
  let ts = SystemTime::now()
    .duration_since(UNIX_EPOCH)
    .map(|duration| duration.as_secs())
    .unwrap_or(0);

  template
    .replace(
      "{name}",
      &default_log_filename(task_name).trim_end_matches(".log"),
    )
    .replace("{id}", &task_id.to_string())
    .replace("{pid}", &pid.to_string())
    .replace("{ts}", &ts.to_string())
}
