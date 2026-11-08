//! The client side of `dekit logs`: the request on a connection of its
//! own, and the records printed as they come.

use std::{
  collections::HashSet,
  io::{self, IsTerminal, Write},
};

use anyhow::bail;
use unicode_width::UnicodeWidthStr;

use crate::{
  dekit::rpc_client::RpcConn,
  protocol::{
    CtlMsg, RpcRequest,
    rpc::{EVENT_LOGS, FEATURE_LOGS, LogsResult, RpcLogKind, RpcLogRecord},
  },
  runner::RunnerSpec,
  target::Target,
};

/// What `dekit logs` was asked for.
pub struct LogsArgs {
  pub targets: Vec<Target>,
  pub follow: bool,
  pub lines: Option<usize>,
  pub since_ms: Option<u64>,
  pub prefix: bool,
  pub json: bool,
}

/// Prints the tasks' last records and, with `follow`, the new ones until
/// Ctrl-C (a success) or the runner ends it (an error).
pub async fn cli_logs(
  runner: &RunnerSpec,
  args: LogsArgs,
) -> anyhow::Result<()> {
  // Polled from the start, so Ctrl-C is caught however far it got.
  tokio::select! {
    result = print_logs(runner, args) => result,
    // An error (no handler) leaves the logs to go on.
    Ok(()) = tokio::signal::ctrl_c() => Ok(()),
  }
}

async fn print_logs(runner: &RunnerSpec, args: LogsArgs) -> anyhow::Result<()> {
  let mut conn = RpcConn::connect(runner, false).await?;
  if !conn.hello().has(FEATURE_LOGS) {
    bail!(
      "the runner ({}) has no logs; restart it to run this dekit: `dekit down && dekit up`",
      conn.hello().app
    );
  }
  let request = RpcRequest::Logs {
    targets: args.targets,
    follow: args.follow,
    lines: args.lines,
    since_ms: args.since_ms,
  };
  let result: LogsResult =
    serde_json::from_value(conn.request(request).await?)?;
  let color = io::stdout().is_terminal() && crate::help::help::color_enabled();
  let mut printer = Printer::new(args.prefix, args.json, color, &result.tasks);
  if !printer.print(&result.records)? || !args.follow {
    return Ok(());
  }
  loop {
    match conn.recv_ctl().await {
      Ok(CtlMsg::Event(event)) if event.name == EVENT_LOGS => {
        let batch: LogsResult = serde_json::from_value(event.params)?;
        if !printer.print(&batch.records)? {
          return Ok(());
        }
      }
      Ok(CtlMsg::Bye(bye)) if bye.message.is_empty() => {
        bail!("the runner ended the logs ({})", bye.code)
      }
      Ok(CtlMsg::Bye(bye)) => bail!("{} ({})", bye.message, bye.code),
      Ok(_) => (),
      Err(_) => bail!("the runner went away"),
    }
  }
}

/// Writes records to stdout, a line each, flushed as it goes so a pipe
/// sees them at once.
struct Printer {
  prefix: bool,
  json: bool,
  color: bool,
  /// The widest name so far: names are padded to it.
  width: usize,
  /// Names already measured.
  seen: HashSet<String>,
}

impl Printer {
  fn new(prefix: bool, json: bool, color: bool, tasks: &[String]) -> Self {
    Printer {
      prefix,
      json,
      color,
      width: tasks.iter().map(|name| name.width()).max().unwrap_or(0),
      seen: tasks.iter().cloned().collect(),
    }
  }

  /// False once stdout is closed (`dekit logs -f | head`): nothing more
  /// to do.
  fn print(&mut self, records: &[RpcLogRecord]) -> anyhow::Result<bool> {
    let mut out = io::stdout().lock();
    let written = records.iter().try_for_each(|record| {
      let line = self.line(record);
      writeln!(out, "{line}")?;
      out.flush()
    });
    match written {
      Ok(()) => Ok(true),
      Err(err) if err.kind() == io::ErrorKind::BrokenPipe => Ok(false),
      Err(err) => Err(err.into()),
    }
  }

