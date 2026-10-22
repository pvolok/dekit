//! Writing back to the config what changed here: each config task's
//! definition against what the config on disk says for it, the tasks
//! removed here, and the runner's own tasks asked to be saved. The writer
//! edits the files in place; what it can't edit safely goes to an agent,
//! as a prompt. Either way, each entry that then reads back as written is
//! recorded in the base (`written`).

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_yaml::{Mapping, Value};

use crate::command::{Command, CommandResult, execute};
use crate::config::config::{Config, Entry};
use crate::config::writer::{self, Edit};
use crate::kernel::kernel_message::{
  FolderSettings, KernelCommand, TaskContext,
};
use crate::kernel::task::TaskId;
use crate::project::{Project, relative_path};
use crate::reload::Live;
use crate::runner::atomic_write;
use crate::spec::{Base, Source, TaskSpec, show_value};
use crate::target::Target;
use crate::task::config_tasks::{config_base, config_specs, dep_indexes};

#[derive(Clone, Debug)]
pub struct WritePlan {
  pub items: Vec<WriteItem>,
  root: PathBuf,
}

#[derive(Clone, Debug)]
pub struct WriteItem {
  /// The task's path here, or in the config for one removed here; a
  /// folder's ends in `/`.
  pub path: String,
  /// The entry as the file has it: its file, from the project root, and
  /// its key there.
  pub source: Source,
  pub kind: WriteKind,
  pub fields: Vec<WriteField>,
  pub apply: bool,
  /// Why the writer can't do it; an agent can.
  pub note: Option<String>,
  edits: Vec<Edit>,
  pub task: Option<TaskId>,
  /// The folder moved here, as it is.
  folder: Option<(TaskId, FolderSettings)>,
  /// The entry once written: under the task's path for one moved here.
  entry: Source,
  /// What the entry says once written: what the task has; none for a
  /// removal.
  spec: Option<TaskSpec>,
}

/// What a write changes in a task's entry.
#[derive(Clone, Debug)]
pub struct WriteField {
  /// A spec key, `env.NAME`, or `path`.
  pub key: String,
  /// What the file says.
  pub was: Option<Value>,
  /// What it will say; nothing removes it.
  pub now: Option<Value>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WriteKind {
  Changed,
  /// The runner's own task, new in the config.
  Added,
  /// Removed here: removed from the config.
  Removed,
}

/// How to write a plan back.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum How {
  /// Edit the files here.
  Write,
  /// Run the agent with a prompt.
  Agent,
  /// Copy the prompt.
  Copy,
}

impl WritePlan {
  pub fn chosen(&self) -> impl Iterator<Item = &WriteItem> {
    self.items.iter().filter(|item| item.apply)
  }
}

impl WriteItem {
  /// The task or folder here that it writes.
  pub fn here(&self) -> Option<TaskId> {
    self.task.or(self.folder.as_ref().map(|(id, _)| *id))
  }
}

