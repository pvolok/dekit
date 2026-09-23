use std::path::PathBuf;

use bytes::Bytes;
use tokio::io::AsyncWriteExt;
use tokio::sync::mpsc::{self, Receiver, Sender};

use crate::error::ResultLogger;

const CHANNEL_CAP: usize = 256;

pub struct LogSink {
  pub path: PathBuf,
  pub append: bool,
}

/// Where a task's output is logged; resolved per spawn because the path
/// template may name the pid.
#[derive(Clone, Debug)]
pub struct LogSpec {
  pub config: crate::config::task_log::TaskLogConfig,
  pub name: String,
}

impl LogSpec {
  pub fn resolve(&self, task_id: usize, pid: u32) -> Option<LogSink> {
    self
      .config
      .file_path(&self.name, task_id, pid)
      .map(|path| LogSink {
        path,
        append: self.config.mode() == crate::config::task_log::LogMode::Append,
      })
  }
}

pub enum LogMsg {
  Bytes(Bytes),
  /// Answered once everything queued before it is on disk.
  Flush(tokio::sync::oneshot::Sender<()>),
}

pub fn spawn_logger(sink: LogSink) -> Sender<LogMsg> {
  let (tx, rx) = mpsc::channel(CHANNEL_CAP);
  tokio::spawn(logger_main(rx, sink));
  tx
}

async fn logger_main(mut rx: Receiver<LogMsg>, sink: LogSink) {
  let mut file = match open_log(&sink).await {
    Some(file) => file,
    None => return,
  };
  while let Some(msg) = rx.recv().await {
    match msg {
      LogMsg::Bytes(bytes) => file.write_all(&bytes).await.log_ignore(),
      LogMsg::Flush(done) => {
        file.flush().await.log_ignore();
        let _ = done.send(());
      }
    }
  }
}

async fn open_log(sink: &LogSink) -> Option<tokio::fs::File> {
  if let Some(parent) = sink.path.parent() {
    tokio::fs::create_dir_all(parent).await.log_ignore();
  }
  let mut options = tokio::fs::OpenOptions::new();
  options.create(true).write(true).append(sink.append);
  if !sink.append {
    options.truncate(true);
  }
  options
    .open(&sink.path)
    .await
    .map_err(|e| log::warn!("Failed to open log file {:?}: {}", sink.path, e))
    .ok()
}
