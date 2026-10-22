//! The project a runner runs: its config as last loaded, the base a
//! reload and a write-back compare the config on disk and the tasks
//! with, and a watch on the files the config on disk is read from.

use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use tokio::sync::watch;

use crate::config::{config::Config, watch::ConfigWatch};
use crate::spec::{Base, Source, TaskSpec};
use crate::task::config_tasks::config_base;

pub struct Project {
  state: Mutex<State>,
  /// Bumped when a file the config is read from changes, once watched,
  /// and when the base does: either way the config and the tasks compare
  /// otherwise.
  changes: Arc<watch::Sender<u64>>,
  watch: Mutex<Option<ConfigWatch>>,
  /// Held while the tasks and the base are read or brought in line with
  /// the config (`syncing`).
  syncing: tokio::sync::Mutex<()>,
}

struct State {
  config: Arc<Config>,
  base: Base,
}

impl Project {
  /// A project whose config was just loaded, its tasks all from it.
  pub fn new(config: Arc<Config>) -> Arc<Self> {
    let base = config_base(&config);
    Arc::new(Project {
      state: Mutex::new(State { config, base }),
      changes: Arc::new(watch::channel(0).0),
      watch: Mutex::new(None),
      syncing: tokio::sync::Mutex::new(()),
    })
  }

  /// Held by a reload or a write-back carried out, and by what reads the
  /// tasks with the base: one at a time, each against what the last one
  /// left.
  pub async fn syncing(&self) -> tokio::sync::MutexGuard<'_, ()> {
    self.syncing.lock().await
  }

  /// Sees a change each time a file the config on disk is read from
  /// changes, or the base does; the files are watched from the first
  /// call on.
  pub fn changes(&self) -> watch::Receiver<u64> {
    let mut watch = self.watch();
    if watch.is_none() {
      match ConfigWatch::new(self.changes.clone()) {
        Ok(mut started) => {
          started.set(&self.config().watched);
          *watch = Some(started);
        }
        Err(err) => log::warn!("Can't watch the config for changes: {err}"),
      }
    }
    self.changes.subscribe()
  }

  /// `load` off the async workers: it reads and parses files and walks
  /// the globs of `load`.
  pub async fn load_blocking(self: &Arc<Self>) -> anyhow::Result<Config> {
    let project = self.clone();
    tokio::task::spawn_blocking(move || project.load()).await?
  }

  /// The state, taken over from a thread that panicked holding it: it is
  /// read and written whole, so it is as the last writer left it.
  fn state(&self) -> MutexGuard<'_, State> {
    self.state.lock().unwrap_or_else(PoisonError::into_inner)
  }

  /// The watch, taken over the same way.
  fn watch(&self) -> MutexGuard<'_, Option<ConfigWatch>> {
    self.watch.lock().unwrap_or_else(PoisonError::into_inner)
  }

  pub fn config(&self) -> Arc<Config> {
    self.state().config.clone()
  }

  pub fn base(&self) -> Base {
    self.state().base.clone()
  }

  /// The base of tasks restored from a save (`saved_base`).
  pub fn set_base(&self, base: Base) {
    self.state().base = base;
    self.changed();
  }

  fn changed(&self) {
    self.changes.send_modify(|n| *n = n.wrapping_add(1));
  }

  /// Entries written into `config`, the config on disk, as they read
  /// back: each says what it says now, or is gone from it. A new one goes
  /// where `config` has it.
  pub fn record(
    &self,
    config: &Config,
    entries: Vec<(Source, Option<TaskSpec>)>,
  ) {
    let order = config.sources();
    let mut state = self.state();
    for (source, spec) in entries {
      match spec {
        Some(spec) => state.base.set(source, spec, &order),
        None => state.base.remove(&source),
      }
    }
    drop(state);
    self.changed();
  }

  /// Loads the config on disk as the runner loaded it, with the
  /// runner's identity. The watch follows what it was read from, as far
  /// as that got when it does not load.
  pub fn load(&self) -> anyhow::Result<Config> {
    let current = self.config();
    let Some(runner) = current.runner.clone() else {
      anyhow::bail!("this runner has no project to reload");
    };
    let loaded = Config::try_load_dir(&runner.root);
    let mut watch = self.watch();
    if let Some(watch) = watch.as_mut() {
      watch.set(match &loaded {
        Ok(config) => &config.watched,
        Err((_, watched)) => watched,
      });
    }
    let mut config = loaded.map_err(|(err, _)| err)?;
    config.runner = Some(runner);
    Ok(config)
  }

  /// The config a reload carried out, and the base it leaves.
  pub fn reloaded(&self, config: Arc<Config>, base: Base) {
    let mut state = self.state();
    state.base = base;
    state.config = config;
    drop(state);
    self.changed();
  }
}

/// `path` from `dir`: `.` for `dir` itself, and `path` as it is when it
/// is not under `dir`.
pub fn relative_path(path: &Path, dir: &Path) -> String {
  match path.strip_prefix(dir) {
    Ok(rest) if rest.as_os_str().is_empty() => ".".to_string(),
    Ok(rest) => crate::spec::written_path(rest),
    Err(_) => crate::spec::written_path(path),
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::config::task::TaskConfig;
  use crate::test_util::source;

  fn config(keys: &[&str]) -> Config {
    let mut config = Config::make_default();
    config.tasks = keys
      .iter()
      .map(|key| TaskConfig {
        path: (*key).into(),
        source: Some(source(key)),
        ..TaskConfig::default()
      })
      .collect();
    config
  }

  fn keys(project: &Project) -> Vec<String> {
    project.base().sources().map(|s| s.key.clone()).collect()
  }

  /// An entry written takes its place in the base where the config has
  /// it; one renamed or removed leaves it, and the rest stay as they are.
  #[test]
  fn an_entry_written_takes_its_place_in_the_base() {
    let project = Project::new(Arc::new(config(&["api", "db", "old"])));
    // `api` renamed in the file, and a task of the runner's own saved
    // first.
    let now = config(&["mine", "web/api", "db"]);
    let spec = Some(TaskSpec::with_cmd(vec!["api".into()]));
    project
      .record(&now, vec![(source("web/api"), spec), (source("api"), None)]);
    assert_eq!(keys(&project), ["web/api", "db", "old"]);
    project.record(&now, vec![(source("mine"), Some(TaskSpec::default()))]);
    assert_eq!(keys(&project), ["mine", "web/api", "db", "old"]);
    let base = project.base();
    assert_eq!(
      base.get(&source("web/api")).and_then(TaskSpec::argv),
      Some(vec!["api".into()])
    );
  }
}
