//! The `logs` request. Without `follow` it is answered like any other
//! request. With it, like a wait, it is the last request on its
//! connection: the ok response carries the backlog, then each batch of
//! new records goes out as a `logs` event, until the client leaves or the
//! runner ends the session with a bye.

use std::{
  collections::HashMap,
  sync::Arc,
  time::{Duration, SystemTime, UNIX_EPOCH},
};

use serde_json::Value;
use tokio::sync::mpsc::UnboundedReceiver;

use crate::{
  dekit::server::{ConnCtl, ServerCtx, SessionConn, carried, event, finish},
  kernel::{
    kernel_message::{SetTask, TaskContext},
    logs::{Item, Record, RecordKind, Tail},
    task::TaskId,
  },
  protocol::{
    Bye, CtlMsg, RpcError, codes,
    rpc::{EVENT_LOGS, LogsResult, RpcLogKind, RpcLogRecord},
  },
  target::Target,
  upgrade::snapshot as snap,
};

/// What a `logs` request asked for, `follow` aside.
pub struct LogsParams {
  pub targets: Vec<Target>,
  pub lines: Option<usize>,
  pub since_ms: Option<u64>,
}

impl LogsParams {
  fn lines(&self) -> usize {
    self.lines.unwrap_or(usize::MAX)
  }

  fn since(&self) -> Option<SystemTime> {
    let ago = Duration::from_millis(self.since_ms?);
    SystemTime::now().checked_sub(ago)
  }
}

/// The tasks a request reads, with their names.
struct Set {
  ids: Vec<TaskId>,
  names: HashMap<TaskId, String>,
}

impl Set {
  fn new(tasks: Vec<SetTask>) -> Self {
    let ids = tasks.iter().map(|task| task.id).collect();
    let names = tasks.into_iter().map(|task| (task.id, task.name)).collect();
    Set { ids, names }
  }

  /// The names, in list order.
  fn listed(&self) -> Vec<String> {
    self.ids.iter().map(|id| self.name(*id)).collect()
  }

  /// Records are only read for the set's tasks: each has a name.
  fn name(&self, id: TaskId) -> String {
    self.names.get(&id).cloned().unwrap_or_default()
  }

  /// The tail's records, then, with `open`, its lines under way.
  fn result(&self, tail: &Tail, open: bool) -> Value {
    let open =
      tail
        .open
        .iter()
        .filter(|_| open)
        .map(|(id, text)| RpcLogRecord {
          seq: None,
          time_ms: None,
          task: self.name(*id),
          kind: RpcLogKind::Open { text: text.clone() },
        });
    let records = tail.records.iter().map(|record| self.record(record));
    let result = LogsResult {
      tasks: self.listed(),
      records: records.chain(open).collect(),
    };
    serde_json::to_value(result).expect("records serialize")
  }

  fn record(&self, record: &Record) -> RpcLogRecord {
    let kind = match &record.kind {
      RecordKind::Line(text) => RpcLogKind::Line { text: text.clone() },
      RecordKind::Started => RpcLogKind::Started,
      RecordKind::Exited(info) => RpcLogKind::Exited {
        code: info.code,
        signal: info.signal,
      },
    };
    let time = record.time.duration_since(UNIX_EPOCH).ok();
    RpcLogRecord {
      seq: Some(record.seq),
      time_ms: time.map(|time| time.as_millis() as u64),
      task: self.name(record.task),
      kind,
    }
  }

  fn item(&self, item: &Item) -> RpcLogRecord {
    match item {
      Item::Record(record) => self.record(record),
      Item::Dropped { task, count } => RpcLogRecord {
        seq: None,
        time_ms: None,
        task: self.name(*task),
        kind: RpcLogKind::Dropped { count: *count },
      },
    }
  }
}

/// The tasks the targets match now, and the watch that reports them again
/// as that changes. No match is an error, as for a wait.
async fn begin(
  pc: &TaskContext,
  params: &LogsParams,
) -> Result<(UnboundedReceiver<Vec<SetTask>>, Set), RpcError> {
  let all = [Target::glob("**")];
  let targets = match &params.targets[..] {
    [] => &all[..],
    targets => targets,
  };
  let selectors = targets
    .iter()
    .map(|target| {
      target
        .selector()
        .map_err(|err| RpcError::new(codes::BAD_TARGET, err.to_string()))
    })
    .collect::<Result<Vec<_>, _>>()?;
  let mut watch = pc.watch_set(selectors);
  let Some(tasks) = watch.recv().await else {
    return Err(RpcError::new(
      codes::INTERNAL,
      "the runner is shutting down",
    ));
  };
  if tasks.is_empty() {
    let named: Vec<String> = targets.iter().map(Target::to_string).collect();
    return Err(RpcError::new(
      codes::NO_MATCH,
      format!("no task matches '{}'", named.join(" ")),
    ));
  }
  Ok((watch, Set::new(tasks)))
}