/// What writing back would change in the config: every config task
/// changed here, every one removed here, and the runner's own `include`d.
/// With any included, only those are taken unless chosen.
pub fn plan(
  config: &Config,
  root: &Path,
  base: &Base,
  live: &Live,
  include: &[TaskId],
) -> WritePlan {
  let specs = config_specs(config);
  let defaults = TaskSpec::from_config(&config.defaults);
  let mut items = Vec::new();
  let tasks = live.tasks.iter().filter_map(|task| task.origin.as_ref());
  let folders = live.folders.iter();
  let here: HashSet<&Source> = tasks
    .chain(folders.filter_map(|folder| folder.settings.origin.as_ref()))
    .collect();
  for task in &live.tasks {
    let (Some(spec), Some(path)) = (&task.spec, &task.info.path) else {
      continue;
    };
    let path = path.to_string();
    match &task.origin {
      Some(source) => {
        let Some((cfg, config_spec)) = specs
          .iter()
          .find(|(cfg, _)| cfg.source.as_ref() == Some(source))
        else {
          continue;
        };
        // An entry the base lacks is as the config says it.
        let was = base.get(source).unwrap_or(config_spec);
        let dir = file_dir(root, &source.file);
        let mut edits = Vec::new();
        let mut fields = Vec::new();
        // An entry with no `env` of its own has all of `defaults`', which
        // one it sets replaces: a change to it is written whole.
        let whole_env = cfg.env.is_none() && config.defaults.env.is_some();
        // What the entry says itself, `defaults` left out.
        let own = TaskSpec::from_config(cfg);
        // Only what changed here: what the config changed is a reload's.
        for key in config_spec.changed_keys(spec) {
          let here = spec.value_at(&key).cloned();
          if spec.value_at(&key) == was.value_at(&key) {
            continue;
          }
          fields.push(WriteField {
            key: key.clone(),
            was: config_spec.value_at(&key).cloned(),
            now: here.clone(),
          });
          if whole_env && key.starts_with("env.") {
            continue;
          }
          let value = here
            .and_then(|value| {
              inherited_out(&key, value, own.get(&key), defaults.get(&key))
            })
            .map(|value| to_file(&key, value, &dir, root, &source.mount));
          edits.push(Edit::Set {
            task: source.key.clone(),
            field: key,
            value,
          });
        }
        if whole_env && fields.iter().any(|f| f.key.starts_with("env.")) {
          edits.push(Edit::Set {
            task: source.key.clone(),
            field: "env".into(),
            value: Some(env_of(spec)),
          });
        }
        let mut note = None;
        if path != cfg.path {
          fields.push(WriteField {
            key: "path".into(),
            was: Some(cfg.path.as_str().into()),
            now: Some(path.as_str().into()),
          });
          match key_in(&source.mount, &path) {
            Some(key) => edits.push(Edit::RenameTask {
              from: source.key.clone(),
              to: key,
            }),
            None => {
              note = Some(format!("moves out of {}'s mount", source.file))
            }
          }
        }
        if fields.is_empty() {
          continue;
        }
        let entry = Source {
          key: key_in(&source.mount, &path)
            .unwrap_or_else(|| source.key.clone()),
          ..source.clone()
        };
        items.push(WriteItem {
          path,
          source: source.clone(),
          kind: WriteKind::Changed,
          fields,
          apply: include.is_empty() || include.contains(&task.info.id),
          note,
          edits,
          task: Some(task.info.id),
          folder: None,
          entry,
          spec: Some(spec.clone()),
        });
      }
      None if include.contains(&task.info.id) => {
        let file = "dekit.yaml".to_string();
        let dir = file_dir(root, &file);
        let mut entry = Mapping::new();
        let mut fields = Vec::new();
        for (key, value) in &spec.0 {
          let Some(key) = key.as_str() else { continue };
          // What `defaults` gives it already need not be written.
          if defaults.get(key) == Some(value) {
            continue;
          }
          fields.push(WriteField {
            key: key.to_string(),
            was: None,
            now: Some(value.clone()),
          });
          entry.insert(
            Value::from(key),
            to_file(key, value.clone(), &dir, root, ""),
          );
        }
        // Without one, it would have `defaults`'.
        if spec.get("env").is_none() && defaults.get("env").is_some() {
          fields.push(WriteField {
            key: "env".into(),
            was: None,
            now: Some(env_of(spec)),
          });
          entry.insert(Value::from("env"), env_of(spec));
        }
        let source = Source {
          file,
          key: path.clone(),
          mount: String::new(),
        };
        items.push(WriteItem {
          path: path.clone(),
          source: source.clone(),
          kind: WriteKind::Added,
          fields,
          apply: true,
          note: None,
          edits: vec![Edit::AddTask {
            task: path.clone(),
            entry,
          }],
          task: Some(task.info.id),
          folder: None,
          entry: source,
          spec: Some(spec.clone()),
        });
      }
      None => (),
    }
  }
  // Moved here.
  for folder in &live.folders {
    let Some(source) = &folder.settings.origin else {
      continue;
    };
    let Some(entry) = config
      .folders
      .iter()
      .find(|entry| entry.source.as_ref() == Some(source))
    else {
      continue;
    };
    let path = folder.path.to_string();
    if path == entry.path {
      continue;
    }
    let key = key_in(&source.mount, &path).map(|key| format!("{key}/"));
    let note = key
      .is_none()
      .then(|| format!("moves out of {}'s mount", source.file));
    items.push(WriteItem {
      path: format!("{path}/"),
      source: source.clone(),
      kind: WriteKind::Changed,
      fields: vec![WriteField {
        key: "path".into(),
        was: Some(format!("{}/", entry.path).into()),
        now: Some(format!("{path}/").into()),
      }],
      apply: include.is_empty(),
      note,
      edits: key
        .iter()
        .map(|key| Edit::RenameTask {
          from: source.key.clone(),
          to: key.clone(),
        })
        .collect(),
      task: None,
      folder: Some((folder.id, folder.settings.clone())),
      entry: Source {
        key: key.unwrap_or_else(|| source.key.clone()),
        ..source.clone()
      },
      spec: Some(entry.spec()),
    });
  }
  // Removed here, still in the config.
  for entry in config.entries() {
    let Some(source) = entry.source() else {
      continue;
    };
    if here.contains(source) || !base.contains(source) {
      continue;
    }
    let path = match entry {
      Entry::Task(cfg) => cfg.path.clone(),
      Entry::Folder(folder) => format!("{}/", folder.path),
    };
    items.push(WriteItem {
      path,
      source: source.clone(),
      kind: WriteKind::Removed,
      fields: Vec::new(),
      apply: include.is_empty(),
      note: None,
      edits: vec![Edit::RemoveTask {
        task: source.key.clone(),
      }],
      task: None,
      folder: None,
      entry: source.clone(),
      spec: None,
    });
  }
  WritePlan {
    items,
    root: root.to_path_buf(),
  }
}

