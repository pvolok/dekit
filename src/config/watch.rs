//! Watches what a config is read from and says when it changed: each
//! directory `Config::watched` names, a change counting when its path
//! matches one of the patterns. The system's notifications are used
//! where there are any, and polling where not.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use notify::event::Flag;
use notify::{
  Event, EventHandler, EventKind, PollWatcher, RecursiveMode, Watcher,
};
use tokio::sync::watch;

use super::config::Watched;

/// How often the files are looked at when nothing notifies of changes.
const POLL_EVERY: Duration = Duration::from_secs(2);

pub struct ConfigWatch {
  watcher: Box<dyn Watcher + Send>,
  /// Each directory watched, and whether deeper too.
  dirs: Vec<(PathBuf, bool)>,
  wanted: Arc<Mutex<Wanted>>,
  #[cfg(test)]
  seen: Arc<Mutex<Vec<PathBuf>>>,
}

/// What a change has to touch to count.
#[derive(Default)]
struct Wanted {
  patterns: Vec<glob::Pattern>,
  /// For each directory not there yet, its next part under the nearest
  /// one that is: that appearing counts, and the load it brings on
  /// watches further.
  appearing: Vec<PathBuf>,
}

impl Wanted {
  /// What is wanted, taken over from a thread that panicked holding it:
  /// it is set whole, so it is as the last `set` left it.
  fn lock(wanted: &Mutex<Wanted>) -> MutexGuard<'_, Wanted> {
    wanted.lock().unwrap_or_else(PoisonError::into_inner)
  }

  fn counts(&self, path: &Path) -> bool {
    self
      .patterns
      .iter()
      .any(|pattern| pattern.matches_path(path))
      || self.appearing.iter().any(|dir| dir == path)
  }
}

/// Runs on the watcher's thread.
#[derive(Clone)]
struct Changed {
  wanted: Arc<Mutex<Wanted>>,
  changes: Arc<watch::Sender<u64>>,
  /// Every path an event named, for a test to tell what has arrived.
  #[cfg(test)]
  seen: Arc<Mutex<Vec<PathBuf>>>,
}

impl EventHandler for Changed {
  fn handle_event(&mut self, event: notify::Result<Event>) {
    let event = match event {
      Ok(event) => event,
      Err(err) => return log::debug!("Config watch: {err}"),
    };
    #[cfg(test)]
    self
      .seen
      .lock()
      .unwrap()
      .extend(event.paths.iter().cloned());
    if matches!(event.kind, EventKind::Access(_)) {
      return;
    }
    let wanted = Wanted::lock(&self.wanted);
    // A rescan notice means events were lost, a change maybe among them.
    if event.attrs.flag() == Some(Flag::Rescan)
      || event.paths.iter().any(|path| wanted.counts(path))
    {
      self.changes.send_modify(|n| *n = n.wrapping_add(1));
    }
  }
}

impl ConfigWatch {
  /// Bumps `changes` on each change to what it is `set` to watch.
  pub fn new(changes: Arc<watch::Sender<u64>>) -> notify::Result<Self> {
    Self::with(changes, |handler| {
      match notify::recommended_watcher(handler.clone()) {
        Ok(watcher) => Ok(Box::new(watcher)),
        Err(err) => {
          log::warn!("Polling the config for changes: {err}");
          let config = notify::Config::default().with_poll_interval(POLL_EVERY);
          Ok(Box::new(PollWatcher::new(handler, config)?))
        }
      }
    })
  }

  /// `new`, but looking at the files every `every` whatever the system
  /// offers, and at their contents: a test can't wait on the system's
  /// notifications, which take seconds when its daemon is busy, and a
  /// write within the second of the last look leaves the modification
  /// time as it was.
  #[cfg(test)]
  fn polling(
    changes: Arc<watch::Sender<u64>>,
    every: Duration,
  ) -> notify::Result<Self> {
    Self::with(changes, |handler| {
      let config = notify::Config::default()
        .with_poll_interval(every)
        .with_compare_contents(true);
      Ok(Box::new(PollWatcher::new(handler, config)?))
    })
  }

  fn with(
    changes: Arc<watch::Sender<u64>>,
    watcher: impl FnOnce(Changed) -> notify::Result<Box<dyn Watcher + Send>>,
  ) -> notify::Result<Self> {
    let wanted = Arc::new(Mutex::new(Wanted::default()));
    #[cfg(test)]
    let seen = Arc::new(Mutex::new(Vec::new()));
    let watcher = watcher(Changed {
      wanted: wanted.clone(),
      changes,
      #[cfg(test)]
      seen: seen.clone(),
    })?;
    Ok(ConfigWatch {
      watcher,
      dirs: Vec::new(),
      wanted,
      #[cfg(test)]
      seen,
    })
  }

  /// Watches what a config is read from, and only that.
  pub fn set(&mut self, watched: &[Watched]) {
    let mut wanted = Wanted {
      patterns: watched
        .iter()
        .filter_map(|w| glob::Pattern::new(&w.pattern).ok())
        .collect(),
      appearing: Vec::new(),
    };
    let mut dirs: Vec<(PathBuf, bool)> = Vec::new();
    for w in watched {
      // A directory not there yet is waited for in the nearest one that
      // is, and no deeper.
      let Some(dir) = w.dir.ancestors().find(|dir| dir.is_dir()) else {
        continue;
      };
      if let Some(next) = w.dir.ancestors().take_while(|a| *a != dir).last() {
        wanted.appearing.push(next.to_path_buf());
      }
      let recursive = w.recursive && dir == w.dir;
      match dirs.iter_mut().find(|(d, _)| d == dir) {
        Some((_, deeper)) => *deeper |= recursive,
        None => dirs.push((dir.to_path_buf(), recursive)),
      }
    }
    *Wanted::lock(&self.wanted) = wanted;
    for (dir, _) in self.dirs.iter().filter(|old| !dirs.contains(old)) {
      let _ = self.watcher.unwatch(dir);
    }
    let old = std::mem::take(&mut self.dirs);
    for (dir, recursive) in dirs {
      let mode = if recursive {
        RecursiveMode::Recursive
      } else {
        RecursiveMode::NonRecursive
      };
      let watching = old.contains(&(dir.clone(), recursive))
        || match self.watcher.watch(&dir, mode) {
          Ok(()) => true,
          Err(err) => {
            log::warn!(
              "Can't watch {} for config changes: {err}",
              dir.display()
            );
            false
          }
        };
      if watching {
        self.dirs.push((dir, recursive));
      }
    }
  }
}

