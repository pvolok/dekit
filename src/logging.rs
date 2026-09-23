use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::Mutex;

use anyhow::Result;
pub use flexi_logger::LoggerHandle;
use flexi_logger::writers::LogWriter;
use flexi_logger::{DeferredNow, FormatFunction, Logger};
use log::{LevelFilter, Record};

pub struct Config<'a> {
  /// Binary name, used for the default file basename.
  pub binary: &'a str,
  /// `--log-level` value passed on the CLI, if any.
  pub cli_level: Option<&'a str>,
  /// Env var consulted for the log spec.
  pub log_env: &'a str,
  /// Env var consulted for the log file path.
  pub file_env: &'a str,
  pub config_level: Option<&'a str>,
  pub config_file: Option<&'a Path>,
  /// Working directory used as the default file location. `None` means CWD.
  pub default_dir: Option<&'a Path>,
}

pub fn init(cfg: Config<'_>) -> Result<Option<LoggerHandle>> {
  let spec = cfg
    .cli_level
    .map(str::to_string)
    .or_else(|| std::env::var(cfg.log_env).ok())
    .or_else(|| std::env::var("RUST_LOG").ok())
    .or_else(|| cfg.config_level.map(str::to_string))
    .unwrap_or_else(default_spec);

  if spec.eq_ignore_ascii_case("off") {
    return Ok(None);
  }

  let file = std::env::var_os(cfg.file_env)
    .map(PathBuf::from)
    .or_else(|| cfg.config_file.map(Path::to_path_buf))
    .unwrap_or_else(|| {
      let mut p = cfg
        .default_dir
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
      p.push(format!("{}.log", cfg.binary));
      p
    });

  let max_level = LevelFilter::from_str(&spec).unwrap_or(LevelFilter::Trace);
  let writer = Box::new(LazyFileWriter::new(file, max_level));

  let handle = Logger::try_with_str(&spec)?
    .log_to_writer(writer)
    .use_utc()
    .start()?;

  install_panic_hook();

  Ok(Some(handle))
}

fn default_spec() -> String {
  if cfg!(debug_assertions) {
    "trace".to_string()
  } else {
    "error".to_string()
  }
}

fn install_panic_hook() {
  let prev = std::panic::take_hook();
  std::panic::set_hook(Box::new(move |info| {
    let stacktrace = std::backtrace::Backtrace::capture();
    log::error!("Got panic. @info:{}\n@stackTrace:{}", info, stacktrace);
    prev(info);
  }));
}

/// File-backed `LogWriter` that opens the file on the first write.
struct LazyFileWriter {
  path: PathBuf,
  inner: Mutex<Inner>,
  max_level: LevelFilter,
  format: Mutex<FormatFunction>,
}

struct Inner {
  file: Option<File>,
  /// `true` once we've tried to open the file and failed; we don't keep
  /// retrying forever because each write would otherwise spam stderr.
  open_failed: bool,
}

impl LazyFileWriter {
  fn new(path: PathBuf, max_level: LevelFilter) -> Self {
    Self {
      path,
      inner: Mutex::new(Inner {
        file: None,
        open_failed: false,
      }),
      max_level,
      format: Mutex::new(flexi_logger::default_format),
    }
  }

  fn open(&self) -> std::io::Result<File> {
    if let Some(parent) = self.path.parent() {
      if !parent.as_os_str().is_empty() {
        std::fs::create_dir_all(parent)?;
      }
    }
    OpenOptions::new()
      .create(true)
      .append(true)
      .open(&self.path)
  }
}

impl LogWriter for LazyFileWriter {
  fn write(
    &self,
    now: &mut DeferredNow,
    record: &Record,
  ) -> std::io::Result<()> {
    let mut inner = self.inner.lock().unwrap();
    if inner.file.is_none() {
      if inner.open_failed {
        return Ok(());
      }
      match self.open() {
        Ok(f) => inner.file = Some(f),
        Err(err) => {
          inner.open_failed = true;
          eprintln!("Failed to open log file {}: {}", self.path.display(), err);
          return Ok(());
        }
      }
    }
    let format = *self.format.lock().unwrap();
    let file = inner.file.as_mut().unwrap();
    format(file, now, record)?;
    file.write_all(b"\n")
  }

  fn flush(&self) -> std::io::Result<()> {
    let mut inner = self.inner.lock().unwrap();
    if let Some(file) = inner.file.as_mut() {
      file.flush()
    } else {
      Ok(())
    }
  }

  fn max_log_level(&self) -> LevelFilter {
    self.max_level
  }

  fn format(&mut self, format: FormatFunction) {
    *self.format.lock().unwrap() = format;
  }
}