/// `path` from `dir`, up with `..` while in `root`: a path outside the
/// project is written whole, as no other machine has it relative to the
/// project.
fn path_in_file(path: &Path, dir: &Path, root: &Path) -> String {
  let (Ok(path), Ok(dir)) = (path.strip_prefix(root), dir.strip_prefix(root))
  else {
    return relative_path(path, dir);
  };
  let common = std::iter::zip(path.components(), dir.components())
    .take_while(|(a, b)| a == b)
    .count();
  let mut written = PathBuf::new();
  for _ in common..dir.components().count() {
    written.push("..");
  }
  written.extend(path.components().skip(common));
  if written.as_os_str().is_empty() {
    ".".to_string()
  } else {
    crate::spec::written_path(&written)
  }
}

fn file_dir(root: &Path, file: &str) -> PathBuf {
  root
    .join(file)
    .parent()
    .map_or_else(|| root.to_path_buf(), Path::to_path_buf)
}

/// A value as an entry writes it over `defaults`: of `stop` and `log`,
/// which take each of their parts from `defaults` unless the entry has
/// it, the parts it would take from them anyway are left out, unless the
/// entry says them itself; with none left, the key is left out.
fn inherited_out(
  key: &str,
  value: Value,
  own: Option<&Value>,
  defaults: Option<&Value>,
) -> Option<Value> {
  let parts: &[&[&str]] = match key {
    // How to stop is one part, however it is said.
    "stop" => &[&["signal", "group", "keys", "cmd"], &["timeout"]],
    "log" => &[&["enabled"], &["dir"], &["file"], &["mode"]],
    _ => return Some(value),
  };
  let (Value::Mapping(mut map), Some(Value::Mapping(defaults))) =
    (value.clone(), defaults)
  else {
    return Some(value);
  };
  let own = own.and_then(Value::as_mapping);
  for part in parts {
    let inherited = part.iter().all(|k| map.get(*k) == defaults.get(*k));
    let said = own.is_some_and(|own| part.iter().any(|k| own.contains_key(*k)));
    if inherited && !said {
      for k in *part {
        map.remove(*k);
      }
    }
  }
  (!map.is_empty()).then_some(Value::Mapping(map))
}

/// A task's key in a file mounted at `mount`, if the path is under it.
fn key_in(mount: &str, path: &str) -> Option<String> {
  if mount.is_empty() {
    return Some(path.to_string());
  }
  path
    .strip_prefix(mount)
    .and_then(|rest| rest.strip_prefix('/'))
    .map(String::from)
}

/// A task's whole `env`, `{}` for none.
fn env_of(spec: &TaskSpec) -> Value {
  spec
    .get("env")
    .cloned()
    .unwrap_or_else(|| Value::Mapping(Mapping::new()))
}

/// A spec value as the file writes it: paths relative to the file's
/// directory, `dir`, as far up as `root`, the project's; deps relative to
/// its mount.
fn to_file(
  key: &str,
  value: Value,
  dir: &Path,
  root: &Path,
  mount: &str,
) -> Value {
  let relative = |value: &Value| -> Value {
    match value.as_str() {
      Some(path) => Value::String(path_in_file(Path::new(path), dir, root)),
      None => value.clone(),
    }
  };
  match key.split('.').next().unwrap_or(key) {
    "cwd" | "script" => relative(&value),
    "add_path" => match &value {
      Value::Sequence(items) => {
        Value::Sequence(items.iter().map(relative).collect())
      }
      _ => value,
    },
    "log" => match value {
      Value::Mapping(mut map) => {
        for key in ["dir", "file"] {
          if let Some(path) = map.get(key).map(&relative) {
            map.insert(Value::from(key), path);
          }
        }
        Value::Mapping(map)
      }
      value => value,
    },
    "deps" => match &value {
      Value::Sequence(items) => Value::Sequence(
        items
          .iter()
          .map(|dep| match dep.as_str() {
            Some(dep) => Value::String(match key_in(mount, dep) {
              Some(key) => key,
              None => format!("/{dep}"),
            }),
            None => dep.clone(),
          })
          .collect(),
      ),
      _ => value,
    },
    _ => value,
  }
}