#[cfg(test)]
mod tests {
  use std::sync::atomic::{AtomicUsize, Ordering};
  use std::time::{Duration, Instant};

  use super::*;
  use crate::config::config::Config;
  use crate::runner::RunnerSpec;
  use crate::test_util::TempDir;

  /// How often the tests' watches look at the files.
  const POLL: Duration = Duration::from_millis(20);

  /// Whether a change is reported in time.
  async fn changed(rx: &mut watch::Receiver<u64>) -> bool {
    tokio::time::timeout(Duration::from_secs(10), rx.changed())
      .await
      .is_ok()
  }

  /// Waits until the watch has reported all it will of what was written
  /// so far: a file that does not count is written under `root`, and its
  /// event means a look since the write, which took in the earlier ones.
  async fn delivered(watch: &ConfigWatch, root: &Path) {
    static N: AtomicUsize = AtomicUsize::new(0);
    let root = dunce::canonicalize(root).unwrap();
    let marker =
      root.join(format!(".sync{}", N.fetch_add(1, Ordering::Relaxed)));
    std::fs::write(&marker, "").unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while !watch.seen.lock().unwrap().contains(&marker) {
      assert!(
        Instant::now() < deadline,
        "no event for {}",
        marker.display()
      );
      tokio::time::sleep(Duration::from_millis(10)).await;
    }
  }

  /// A change to a file the config is read from, or a new one its `load`
  /// pattern matches, is seen; one to any other file is not.
  #[tokio::test]
  async fn sees_changes_to_what_the_config_is_read_from() {
    let dir = TempDir::new("watch");
    let root = dir.0.clone();
    std::fs::create_dir_all(root.join("cfg")).unwrap();
    std::fs::write(
      root.join("dekit.yaml"),
      "load: ['cfg/*.yaml']\ntasks:\n  a: {cmd: ['true']}\n",
    )
    .unwrap();
    std::fs::write(root.join("cfg/b.yaml"), "tasks: {}\n").unwrap();
    let config = Config::load_dir(&root).unwrap();

    let (changes, mut rx) = watch::channel(0);
    let mut watch = ConfigWatch::polling(Arc::new(changes), POLL).unwrap();
    watch.set(&config.watched);
    // Whatever the setup itself stirred up.
    delivered(&watch, &root).await;
    rx.mark_unchanged();

    std::fs::write(root.join("notes.txt"), "x").unwrap();
    std::fs::write(root.join("cfg/notes.txt"), "x").unwrap();
    delivered(&watch, &root).await;
    assert!(!rx.has_changed().unwrap(), "not what the config reads");

    std::fs::write(root.join("cfg/c.yaml"), "tasks: {}\n").unwrap();
    assert!(changed(&mut rx).await, "a new fragment");
    delivered(&watch, &root).await;
    rx.mark_unchanged();
    // As editors save: a new file renamed over the old one.
    std::fs::write(root.join(".dekit.yaml.tmp"), "tasks: {}\n").unwrap();
    std::fs::rename(root.join(".dekit.yaml.tmp"), root.join("dekit.yaml"))
      .unwrap();
    assert!(changed(&mut rx).await, "dekit.yaml saved");

    drop(watch);
  }

  /// A `load` directory not there yet is waited for in its parent, which
  /// is watched no deeper; once there, it is watched itself.
  #[tokio::test]
  async fn waits_for_a_directory_not_there_yet() {
    let dir = TempDir::new("watch-later");
    std::fs::write(dir.0.join("dekit.yaml"), "load: ['gen/*.yaml']\n").unwrap();
    let root = dunce::canonicalize(&dir.0).unwrap();
    let watched = |root: &Path| match Config::try_load(&RunnerSpec {
      kind: crate::runner::RunnerKind::Project,
      root: root.to_path_buf(),
    }) {
      Ok(config) => config.watched,
      Err((_, watched)) => watched,
    };

    let (changes, mut rx) = watch::channel(0);
    let mut watch = ConfigWatch::polling(Arc::new(changes), POLL).unwrap();
    watch.set(&watched(&root));
    // The project root, besides the user config's directory.
    assert!(
      watch.dirs.contains(&(root.clone(), false)),
      "{:?}",
      watch.dirs
    );
    assert!(!watch.dirs.iter().any(|(dir, _)| *dir == root.join("gen")));
    delivered(&watch, &root).await;
    rx.mark_unchanged();

    std::fs::create_dir(root.join("gen")).unwrap();
    assert!(changed(&mut rx).await, "gen made");
    watch.set(&watched(&root));
    assert!(watch.dirs.contains(&(root.join("gen"), false)));
    delivered(&watch, &root).await;
    rx.mark_unchanged();
    std::fs::write(root.join("gen/a.yaml"), "tasks: {}\n").unwrap();
    assert!(changed(&mut rx).await, "a fragment in it");

    drop(watch);
  }
}