/// `logs` without `follow`: the last records, then the lines under way.
pub async fn logs(
  pc: &TaskContext,
  params: LogsParams,
) -> Result<Value, RpcError> {
  let (_, set) = begin(pc, &params).await?;
  let tail = pc.logs().tail(&set.ids, params.lines(), params.since());
  Ok(set.result(&tail, params.lines != Some(0)))
}

/// `logs` with `follow`.
pub async fn logs_session(
  ctx: &Arc<ServerCtx>,
  conn: SessionConn,
  request_id: u64,
  params: LogsParams,
) {
  let SessionConn {
    mut reg,
    fd,
    hello,
    mut sender,
    mut receiver,
  } = conn;
  let store = ctx.pc.logs().clone();
  // Before the backlog is read, so nothing logged after it goes unseen.
  let mut logged = store.subscribe();
  let (mut watch, mut set) = match begin(&ctx.pc, &params).await {
    Ok(begun) => begun,
    Err(error) => {
      if sender.queue_ctl(CtlMsg::err(request_id, error)).is_ok() {
        finish(
          &mut sender,
          &mut reg.ctl,
          fd,
          Some(&hello),
          receiver.buffered(),
        )
        .await;
      }
      return;
    }
  };
  let tail = store.tail(&set.ids, params.lines(), params.since());
  // The lines under way come in the records once they are done.
  let backlog = set.result(&tail, false);
  let mut cursor = tail.cursor;
  if sender.queue_ctl(CtlMsg::ok(request_id, backlog)).is_err() {
    return;
  }

  let stopping = || Bye::new(codes::QUIT, "the runner is stopping");
  let mut frozen = false;
  let bye = loop {
    tokio::select! {
      tasks = watch.recv(), if !frozen => match tasks {
        Some(tasks) => set = Set::new(tasks),
        None => break Some(stopping()),
      },
      // Read once the last batch is written: a client that reads slowly
      // costs nothing, and one left behind hears what it missed.
      changed = logged.changed(), if !frozen && sender.pending().is_empty() => {
        if changed.is_err() {
          break Some(stopping());
        }
        let items = store.read(&set.ids, &mut cursor);
        if items.is_empty() {
          continue;
        }
        let batch = LogsResult {
          tasks: Vec::new(),
          records: items.iter().map(|item| set.item(item)).collect(),
        };
        if sender.queue_ctl(event(EVENT_LOGS, &batch)).is_err() {
          return;
        }
      }
      ctl = reg.ctl.recv() => match ctl {
        Some(ConnCtl::Freeze(reply)) => {
          // The log store doesn't survive an upgrade: the session ends
          // with a bye, which the next image writes, on a connection it
          // takes as a plain one.
          let bye = Bye::new(codes::UPGRADE, "the runner is being upgraded");
          if sender.queue_ctl(CtlMsg::Bye(bye)).is_err() {
            return;
          }
          frozen = true;
          let kind = snap::ConnectionKind::Rpc { pending_upgrade: None };
          let _ = reply.send(carried(
            fd,
            Some(&hello),
            receiver.buffered(),
            &sender,
            kind,
          ));
        }
        // The upgrade failed: the bye is on its way already.
        Some(ConnCtl::Thaw) => break None,
        Some(ConnCtl::Close) | None => break Some(stopping()),
      },
      written = sender.flush(), if !frozen && !sender.pending().is_empty() => {
        if written.is_err() {
          return;
        }
      }
      // Read only to see the client go.
      msg = receiver.recv_ctl(), if !frozen => {
        if msg.is_err() {
          return;
        }
      }
    }
  };
  drop(watch);
  if let Some(bye) = bye
    && sender.queue_ctl(CtlMsg::Bye(bye)).is_err()
  {
    return;
  }
  finish(
    &mut sender,
    &mut reg.ctl,
    fd,
    Some(&hello),
    receiver.buffered(),
  )
  .await;
}