/// Edits the files with the chosen items. A write the config then does
/// not load with is undone. Returns what could not be written, by task
/// path.
pub fn write(plan: &WritePlan) -> Vec<(String, String)> {
  let mut failed = Vec::new();
  let mut by_file: BTreeMap<&str, Vec<&WriteItem>> = BTreeMap::new();
  for item in plan.chosen() {
    if let Some(note) = &item.note {
      failed.push((item.path.clone(), note.clone()));
      continue;
    }
    by_file.entry(&item.source.file).or_default().push(item);
  }
  // Each file written and what it said before, none for a new one.
  let mut before: Vec<(PathBuf, Option<String>)> = Vec::new();
  let mut written = Vec::new();
  for (file, items) in by_file {
    let path = plan.root.join(file);
    let old = match std::fs::read_to_string(&path) {
      Ok(text) => Some(text),
      Err(err) if err.kind() == std::io::ErrorKind::NotFound => None,
      Err(err) => {
        for item in items {
          failed.push((item.path.clone(), format!("{file}: {err}")));
        }
        continue;
      }
    };
    let mut text = old.clone().unwrap_or_default();
    let mut edited = Vec::new();
    for item in items {
      match writer::apply(&text, &item.edits) {
        Ok(new) => {
          text = new;
          edited.push(item);
        }
        Err(err) => failed.push((item.path.clone(), err.to_string())),
      }
    }
    if edited.is_empty() {
      continue;
    }
    match atomic_write(&path, text.as_bytes()) {
      Ok(()) => {
        before.push((path, old));
        written.extend(edited);
      }
      Err(err) => {
        for item in edited {
          failed.push((item.path.clone(), format!("{file}: {err}")));
        }
      }
    }
  }
  // As a fresh start takes it: deps on what the config lacks fail there.
  if !before.is_empty()
    && let Err(err) =
      Config::load_dir(&plan.root).and_then(|config| dep_indexes(&config))
  {
    for (path, old) in before {
      let undone = match old {
        Some(text) => atomic_write(&path, text.as_bytes()),
        None => std::fs::remove_file(&path).map_err(anyhow::Error::from),
      };
      if let Err(undo) = undone {
        log::error!("Can't undo the write to {}: {undo}", path.display());
      }
    }
    let why = format!("the config would not load: {err:#}");
    failed.extend(written.iter().map(|item| (item.path.clone(), why.clone())));
  }
  failed
}

/// After a write, by the writer or an agent: each entry that reads back
/// as written is recorded in the base, a task written under another key
/// or saved into the config takes its entry as its own, and a removal is
/// gone from the base. What does not read back is added to `failed`.
pub async fn written(
  pc: &TaskContext,
  project: &Arc<Project>,
  plan: &WritePlan,
  failed: &mut Vec<(String, String)>,
) {
  let _syncing = project.syncing().await;
  let config = project.load_blocking().await;
  let read = read_back(plan, &config, failed);
  let Ok(config) = config else { return };
  let mut entries = Vec::new();
  for item in read {
    if let Some(task) = item.task {
      let command = Command::SetOrigin {
        target: Target::Id(task),
        origin: Some(item.entry.clone()),
      };
      // Gone meanwhile: what was written for it is new to the runner.
      if !matches!(
        execute(pc, &config, &command).await,
        Ok(CommandResult::Matched(1..))
      ) {
        continue;
      }
    }
    if let Some((id, settings)) = &item.folder {
      let settings = FolderSettings {
        origin: Some(item.entry.clone()),
        ..settings.clone()
      };
      let (tx, rx) = tokio::sync::oneshot::channel();
      pc.send(KernelCommand::SetFolder(*id, settings, Some(tx)));
      if rx.await.unwrap_or(0) == 0 {
        continue;
      }
    }
    // Written under another key: the old one is gone.
    if item.entry != item.source {
      entries.push((item.source.clone(), None));
    }
    entries.push((item.entry.clone(), item.spec.clone()));
  }
  project.record(&config, entries);
}

/// The items chosen and written that read back from the config as
/// written: an entry that says what its task has, or is gone. One that
/// reads back otherwise, as `defaults` can make it, is added to `failed`
/// instead.
fn read_back<'a>(
  plan: &'a WritePlan,
  config: &anyhow::Result<Config>,
  failed: &mut Vec<(String, String)>,
) -> Vec<&'a WriteItem> {
  let base = config.as_ref().map(config_base);
  let mut read = Vec::new();
  let mut otherwise = Vec::new();
  let written = plan
    .chosen()
    .filter(|item| !failed.iter().any(|(path, _)| *path == item.path));
  for item in written {
    let why = match &base {
      Err(err) => format!("the config: {err:#}"),
      Ok(base) => match (base.get(&item.entry), &item.spec) {
        (None, None) => {
          read.push(item);
          continue;
        }
        (Some(_), None) => "the config still has it".to_string(),
        (None, Some(_)) => "the config does not have it".to_string(),
        (Some(spec), Some(written)) => {
          let differs: Vec<String> = spec
            .changed_keys(written)
            .into_iter()
            .filter(|key| {
              item.kind == WriteKind::Added
                || item.fields.iter().any(|f| f.key == *key)
            })
            .collect();
          if differs.is_empty() {
            read.push(item);
            continue;
          }
          format!("the config gives it another {}", differs.join(", "))
        }
      },
    };
    otherwise.push((item.path.clone(), why));
  }
  failed.extend(otherwise);
  read
}