  fn line(&mut self, record: &RpcLogRecord) -> String {
    if self.json {
      return serde_json::to_string(record).expect("a record serializes");
    }
    let (text, dim) = match &record.kind {
      RpcLogKind::Line { text } | RpcLogKind::Open { text } => {
        (text.clone(), false)
      }
      RpcLogKind::Started => ("--- started".to_string(), true),
      RpcLogKind::Exited {
        code: Some(code), ..
      } => (format!("--- exited (code {code})"), true),
      RpcLogKind::Exited {
        signal: Some(signal),
        ..
      } => (format!("--- exited (signal {signal})"), true),
      RpcLogKind::Exited { .. } => ("--- exited".to_string(), true),
      RpcLogKind::Dropped { count: 1 } => {
        ("--- 1 line dropped".to_string(), true)
      }
      RpcLogKind::Dropped { count } => {
        (format!("--- {count} lines dropped"), true)
      }
    };
    let text = match dim && self.color {
      true => format!("\x1b[2m{text}\x1b[0m"),
      false => text,
    };
    if !self.prefix {
      return text;
    }
    let name = &record.task;
    if self.seen.insert(name.clone()) {
      self.width = self.width.max(name.width());
    }
    let pad = " ".repeat(self.width.saturating_sub(name.width()));
    match self.color {
      true => format!("\x1b[{}m{name}\x1b[0m{pad} | {text}", name_color(name)),
      false => format!("{name}{pad} | {text}"),
    }
  }
}

/// An SGR foreground among the six basic colors, the same for a name on
/// every run (FNV-1a, which doesn't change between builds).
fn name_color(name: &str) -> u8 {
  let hash = name.bytes().fold(0x811c_9dc5_u32, |hash, byte| {
    (hash ^ byte as u32).wrapping_mul(0x0100_0193)
  });
  31 + (hash % 6) as u8
}

#[cfg(test)]
mod tests {
  use super::*;

  fn record(task: &str, kind: RpcLogKind) -> RpcLogRecord {
    RpcLogRecord {
      seq: Some(1),
      time_ms: Some(0),
      task: task.to_string(),
      kind,
    }
  }

  fn line(task: &str, text: &str) -> RpcLogRecord {
    record(
      task,
      RpcLogKind::Line {
        text: text.to_string(),
      },
    )
  }

  #[test]
  fn names_are_padded_to_the_widest() {
    let tasks = ["web".to_string(), "db".to_string()];
    let mut printer = Printer::new(true, false, false, &tasks);
    assert_eq!(printer.line(&line("db", "ready")), "db  | ready");
    assert_eq!(printer.line(&line("web", "up")), "web | up");
    // A task that joins later widens what follows.
    assert_eq!(printer.line(&line("worker", "go")), "worker | go");
    assert_eq!(printer.line(&line("db", "again")), "db     | again");
  }

  #[test]
  fn lifecycle_records_read_as_markers() {
    let mut printer = Printer::new(false, false, false, &[]);
    let exited = |code, signal| RpcLogKind::Exited { code, signal };
    let lines: Vec<String> = [
      RpcLogKind::Started,
      exited(Some(1), None),
      exited(None, Some(9)),
      exited(None, None),
      RpcLogKind::Dropped { count: 1 },
      RpcLogKind::Dropped { count: 120 },
    ]
    .into_iter()
    .map(|kind| printer.line(&record("web", kind)))
    .collect();
    assert_eq!(
      lines,
      [
        "--- started",
        "--- exited (code 1)",
        "--- exited (signal 9)",
        "--- exited",
        "--- 1 line dropped",
        "--- 120 lines dropped",
      ]
    );
  }

  #[test]
  fn colors_mark_the_name_and_dim_the_markers() {
    let mut printer = Printer::new(true, false, true, &["web".to_string()]);
    let color = name_color("web");
    assert_eq!(
      printer.line(&line("web", "up")),
      format!("\x1b[{color}mweb\x1b[0m | up")
    );
    assert_eq!(
      printer.line(&record("web", RpcLogKind::Started)),
      format!("\x1b[{color}mweb\x1b[0m | \x1b[2m--- started\x1b[0m")
    );
    assert_eq!(name_color("web"), color);
  }

  #[test]
  fn json_is_the_record() {
    let mut printer = Printer::new(true, true, true, &[]);
    assert_eq!(
      printer.line(&line("web", "up")),
      r#"{"seq":1,"time_ms":0,"task":"web","kind":"line","text":"up"}"#
    );
  }
}