/// What an agent needs to make the config say what the runner has.
pub fn prompt(plan: &WritePlan) -> String {
  let mut out = format!(
    "Update the dekit config in {} so it defines these tasks as the \
     runner has them. Change only what is listed and keep comments and \
     formatting. Task keys are relative to the file's mount (`load: {{at: \
     ...}}`); relative paths in a file are from its directory; a value \
     `defaults` gives may also be set on the task itself.\n\n",
    plan.root.display()
  );
  if plan.chosen().any(|item| item.source.is_folder()) {
    out.insert_str(
      out.len() - 1,
      "A key that ends in `/` is a folder: it holds the tasks under its \
       path.\n",
    );
  }
  for item in plan.chosen() {
    let noun = match item.source.is_folder() {
      true => "folder",
      false => "task",
    };
    match item.kind {
      WriteKind::Changed => {
        out.push_str(&format!(
          "- In {}, {noun} `{}`:\n",
          item.source.file, item.source.key
        ));
        for WriteField { key, was, now } in &item.fields {
          let was = show_value(was.as_ref());
          let line = match now {
            _ if key == "path" => {
              format!("move it from `{was}` to `{}`", show_value(now.as_ref()))
            }
            None => format!("remove `{key}` (now `{was}`)"),
            Some(now) => {
              format!(
                "set `{key}` to `{}` (now `{was}`)",
                show_value(Some(now))
              )
            }
          };
          out.push_str(&format!("  - {line}\n"));
        }
      }
      WriteKind::Added => {
        out.push_str(&format!(
          "- In {}, add task `{}`:\n",
          item.source.file, item.source.key
        ));
        for field in &item.fields {
          let now = show_value(field.now.as_ref());
          out.push_str(&format!("  - `{}`: `{now}`\n", field.key));
        }
      }
      WriteKind::Removed => out.push_str(&format!(
        "- In {}, remove {noun} `{}`.\n",
        item.source.file, item.source.key
      )),
    }
  }
  out.push_str(
    "\nThen run `dekit reload`: it should list none of these tasks as \
     changed.\n",
  );
  out
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::test_util::{TempDir, live_with, source, spec, task};

  /// A base of one entry.
  fn base_of(source: &Source, spec: &TaskSpec) -> Base {
    [(source.clone(), spec.clone())].into_iter().collect()
  }

  /// A config task removed here is taken out of the config too, unless
  /// only the runner's own tasks were asked for.
  #[test]
  fn a_removal_here_is_taken() {
    let source = source("api");
    let mut config = Config::make_default();
    let mut cfg = spec("{cmd: [api]}").parse("api", Path::new("/p")).unwrap();
    cfg.source = Some(source.clone());
    config.tasks = vec![cfg];
    let taken = |include: &[TaskId]| {
      let base = base_of(&source, &spec("{cmd: [api]}"));
      let plan =
        plan(&config, Path::new("/p"), &base, &Live::default(), include);
      let items = plan.items.iter().map(|i| (i.kind, i.apply));
      items.collect::<Vec<_>>()
    };
    assert_eq!(taken(&[]), [(WriteKind::Removed, true)]);
    assert_eq!(taken(&[TaskId(9)]), [(WriteKind::Removed, false)]);
  }

  #[test]
  fn a_path_is_written_from_the_file_up_to_the_project() {
    let written = |path: &str| {
      path_in_file(Path::new(path), Path::new("/p/web"), Path::new("/p"))
    };
    assert_eq!(written("/p/web/api"), "api");
    assert_eq!(written("/p/web"), ".");
    assert_eq!(written("/p"), "..");
    assert_eq!(written("/p/api"), "../api");
    assert_eq!(written("/opt/bin"), "/opt/bin");
  }

  #[test]
  fn writes_paths_relative_to_the_file_and_deps_to_the_mount() {
    let mut config = Config::make_default();
    let mut cfg = spec("{cmd: [api], cwd: /p/web/api, deps: [web/db]}")
      .parse("web/api", Path::new("/p"))
      .unwrap();
    let source = Source {
      file: "web/tasks.yaml".into(),
      key: "api".into(),
      mount: "web".into(),
    };
    cfg.source = Some(source.clone());
    config.tasks = vec![cfg.clone()];
    let base = TaskSpec::from_config(&cfg);
    let mut here = base.clone();
    here.set("cwd", Some("/p/shared/src".into()));
    here.set_deps(vec!["web/db".into(), "cache".into()]);
    let live = live_with([task(1, "web/server", here, Some(source.clone()))]);
    let plan = plan(
      &config,
      Path::new("/p"),
      &base_of(&source, &base),
      &live,
      &[],
    );
    let item = &plan.items[0];
    assert_eq!(item.kind, WriteKind::Changed);
    assert_eq!(
      item.edits,
      [
        Edit::Set {
          task: "api".into(),
          field: "deps".into(),
          value: Some(serde_yaml::from_str("[/cache, db]").unwrap()),
        },
        Edit::Set {
          task: "api".into(),
          field: "cwd".into(),
          value: Some("../shared/src".into()),
        },
        Edit::RenameTask {
          from: "api".into(),
          to: "server".into(),
        },
      ]
    );
    let prompt = prompt(&plan);
    assert!(prompt.contains("task `api`") && prompt.contains("`web/server`"));
  }

  /// What is written reads back as the tasks have it over `defaults`:
  /// an `env` an entry took whole from them is written whole, and a task
  /// added here without one is written with an empty one.
  #[test]
  fn what_is_written_reads_back_over_defaults() {
    let dir = TempDir::new("write-defaults");
    let root = dunce::canonicalize(&dir.0).unwrap();
    std::fs::write(
      root.join("dekit.yaml"),
      "defaults:\n  env: {B: '1'}\ntasks:\n  api:\n    cmd: [api]\n",
    )
    .unwrap();
    let config = Config::load_dir(&root).unwrap();
    let (cfg, base) = config_specs(&config).remove(0);
    let source = cfg.source.clone().unwrap();
    let mut api = base.clone();
    api.set_at("env.A", Some("2".into()));
    // In the root: written `cwd: .`, which reads back as the root.
    let build = spec(&format!(
      "{{type: job, cmd: [make], cwd: '{}'}}",
      crate::spec::written_path(&root)
    ));
    let live = live_with([
      task(1, "api", api.clone(), Some(source.clone())),
      task(2, "build", build.clone(), None),
    ]);
    let base = base_of(&source, &base);
    let mut plan = plan(&config, &root, &base, &live, &[TaskId(2)]);
    // Asked for the one task: the other change is offered, not taken.
    let taken: Vec<(&str, bool)> = plan
      .items
      .iter()
      .map(|i| (i.path.as_str(), i.apply))
      .collect();
    assert_eq!(taken, [("api", false), ("build", true)]);
    plan.items[0].apply = true;
    let mut failed = write(&plan);
    let read = read_back(&plan, &Config::load_dir(&root), &mut failed);
    assert!(failed.is_empty(), "{failed:?}");
    assert_eq!(read.len(), 2);
  }

  /// What an entry takes from `defaults` stays theirs: a new stop signal
  /// is written without the timeout they give it.
  #[test]
  fn what_defaults_give_is_not_copied_in() {
    let dir = TempDir::new("write-inherited");
    let root = dunce::canonicalize(&dir.0).unwrap();
    let text = "defaults:\n  stop: {signal: SIGTERM, timeout: 30s}\ntasks:\n  api:\n    cmd: [api]\n";
    std::fs::write(root.join("dekit.yaml"), text).unwrap();
    let config = Config::load_dir(&root).unwrap();
    let (cfg, base) = config_specs(&config).remove(0);
    let source = cfg.source.clone().unwrap();
    let mut here = base.clone();
    here.set(
      "stop",
      Some(serde_yaml::from_str("{signal: SIGINT, timeout: 30s}").unwrap()),
    );
    let live = live_with([task(1, "api", here, Some(source.clone()))]);
    let plan = plan(&config, &root, &base_of(&source, &base), &live, &[]);
    let mut failed = write(&plan);
    let read = read_back(&plan, &Config::load_dir(&root), &mut failed);
    let written = std::fs::read_to_string(root.join("dekit.yaml")).unwrap();
    assert!(failed.is_empty(), "{failed:?}");
    assert_eq!(read.len(), 1);
    assert!(
      written.ends_with("    cmd: [api]\n    stop:\n      signal: SIGINT\n"),
      "{written}"
    );
  }

  /// A change the config made meanwhile to what is not written stays a
  /// reload's: the write reads back, and the base records what the task
  /// has.
  #[test]
  fn a_change_the_config_made_meanwhile_stays_a_reloads() {
    let dir = TempDir::new("write-meanwhile");
    let root = dunce::canonicalize(&dir.0).unwrap();
    std::fs::write(root.join("dekit.yaml"), "tasks:\n  api:\n    cmd: [api]\n")
      .unwrap();
    let before = Config::load_dir(&root).unwrap();
    let (cfg, base) = config_specs(&before).remove(0);
    let source = cfg.source.clone().unwrap();
    std::fs::write(
      root.join("dekit.yaml"),
      "tasks:\n  api:\n    cmd: [api, --v2]\n",
    )
    .unwrap();
    let config = Config::load_dir(&root).unwrap();
    let mut here = base.clone();
    here.set_at("env.A", Some("1".into()));
    let live = live_with([task(1, "api", here.clone(), Some(source.clone()))]);
    let plan = plan(&config, &root, &base_of(&source, &base), &live, &[]);
    let mut failed = write(&plan);
    let read = read_back(&plan, &Config::load_dir(&root), &mut failed);
    let text = std::fs::read_to_string(root.join("dekit.yaml")).unwrap();
    assert!(failed.is_empty(), "{failed:?}");
    assert!(text.contains("--v2"), "{text}");
    let [item] = &read[..] else {
      panic!("{}", read.len())
    };
    assert_eq!(
      item.spec,
      Some(here),
      "the cmd is still the config's change"
    );
  }

  /// What a write-back reads back is recorded: a task written under its
  /// new key takes that entry as its own, the old key leaves the base,
  /// and so does an entry removed.
  #[tokio::test]
  async fn what_reads_back_is_recorded() {
    let dir = TempDir::new("write-recorded");
    let root = dunce::canonicalize(&dir.0).unwrap();
    let text = "tasks:\n  api:\n    cmd: [api]\n  db:\n    cmd: [db]\n";
    std::fs::write(root.join("dekit.yaml"), text).unwrap();
    let mut config = Config::load_dir(&root).unwrap();
    config.runner = Some(crate::runner::RunnerSpec {
      kind: crate::runner::RunnerKind::Project,
      root: root.clone(),
    });
    let project = Project::new(Arc::new(config.clone()));
    let kernel = crate::kernel::kernel::Kernel::new();
    let pc = kernel.context();
    let handle = tokio::spawn(kernel.run());
    // `api` is at `server` here, and `db` was removed here.
    let (cfg, spec) = config_specs(&config).remove(0);
    let add = Command::Add {
      target: Target::glob("server"),
      spec,
      place: Default::default(),
      start: false,
      defaults: true,
      origin: cfg.source.clone(),
    };
    execute(&pc, &config, &add).await.unwrap();

    let (config, live, base) =
      crate::reload::load(&pc, &project).await.unwrap();
    let plan = plan(&config, &root, &base, &live, &[]);
    let kinds: Vec<(WriteKind, bool)> =
      plan.items.iter().map(|i| (i.kind, i.apply)).collect();
    assert_eq!(
      kinds,
      [(WriteKind::Changed, true), (WriteKind::Removed, true)]
    );
    let mut failed = write(&plan);
    written(&pc, &project, &plan, &mut failed).await;
    let text = std::fs::read_to_string(root.join("dekit.yaml")).unwrap();
    assert!(failed.is_empty(), "{failed:?}");
    assert_eq!(text, "tasks:\n  server:\n    cmd: [api]\n");
    let keys: Vec<String> =
      project.base().sources().map(|s| s.key.clone()).collect();
    assert_eq!(keys, ["server"]);
    let live = crate::reload::describe(&pc).await;
    assert_eq!(
      live[0].origin.as_ref().map(|o| o.key.as_str()),
      Some("server")
    );
    pc.send(crate::kernel::kernel_message::KernelCommand::Quit);
    handle.await.unwrap();
  }

  /// A value `defaults` gives can't be taken away by removing the key,
  /// so the entry is not recorded as written.
  #[test]
  fn what_reads_back_otherwise_is_not_written() {
    let dir = TempDir::new("write-otherwise");
    let root = dunce::canonicalize(&dir.0).unwrap();
    std::fs::write(
      root.join("dekit.yaml"),
      "defaults:\n  cwd: app\ntasks:\n  api:\n    cmd: [api]\n",
    )
    .unwrap();
    let config = Config::load_dir(&root).unwrap();
    let (cfg, base) = config_specs(&config).remove(0);
    let source = cfg.source.clone().unwrap();
    let mut here = base.clone();
    here.set("cwd", None);
    let live = live_with([task(1, "api", here, Some(source.clone()))]);
    let plan = plan(&config, &root, &base_of(&source, &base), &live, &[]);
    let mut failed = write(&plan);
    let read = read_back(&plan, &Config::load_dir(&root), &mut failed);
    assert!(read.is_empty());
    assert_eq!(
      failed,
      [(
        "api".to_string(),
        "the config gives it another cwd".to_string()
      )]
    );
  }

  /// A file that can't be written fails each task written into it, so
  /// none of them is recorded. The write is atomic, a new file renamed
  /// over the old one, so what stops it is a directory that takes no
  /// new file.
  #[cfg(unix)]
  #[test]
  fn a_file_that_cant_be_written_fails_its_tasks() {
    use std::os::unix::fs::PermissionsExt;
    let dir = TempDir::new("write-back");
    let root = dir.0.clone();
    let file = root.join("dekit.yaml");
    std::fs::write(&file, "tasks:\n  api:\n    cmd: [api]\n").unwrap();
    let mode = |mode| std::fs::Permissions::from_mode(mode);
    std::fs::set_permissions(&root, mode(0o555)).unwrap();
    // Writable again for the cleanup, whatever the assertions do.
    struct Restore(std::path::PathBuf);
    impl Drop for Restore {
      fn drop(&mut self) {
        let _ = std::fs::set_permissions(
          &self.0,
          std::fs::Permissions::from_mode(0o755),
        );
      }
    }
    let _restore = Restore(root.clone());
    let mut config = Config::make_default();
    let mut cfg = spec("{cmd: [api]}").parse("api", &root).unwrap();
    let source = source("api");
    cfg.source = Some(source.clone());
    config.tasks = vec![cfg.clone()];
    let base = TaskSpec::from_config(&cfg);
    let mut here = base.clone();
    here.set_label(Some("API"));
    let live = live_with([task(1, "api", here, Some(source.clone()))]);
    let plan = plan(&config, &root, &base_of(&source, &base), &live, &[]);
    let failed = write(&plan);
    assert_eq!(failed.len(), 1, "{failed:?}");
    assert_eq!(failed[0].0, "api");
  }

  /// The runner's own task saved at the path of a config task removed
  /// here, which another file still has: the config would not load, so
  /// nothing is written.
  #[test]
  fn a_write_the_config_does_not_load_with_is_undone() {
    let dir = TempDir::new("write-undone");
    let root = dunce::canonicalize(&dir.0).unwrap();
    let main = "load: ['more.yaml']\ntasks:\n  db:\n    cmd: [db]\n";
    std::fs::write(root.join("dekit.yaml"), main).unwrap();
    std::fs::write(root.join("more.yaml"), "tasks:\n  api:\n    cmd: [api]\n")
      .unwrap();
    let config = Config::load_dir(&root).unwrap();
    let api = config_specs(&config)
      .into_iter()
      .find_map(|(cfg, _)| (cfg.path == "api").then(|| cfg.source.clone()))
      .flatten()
      .unwrap();
    let live = live_with([task(1, "api", spec("{cmd: [mine]}"), None)]);
    let base = base_of(&api, &spec("{cmd: [api]}"));
    let plan = plan(&config, &root, &base, &live, &[TaskId(1)]);
    let failed = write(&plan);
    let after = std::fs::read_to_string(root.join("dekit.yaml")).unwrap();
    assert_eq!(after, main);
    assert_eq!(failed.len(), 1, "{failed:?}");
    assert!(
      failed[0].1.starts_with("the config would not load"),
      "{failed:?}"
    );
  }

  /// A task of the runner's own that needs another not in the config is
  /// not written: the config would not start.
  #[test]
  fn a_task_needing_one_the_config_lacks_is_not_written() {
    let dir = TempDir::new("write-deps");
    let root = dunce::canonicalize(&dir.0).unwrap();
    let main = "tasks:\n  api:\n    cmd: [api]\n";
    std::fs::write(root.join("dekit.yaml"), main).unwrap();
    let config = Config::load_dir(&root).unwrap();
    let live =
      live_with([task(1, "web", spec("{cmd: [web], deps: [db]}"), None)]);
    let plan = plan(&config, &root, &Base::default(), &live, &[TaskId(1)]);
    let failed = write(&plan);
    let after = std::fs::read_to_string(root.join("dekit.yaml")).unwrap();
    assert_eq!(after, main);
    assert!(
      failed.len() == 1 && failed[0].1.contains("unknown process 'db'"),
      "{failed:?}"
    );
  }

  /// A folder removed here is taken out of the config; one moved here is
  /// written under its new key. Both read back.
  #[test]
  fn folders_are_written_back() {
    let dir = TempDir::new("write-folders");
    let root = dunce::canonicalize(&dir.0).unwrap();
    let text = "tasks:\n  apps/:\n    collapsed: true\n  apps/web:\n    cmd: [web]\n  tmp/: {}\n";
    std::fs::write(root.join("dekit.yaml"), text).unwrap();
    let config = Config::load_dir(&root).unwrap();
    let base = config_base(&config);
    let apps = config.folders[0].clone();
    let (cfg, _) = config_specs(&config).remove(0);
    let mut live = live_with([task(
      1,
      "apps/web",
      base.get(cfg.source.as_ref().unwrap()).unwrap().clone(),
      cfg.source.clone(),
    )]);
    live.folders = vec![crate::kernel::kernel_message::FolderInfo {
      id: TaskId(2),
      space: crate::kernel::task_key::TaskSpaceId::default_space(),
      path: crate::kernel::task_path::TaskPath::new("svc").unwrap(),
      after: None,
      settings: apps.settings(),
    }];
    let plan = plan(&config, &root, &base, &live, &[]);
    let items: Vec<(WriteKind, &str)> = plan
      .items
      .iter()
      .map(|item| (item.kind, item.path.as_str()))
      .collect();
    assert_eq!(
      items,
      [(WriteKind::Changed, "svc/"), (WriteKind::Removed, "tmp/")]
    );
    assert!(prompt(&plan).contains("A key that ends in `/` is a folder"));
    let mut failed = write(&plan);
    let read = read_back(&plan, &Config::load_dir(&root), &mut failed);
    assert!(failed.is_empty(), "{failed:?}");
    assert_eq!(read.len(), 2);
    let written = std::fs::read_to_string(root.join("dekit.yaml")).unwrap();
    assert_eq!(
      written,
      "tasks:\n  svc/:\n    collapsed: true\n  apps/web:\n    cmd: [web]\n"
    );
  }
}
