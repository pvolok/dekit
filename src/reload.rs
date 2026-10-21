//! A reload: the config on disk merged into the runner's tasks. The base
//! is the config as the runner last took it in, so the merge is
//! three-way: what changed only in the config comes in, what changed only
//! here stays, and what changed on both sides is the user's to pick (here,
//! unless they say otherwise). A plan only reads; carrying it out is what
//! changes the tasks and the base.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_yaml::Value;

use crate::command::{Command, CommandError, execute};
use crate::config::config::Config;
use crate::config::folder::collapsed;
use crate::kernel::kernel_message::{
  FolderDef, FolderInfo, FolderSettings, KernelCommand, Place, RegisterError,
  Registration, TaskContext, TaskDescription, TaskSelector,
};
use crate::kernel::path_trie::PathTrie;
use crate::kernel::task::TaskId;
use crate::kernel::task_key::TaskSpaceId;
use crate::kernel::task_path::{TaskPath, is_at_or_under, overlaps};
use crate::project::Project;
use crate::spec::{Base, Source, TaskSpec};
use crate::target::Target;
use crate::task::config_tasks::{config_base, config_specs};

/// Result of `reload`: what the config on disk would change, whether it
/// was carried out, and what failed then.
#[derive(Deserialize, Serialize)]
pub struct ReloadResult {
  pub plan: Plan,
  pub applied: bool,
  #[serde(default)]
  pub errors: Vec<String>,
}

/// The runner's tasks and folders in the default space, in list order,
/// the tasks with their definitions: what a reload and a write-back
/// compare the config with.
#[derive(Clone, Debug, Default)]
pub struct Live {
  pub tasks: Vec<TaskDescription>,
  pub folders: Vec<FolderInfo>,
}

impl Live {
  pub async fn read(pc: &TaskContext) -> Live {
    let folders = pc.folders(TaskSpaceId::default_space());
    let (tasks, folders) = tokio::join!(describe(pc), folders);
    Live { tasks, folders }
  }
}

/// What a reload would change. Sent as `reload` replies with it; what
/// carrying it out takes stays here.
#[derive(Clone, Deserialize, Serialize)]
pub struct Plan {
  pub changes: Vec<Change>,
  /// The config reordered its tasks.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub order: Option<OrderChange>,
  /// Config tasks changed here only: kept, and the config is behind.
  #[serde(default)]
  pub edited_here: Vec<String>,
  #[serde(skip, default = "no_config")]
  pub config: Arc<Config>,
  /// Config tasks whose entry is at their path under a new key or in
  /// another file: they take it as theirs.
  #[serde(skip)]
  adopt: Vec<(TaskId, Source)>,
  /// Each task here and its path, for what is in the way of a new one.
  #[serde(skip)]
  paths: Vec<(TaskId, String)>,
  /// Each folder here and its path, the same way.
  #[serde(skip)]
  folders: Vec<(TaskId, String)>,
}

fn no_config() -> Arc<Config> {
  Arc::new(Config::make_default())
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Change {
  pub kind: ChangeKind,
  /// Where the task is listed here, or will be; a folder's ends in `/`.
  pub path: String,
  /// For a changed task: what changed in the config.
  #[serde(default, skip_serializing_if = "Vec::is_empty")]
  pub fields: Vec<Field>,
  /// Whether the change is taken. A changed task takes its fields one by
  /// one; this is whether any is taken.
  pub apply: bool,
  /// Why it is not taken unless asked.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub reason: Option<Reason>,
  /// Taken, and carrying it out failed: a reload offers it again.
  #[serde(default, skip_serializing_if = "std::ops::Not::not")]
  pub failed: bool,
  #[serde(skip)]
  pub task: Option<TaskId>,
  #[serde(skip)]
  pub source: Source,
  #[serde(skip)]
  config: Option<TaskSpec>,
  /// What the entry said as last taken in, if anything: what it keeps
  /// saying in the base when the change fails.
  #[serde(skip)]
  base: Option<TaskSpec>,
  /// For a new task: the task in its way, as the changes taken stand,
  /// and whether it was taken before that.
  #[serde(skip)]
  in_the_way: Option<(String, bool)>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ChangeKind {
  /// New in the config.
  Added,
  Changed,
  /// Gone from the config.
  Removed,
  /// Removed here, and still in the config.
  RemovedHere,
  /// The runner's own, in the config now as it has it: taken, it is a
  /// config task.
  Written,
}

impl ChangeKind {
  /// What taking it does to the tasks, as a diff marks it.
  pub fn mark(self) -> &'static str {
    match self {
      ChangeKind::Added | ChangeKind::RemovedHere => "+",
      ChangeKind::Removed => "-",
      ChangeKind::Changed => "~",
      ChangeKind::Written => "=",
    }
  }
}

/// Why a change is not taken unless asked. On the wire a word, or
/// `{in-the-way: <path>}`.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Reason {
  /// Removed here, and still in the config.
  RemovedHere,
  /// Changed here too, to something else.
  Conflict,
  /// Gone from the config, and changed here.
  ChangedHere,
  /// Gone from the config, and running.
  Running,
  /// A task here is at or across the new task's path.
  InTheWay(String),
}

impl Reason {
  /// In words, for a list.
  pub fn text(&self) -> String {
    match self {
      Reason::RemovedHere => "removed here".to_string(),
      Reason::Conflict => "changed here too".to_string(),
      Reason::ChangedHere => "changed here".to_string(),
      Reason::Running => "running".to_string(),
      Reason::InTheWay(path) => format!("{path} is in the way"),
    }
  }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Field {
  /// A spec key, or `env.NAME`.
  pub key: String,
  pub base: Option<Value>,
  pub here: Option<Value>,
  pub config: Option<Value>,
  /// Changed here too, to something else.
  pub conflict: bool,
  /// Take the config's value; otherwise keep the one here.
  pub take: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct OrderChange {
  pub apply: bool,
  /// Reordered here too.
  pub conflict: bool,
}

impl Change {
  /// Whether it is a folder's entry that changed.
  pub fn is_folder(&self) -> bool {
    self.path.ends_with('/')
  }

  /// Takes or leaves the whole change.
  pub fn set_apply(&mut self, apply: bool) {
    self.apply = apply;
    for field in &mut self.fields {
      field.take = apply;
    }
  }

  /// Whether some fields are taken and some are not.
  pub fn mixed(&self) -> bool {
    let taken = self.fields.iter().filter(|f| f.take).count();
    taken > 0 && taken < self.fields.len()
  }
}

impl Plan {
  /// How many changes the config has for the runner.
  pub fn len(&self) -> usize {
    self.changes.len() + usize::from(self.order.is_some())
  }

  /// Takes or leaves a change whole, and what is in the way of a new
  /// task with it.
  pub fn set_apply(&mut self, index: usize, apply: bool) {
    if let Some(change) = self.changes.get_mut(index) {
      change.set_apply(apply);
      self.check_the_way();
    }
  }

  /// Takes every change and the order but the new tasks a task here is
  /// in the way of; or, when those are what is taken, leaves them all.
  pub fn toggle_all(&mut self) {
    let all = self.changes.iter().all(|change| {
      (change.apply && !change.mixed()) || change.in_the_way.is_some()
    }) && self.order.as_ref().is_none_or(|order| order.apply);
    for change in &mut self.changes {
      change.set_apply(!all);
    }
    if let Some(order) = &mut self.order {
      order.apply = !all;
    }
    self.check_the_way();
    for change in &mut self.changes {
      change.set_apply(!all && change.in_the_way.is_none());
    }
  }

  /// Marks each new task that a task here would be in the way of, once
  /// the changes taken are carried out: the kernel would refuse it. One
  /// that comes to be in the way is left; one no longer in the way is
  /// taken again if it was before. A task removed is not in the way.
  fn check_the_way(&mut self) {
    let gone: HashSet<TaskId> = self
      .changes
      .iter()
      .filter(|change| change.kind == ChangeKind::Removed && change.apply)
      .filter_map(|change| change.task)
      .collect();
    let here = |list: &[(TaskId, String)]| -> Vec<String> {
      list
        .iter()
        .filter(|(id, _)| !gone.contains(id))
        .map(|(_, path)| path.clone())
        .collect()
    };
    let (paths, folders) = (here(&self.paths), here(&self.folders));
    for change in &mut self.changes {
      if !matches!(change.kind, ChangeKind::Added | ChangeKind::RemovedHere) {
        continue;
      }
      // A folder holds tasks: a task at it or above it is in its way, as
      // is a folder at it; a folder is in a task's way at it or under it.
      let task = |p: &String| p.clone();
      let folder = |p: &String| format!("{p}/");
      let in_the_way = match change.path.strip_suffix('/') {
        Some(at) => (paths.iter().find(|p| is_at_or_under(at, p)).map(task))
          .or_else(|| folders.iter().find(|p| *p == at).map(folder)),
        None => (paths.iter().find(|p| overlaps(p, &change.path)).map(task))
          .or_else(|| {
            let under = |p: &&String| is_at_or_under(p, &change.path);
            folders.iter().find(under).map(folder)
          }),
      };
      if in_the_way.as_ref() == change.in_the_way.as_ref().map(|(path, _)| path)
      {
        continue;
      }
      let was = change.in_the_way.take();
      match in_the_way {
        Some(path) => {
          change.reason = Some(Reason::InTheWay(path.clone()));
          let taken = was.map_or(change.apply, |(_, taken)| taken);
          change.in_the_way = Some((path, taken));
          change.apply = false;
        }
        None => {
          let removed_here = change.kind == ChangeKind::RemovedHere;
          change.reason = removed_here.then_some(Reason::RemovedHere);
          change.apply = was.is_some_and(|(_, taken)| taken);
        }
      }
    }
  }
}

/// The default space's tasks with their definitions, in list order.
pub async fn describe(pc: &TaskContext) -> Vec<TaskDescription> {
  pc.describe(TaskSelector::all()).await
}

/// What a reload to `config` would change, with the default choices.
pub fn plan(config: Arc<Config>, base: &Base, here: &Live) -> Plan {
  let live = here.tasks.as_slice();
  let specs = config_specs(&config);
  let in_config: HashSet<&Source> = specs
    .iter()
    .filter_map(|(cfg, _)| cfg.source.as_ref())
    .collect();
  let by_source: HashMap<&Source, &TaskDescription> = live
    .iter()
    .filter_map(|task| Some((task.origin.as_ref()?, task)))
    .collect();
  fn by_path(task: &TaskDescription) -> Option<(&str, &TaskDescription)> {
    Some((task.info.path.as_ref()?.as_str(), task))
  }
  // A task whose entry is gone, at the path of an entry the config has
  // now: the same task, under another key or in another file.
  let mut moved: HashMap<&str, &TaskDescription> = live
    .iter()
    .filter(|task| task.origin.as_ref().is_some_and(|o| !in_config.contains(o)))
    .filter_map(by_path)
    .collect();
  // The runner's own, at the path of an entry that says what it has:
  // written into the config.
  let own: HashMap<&str, &TaskDescription> = live
    .iter()
    .filter(|task| task.origin.is_none() && task.spec.is_some())
    .filter_map(by_path)
    .collect();
  let mut adopted = HashSet::new();
  let mut adopt = Vec::new();
  let mut changes = Vec::new();
  let mut edited_here = Vec::new();
  for (cfg, config_spec) in &specs {
    let Some(source) = &cfg.source else {
      continue;
    };
    let task = by_source.get(source).copied().or_else(|| {
      let task = moved.remove(cfg.path.as_str())?;
      adopted.insert(task.info.id);
      Some(task)
    });
    let Some(task) = task else {
      let written = own.get(cfg.path.as_str()).filter(|task| {
        task
          .spec
          .as_ref()
          .is_some_and(|here| here.changed_keys(config_spec).is_empty())
      });
      if let Some(task) = written {
        changes.push(Change {
          kind: ChangeKind::Written,
          path: cfg.path.clone(),
          task: Some(task.info.id),
          source: source.clone(),
          fields: Vec::new(),
          apply: true,
          reason: None,
          config: Some(config_spec.clone()),
          base: None,
          in_the_way: None,
          failed: false,
        });
        continue;
      }
      let was = base.get(source).cloned();
      let removed_here = was.is_some();
      let reason = removed_here.then_some(Reason::RemovedHere);
      changes.push(Change {
        kind: if removed_here {
          ChangeKind::RemovedHere
        } else {
          ChangeKind::Added
        },
        path: cfg.path.clone(),
        task: None,
        source: source.clone(),
        fields: Vec::new(),
        apply: reason.is_none(),
        reason,
        config: Some(config_spec.clone()),
        base: was,
        in_the_way: None,
        failed: false,
      });
      continue;
    };
    let origin = task.origin.as_ref().expect("found by origin");
    if origin != source {
      adopt.push((task.info.id, source.clone()));
    }
    let here = task.spec.clone().unwrap_or_default();
    let here_path = task.info.path.as_ref().map_or("", |p| p.as_str());
    // An entry the base lacks, as while a write-back records it, is as
    // the config says it: what differs is here's.
    let was = base.get(origin).unwrap_or(config_spec);
    let (fields, changed_here) = merge(was, &here, config_spec);
    // Moved here. The config's own moves come as a removal and an
    // addition: an entry is known by its key.
    if changed_here || here_path != cfg.path {
      edited_here.push(here_path.to_string());
    }
    if fields.is_empty() {
      continue;
    }
    let apply = fields.iter().any(|f| f.take);
    changes.push(Change {
      kind: ChangeKind::Changed,
      path: here_path.to_string(),
      task: Some(task.info.id),
      source: source.clone(),
      apply,
      reason: fields
        .iter()
        .any(|f| f.conflict)
        .then_some(Reason::Conflict),
      fields,
      config: Some(config_spec.clone()),
      base: Some(was.clone()),
      in_the_way: None,
      failed: false,
    });
  }
  for task in live {
    let Some(origin) = &task.origin else {
      continue;
    };
    if in_config.contains(origin) || adopted.contains(&task.info.id) {
      continue;
    }
    let here = task.spec.clone().unwrap_or_default();
    let running = task.info.state.is_active();
    let was = base.get(origin);
    // Without a base to tell, it is taken as changed here.
    let edited = was.is_none_or(|was| !here.changed_keys(was).is_empty());
    let reason = match (edited, running) {
      (true, _) => Some(Reason::ChangedHere),
      (false, true) => Some(Reason::Running),
      (false, false) => None,
    };
    changes.push(Change {
      kind: ChangeKind::Removed,
      path: task
        .info
        .path
        .as_ref()
        .map_or("", |p| p.as_str())
        .to_string(),
      task: Some(task.info.id),
      source: origin.clone(),
      fields: Vec::new(),
      apply: reason.is_none(),
      reason,
      config: None,
      base: was.cloned(),
      in_the_way: None,
      failed: false,
    });
  }
  plan_folders(&config, base, &here.folders, &mut changes, &mut adopt);
  for folder in &here.folders {
    let moved = folder.settings.origin.as_ref().is_some_and(|origin| {
      config.folders.iter().any(|entry| {
        entry.source.as_ref() == Some(origin)
          && entry.path != folder.path.as_str()
      })
    });
    if moved {
      edited_here.push(format!("{}/", folder.path));
    }
  }
  let base_sources: Vec<Source> = base.sources().cloned().collect();
  let mut plan = Plan {
    order: order_change(&config.sources(), &base_sources, &listed_here(here)),
    config,
    changes,
    edited_here,
    adopt,
    paths: live
      .iter()
      .filter_map(|task| {
        Some((task.info.id, task.info.path.as_ref()?.to_string()))
      })
      .collect(),
    folders: here
      .folders
      .iter()
      .map(|folder| (folder.id, folder.path.to_string()))
      .collect(),
  };
  plan.check_the_way();
  plan
}

/// The changes to the folders, as `plan` makes the tasks' with the same
/// kinds: a folder is known by its entry, or else by its path. Taking
/// one away leaves what is under it.
fn plan_folders(
  config: &Config,
  base: &Base,
  folders: &[FolderInfo],
  changes: &mut Vec<Change>,
  adopt: &mut Vec<(TaskId, Source)>,
) {
  let in_config: HashSet<&Source> = config
    .folders
    .iter()
    .filter_map(|entry| entry.source.as_ref())
    .collect();
  let by_source: HashMap<&Source, &FolderInfo> = folders
    .iter()
    .filter_map(|folder| Some((folder.settings.origin.as_ref()?, folder)))
    .collect();
  let mut moved: HashMap<&str, &FolderInfo> = folders
    .iter()
    .filter(|folder| {
      let origin = folder.settings.origin.as_ref();
      origin.is_some_and(|origin| !in_config.contains(origin))
    })
    .map(|folder| (folder.path.as_str(), folder))
    .collect();
  let own: HashMap<&str, &FolderInfo> = folders
    .iter()
    .filter(|folder| folder.settings.origin.is_none())
    .map(|folder| (folder.path.as_str(), folder))
    .collect();
  let spec_of = |folder: &FolderInfo| {
    let mut spec = TaskSpec::default();
    if folder.settings.collapsed {
      spec.set("collapsed", Some(Value::Bool(true)));
    }
    spec
  };
  let change =
    |kind, path: &str, folder: Option<&FolderInfo>, source: &Source| Change {
      kind,
      path: format!("{path}/"),
      fields: Vec::new(),
      apply: true,
      reason: None,
      failed: false,
      task: folder.map(|folder| folder.id),
      source: source.clone(),
      config: None,
      base: None,
      in_the_way: None,
    };
  let mut adopted = HashSet::new();
  for entry in &config.folders {
    let Some(source) = &entry.source else {
      continue;
    };
    let config_spec = entry.spec();
    let folder = by_source.get(source).copied().or_else(|| {
      let folder = moved.remove(entry.path.as_str())?;
      adopted.insert(folder.id);
      Some(folder)
    });
    let Some(folder) = folder else {
      // The runner's own at its path: what it says is what folds it at
      // first, which the entry says from now on.
      if let Some(own) = own.get(entry.path.as_str()) {
        changes.push(Change {
          config: Some(config_spec),
          ..change(ChangeKind::Written, &entry.path, Some(own), source)
        });
        continue;
      }
      let was = base.get(source).cloned();
      let removed_here = was.is_some();
      let kind = match removed_here {
        true => ChangeKind::RemovedHere,
        false => ChangeKind::Added,
      };
      changes.push(Change {
        apply: !removed_here,
        reason: removed_here.then_some(Reason::RemovedHere),
        config: Some(config_spec),
        base: was,
        ..change(kind, &entry.path, None, source)
      });
      continue;
    };
    let origin = folder.settings.origin.as_ref().expect("found by origin");
    if origin != source {
      adopt.push((folder.id, source.clone()));
    }
    let was = base.get(origin).unwrap_or(&config_spec);
    let (fields, _) = merge(was, &spec_of(folder), &config_spec);
    if fields.is_empty() {
      continue;
    }
    changes.push(Change {
      apply: fields.iter().any(|f| f.take),
      reason: fields
        .iter()
        .any(|f| f.conflict)
        .then_some(Reason::Conflict),
      fields,
      config: Some(config_spec.clone()),
      base: Some(was.clone()),
      ..change(
        ChangeKind::Changed,
        folder.path.as_str(),
        Some(folder),
        source,
      )
    });
  }
  for folder in folders {
    let Some(origin) = &folder.settings.origin else {
      continue;
    };
    if in_config.contains(origin) || adopted.contains(&folder.id) {
      continue;
    }
    changes.push(Change {
      base: base.get(origin).cloned(),
      ..change(
        ChangeKind::Removed,
        folder.path.as_str(),
        Some(folder),
        origin,
      )
    });
  }
}

/// The three-way merge of one task's definition. Returns the fields the
/// config changed, and whether anything changed here only.
fn merge(
  base: &TaskSpec,
  here: &TaskSpec,
  config: &TaskSpec,
) -> (Vec<Field>, bool) {
  let mut keys = base.changed_keys(config);
  for key in base.changed_keys(here) {
    if !keys.contains(&key) {
      keys.push(key);
    }
  }
  let mut fields = Vec::new();
  let mut changed_here = false;
  for key in keys {
    let value = |spec: &TaskSpec| spec.value_at(&key).cloned();
    let (b, h, c) = (value(base), value(here), value(config));
    match (c != b, h != b) {
      (true, false) => fields.push(Field {
        key,
        base: b,
        here: h,
        config: c,
        conflict: false,
        take: true,
      }),
      (false, true) => changed_here = true,
      // Both made it the same.
      (true, true) if h == c => (),
      (true, true) => fields.push(Field {
        key,
        base: b,
        here: h,
        config: c,
        conflict: true,
        take: false,
      }),
      (false, false) => (),
    }
  }
  (fields, changed_here)
}

/// The entries of the tasks and folders here, in list order.
fn listed_here(live: &Live) -> Vec<Source> {
  let mut listed: Vec<(TaskId, Option<&Source>)> = live
    .tasks
    .iter()
    .map(|task| (task.info.id, task.origin.as_ref()))
    .collect();
  // In list order, so what each is after is in already.
  for folder in &live.folders {
    let at = folder.after.map_or(0, |after| {
      let at = listed.iter().position(|(id, _)| *id == after);
      at.map_or(listed.len(), |at| at + 1)
    });
    listed.insert(at, (folder.id, folder.settings.origin.as_ref()));
  }
  listed
    .into_iter()
    .filter_map(|(_, source)| source.cloned())
    .collect()
}

/// The config's order of its tasks and folders, if it changed it.
fn order_change(
  order: &[Source],
  base: &[Source],
  here: &[Source],
) -> Option<OrderChange> {
  // As a list shows them: each group where its first entry is.
  let listed = |sources: &[Source]| -> Vec<Source> {
    let mut trie = PathTrie::new();
    for (i, source) in sources.iter().enumerate() {
      if let Ok(path) = TaskPath::new(source.path()) {
        let _ = match source.is_folder() {
          true => trie.insert_folder(&path, TaskId(i), &Place::Last),
          false => trie.insert(&path, TaskId(i), &Place::Last),
        };
      }
    }
    trie
      .nodes(&TaskPath::root())
      .into_iter()
      .map(|(_, TaskId(i))| sources[i].clone())
      .collect()
  };
  let in_config_list = listed(order);
  let in_base_list = listed(base);
  let (in_order, in_base, is_here): (
    HashSet<&Source>,
    HashSet<&Source>,
    HashSet<&Source>,
  ) = (
    order.iter().collect(),
    base.iter().collect(),
    here.iter().collect(),
  );
  let common = |list: &[Source]| -> Vec<Source> {
    list
      .iter()
      .filter(|s| {
        in_order.contains(s) && in_base.contains(s) && is_here.contains(s)
      })
      .cloned()
      .collect()
  };
  let (in_config, in_base, in_here) =
    (common(&in_config_list), common(&in_base_list), common(here));
  if in_config == in_base || in_here == in_config {
    return None;
  }
  let conflict = in_here != in_base;
  Some(OrderChange {
    apply: !conflict,
    conflict,
  })
}

/// A step that carries a plan out, and the config entry it is for.
struct Step {
  source: Option<Source>,
  act: Act,
}

/// What a step does: a command, or what no command does to a folder.
enum Act {
  Command(Command),
  AddFolder(TaskPath, Place, FolderSettings),
  /// Takes the folder away, leaving what is under it.
  DropFolder(TaskId),
  SetFolder(TaskId, FolderSettings),
}

/// The steps that carry the plan's choices out, in an order each can run
/// in: the entries tasks and folders take as theirs; removals, so the new
/// tasks' places are free; new tasks and folders, each task after the
/// ones it needs; the changes to the others; and the config's order last.
fn steps(plan: &Plan, live: &Live) -> Vec<Step> {
  let mut steps = Vec::new();
  let mut push = |source: Option<&Source>, act| {
    steps.push(Step {
      source: source.cloned(),
      act,
    });
  };
  let id = |id: TaskId| Target::Id(id);
  let path_of = |path: &str| Target::glob(path);
  let folder = |id: TaskId| live.folders.iter().find(|f| f.id == id);
  // A folder as it is, but for its entry.
  let with_origin = |id: TaskId, origin: Option<Source>| FolderSettings {
    origin,
    ..folder(id).map(|f| f.settings.clone()).unwrap_or_default()
  };
  // Taken whatever else fails: what a task is is not a change to it.
  for (task, source) in &plan.adopt {
    let origin = Some(source.clone());
    push(
      None,
      match folder(*task) {
        Some(_) => Act::SetFolder(*task, with_origin(*task, origin)),
        None => Act::Command(Command::SetOrigin {
          target: id(*task),
          origin,
        }),
      },
    );
  }
  // Where each config task and folder is once the plan is carried out,
  // by source.
  let adopted = |id: TaskId| {
    plan
      .adopt
      .iter()
      .find(|(adopted, _)| *adopted == id)
      .map(|(_, source)| source)
  };
  let tasks = live.tasks.iter().filter_map(|task| {
    let source = adopted(task.info.id).or(task.origin.as_ref())?;
    Some((source.clone(), task.info.path.as_ref()?.to_string()))
  });
  let folders = live.folders.iter().filter_map(|folder| {
    let source = adopted(folder.id).or(folder.settings.origin.as_ref())?;
    Some((source.clone(), folder.path.to_string()))
  });
  let mut at: HashMap<Source, String> = tasks.chain(folders).collect();
  // Where a change's task or folder is, or will be.
  let path_at = |change: &Change| change.path.trim_end_matches('/').to_string();
  let mut removed = HashSet::new();
  let changed = |kind| plan.changes.iter().filter(move |c| c.kind == kind);

  for change in changed(ChangeKind::Removed) {
    let Some(task) = change.task else { continue };
    let source = Some(&change.source);
    match (change.apply, change.is_folder()) {
      (true, true) => {
        push(source, Act::DropFolder(task));
        at.remove(&change.source);
      }
      (true, false) => {
        push(source, Act::Command(Command::Remove { target: id(task) }));
        at.remove(&change.source);
        removed.insert(change.path.clone());
      }
      // Kept: it is the runner's own from now on.
      (false, true) => {
        push(source, Act::SetFolder(task, with_origin(task, None)))
      }
      (false, false) => push(
        source,
        Act::Command(Command::SetOrigin {
          target: id(task),
          origin: None,
        }),
      ),
    }
  }

  for change in changed(ChangeKind::Written).filter(|c| c.apply) {
    let Some(task) = change.task else { continue };
    let origin = Some(change.source.clone());
    push(
      Some(&change.source),
      match change.is_folder() {
        true => Act::SetFolder(
          task,
          FolderSettings {
            collapsed: change.config.as_ref().is_some_and(collapsed),
            origin,
          },
        ),
        false => Act::Command(Command::SetOrigin {
          target: id(task),
          origin,
        }),
      },
    );
    at.insert(change.source.clone(), path_at(change));
  }

  // A dep the config names on a task listed elsewhere here is on where
  // it is, as a move here renames the deps on it.
  let moved: HashMap<&str, String> = plan
    .config
    .tasks
    .iter()
    .filter_map(|task| {
      let here = at.get(task.source.as_ref()?)?;
      (*here != task.path).then(|| (task.path.as_str(), here.clone()))
    })
    .collect();
  let listed_here = |spec: &mut TaskSpec| {
    let deps = spec.deps();
    if deps.iter().any(|dep| moved.contains_key(dep.as_str())) {
      let here = |dep: String| moved.get(dep.as_str()).cloned().unwrap_or(dep);
      spec.set_deps(deps.into_iter().map(here).collect());
    }
  };

  let mut adds: Vec<&Change> = plan
    .changes
    .iter()
    .filter(|c| {
      c.apply && matches!(c.kind, ChangeKind::Added | ChangeKind::RemovedHere)
    })
    // One a task has by now, as another reload added it meanwhile, is in.
    .filter(|c| !at.contains_key(&c.source))
    .collect();
  // The folders first: a new task under one is listed in it, and it
  // shows as its entry says.
  adds.sort_by_key(|change| !change.is_folder());
  while !adds.is_empty() {
    let pending: HashSet<&str> = adds.iter().map(|c| c.path.as_str()).collect();
    let (mut ready, mut waiting): (Vec<&Change>, Vec<&Change>) =
      adds.into_iter().partition(|change| {
        change.config.as_ref().is_none_or(|spec| {
          spec
            .deps()
            .iter()
            .all(|dep| !pending.contains(dep.as_str()))
        })
      });
    if ready.is_empty() {
      // A cycle, which the config refuses: in its order, then.
      std::mem::swap(&mut ready, &mut waiting);
    }
    for change in ready {
      let Some(spec) = &change.config else { continue };
      // With nothing before it in the config listed beside it, all
      // that is comes after it.
      let place = after_in(plan, &at, change)
        .and_then(|after| TaskPath::new(after).ok())
        .map_or(Place::First, Place::After);
      let origin = Some(change.source.clone());
      let act = match TaskPath::new(path_at(change)) {
        Ok(path) if change.is_folder() => {
          let settings = FolderSettings {
            collapsed: collapsed(spec),
            origin,
          };
          Act::AddFolder(path, place, settings)
        }
        Ok(_) | Err(_) => {
          let mut spec = spec.clone();
          listed_here(&mut spec);
          Act::Command(Command::Add {
            target: path_of(&change.path),
            spec,
            place,
            start: false,
            defaults: false,
            origin,
          })
        }
      };
      push(Some(&change.source), act);
      at.insert(change.source.clone(), path_at(change));
    }
    adds = waiting;
  }

  // Each changed task's spec with the fields taken, made from what it
  // has now, as the removals above leave it: a change made since the plan
  // stays, and one made after `live` was read refuses the update.
  for change in changed(ChangeKind::Changed) {
    if change.is_folder() {
      let (Some(id), Some(config)) = (change.task, &change.config) else {
        continue;
      };
      let mut settings = with_origin(id, Some(change.source.clone()));
      if change.fields.iter().any(|f| f.take && f.key == "collapsed") {
        settings.collapsed = collapsed(config);
      }
      push(Some(&change.source), Act::SetFolder(id, settings));
      continue;
    }
    let task = live
      .tasks
      .iter()
      .find(|task| Some(task.info.id) == change.task);
    let (Some(task), Some(config)) = (task, &change.config) else {
      continue;
    };
    let (Some(mut here), Some(path)) = (task.spec.clone(), &task.info.path)
    else {
      continue;
    };
    let deps = here.deps();
    if deps.iter().any(|dep| removed.contains(dep)) {
      here
        .set_deps(deps.into_iter().filter(|d| !removed.contains(d)).collect());
    }
    let mut spec = here.clone();
    for field in change.fields.iter().filter(|f| f.take) {
      spec.set_at(&field.key, config.value_at(&field.key).cloned());
      if field.key == "deps" {
        listed_here(&mut spec);
      }
    }
    if spec == here {
      continue;
    }
    push(
      Some(&change.source),
      Act::Command(Command::Update {
        target: path_of(path.as_str()),
        spec,
        was: Some(here),
        restart: false,
      }),
    );
  }
  if plan.order.as_ref().is_some_and(|order| order.apply) {
    // Each config task, and each group the first time one of its tasks
    // comes, right after what came before it beside it.
    let mut last: HashMap<TaskPath, TaskPath> = HashMap::new();
    let mut placed: HashSet<TaskPath> = HashSet::new();
    for source in &plan.config.sources() {
      let Some(path) = at.get(source).and_then(|p| TaskPath::new(p).ok())
      else {
        continue;
      };
      let mut from_top: Vec<TaskPath> =
        std::iter::successors(Some(path), TaskPath::parent)
          .take_while(|p| !p.is_root())
          .collect();
      from_top.reverse();
      for path in from_top {
        if !placed.insert(path.clone()) {
          continue;
        }
        let parent = path.parent().unwrap_or_else(TaskPath::root);
        let place = match last.get(&parent) {
          Some(before) => Place::After(before.clone()),
          None => Place::First,
        };
        let target = path_of(path.as_str());
        push(
          None,
          Act::Command(Command::Move {
            target,
            to: None,
            place,
          }),
        );
        last.insert(parent, path);
      }
    }
  }
  steps
}

/// Where a new task is listed: right after the nearest config task
/// before it that is there and beside it.
fn after_in<'a>(
  plan: &Plan,
  at: &'a HashMap<Source, String>,
  change: &Change,
) -> Option<&'a str> {
  let parent = |path: &str| {
    let path = path.trim_end_matches('/');
    TaskPath::new(path).ok().and_then(|p| p.parent())
  };
  let sources = plan.config.sources();
  let index = sources.iter().position(|s| *s == change.source)?;
  sources[..index].iter().rev().find_map(|source| {
    let path = at.get(source)?;
    (parent(path) == parent(&change.path)).then_some(path.as_str())
  })
}

/// Carries the plan out, marking the changes that failed, and takes the
/// config in as the base (`carried_out`). Returns what failed.
pub async fn apply(
  pc: &TaskContext,
  project: &Project,
  plan: &mut Plan,
) -> Vec<String> {
  let _syncing = project.syncing().await;
  let live = Live::read(pc).await;
  let config = plan.config.clone();
  let mut errors = Vec::new();
  let mut failed = HashSet::new();
  for Step { source, act } in steps(plan, &live) {
    // What follows a failed step for an entry would act on what is not.
    if source
      .as_ref()
      .is_some_and(|source| failed.contains(source))
    {
      continue;
    }
    let done = match &act {
      Act::Command(command) => match execute(pc, &config, command).await {
        Ok(_) => Ok(()),
        // An order that no longer fits what is here is left as it is.
        Err(CommandError::Move(_))
          if matches!(command, Command::Move { to: None, .. }) =>
        {
          Ok(())
        }
        Err(err) => Err(describe_failure(command, &err)),
      },
      folder => carry_out(pc, folder).await,
    };
    match done {
      Ok(()) => (),
      Err(err) => {
        errors.push(err);
        if let Some(source) = source {
          for change in &mut plan.changes {
            change.failed |= change.source == source;
          }
          failed.insert(source);
        }
      }
    }
  }
  project.reloaded(config, carried_out(plan));
  errors
}

/// The base a plan carried out leaves: each entry as the config says it,
/// but for a new task or one of the runner's own not taken, which a
/// reload offers again, and a change that failed, whose entry says what
/// it said: a reload offers it again.
fn carried_out(plan: &Plan) -> Base {
  let change =
    |source: &Source| plan.changes.iter().find(|c| c.source == *source);
  let config = config_base(&plan.config);
  let in_config = config.iter().filter_map(|(source, spec)| {
    let source = source.clone();
    match change(&source) {
      Some(change) if change.failed => Some((source, change.base.clone()?)),
      Some(change)
        if matches!(change.kind, ChangeKind::Added | ChangeKind::Written)
          && !change.apply =>
      {
        None
      }
      Some(_) | None => Some((source, spec.clone())),
    }
  });
  // Gone from the config, and its task still here.
  let gone = plan.changes.iter().filter_map(|change| {
    let failed = change.kind == ChangeKind::Removed && change.failed;
    Some((
      change.source.clone(),
      change.base.clone().filter(|_| failed)?,
    ))
  });
  in_config.chain(gone).collect()
}

/// Carries a folder's step out; `Err` says why it could not be.
async fn carry_out(pc: &TaskContext, act: &Act) -> Result<(), String> {
  let (tx, rx) = tokio::sync::oneshot::channel();
  let id = match act {
    Act::Command(_) => return Ok(()),
    Act::AddFolder(path, place, settings) => {
      let folder = Registration::Folder(FolderDef {
        id: pc.alloc_id(),
        space: TaskSpaceId::default_space(),
        path: path.clone(),
        place: place.clone(),
        settings: settings.clone(),
      });
      let results = pc.register_tasks(vec![folder]).await;
      return match results.map(|mut results| results.pop()) {
        Ok(Some(Ok(()))) => Ok(()),
        Ok(Some(Err(err))) => Err(format!("{path}/: {err}")),
        Ok(None) | Err(_) => {
          Err(format!("{path}/: {}", RegisterError::Changed))
        }
      };
    }
    Act::DropFolder(id) => {
      pc.send(KernelCommand::DropFolder(*id, Some(tx)));
      id
    }
    Act::SetFolder(id, settings) => {
      pc.send(KernelCommand::SetFolder(*id, settings.clone(), Some(tx)));
      id
    }
  };
  match rx.await.unwrap_or(0) {
    0 => Err(format!("folder {}: it is gone", id.0)),
    _ => Ok(()),
  }
}

fn describe_failure(command: &Command, err: &CommandError) -> String {
  let target = match command {
    Command::Add { target, .. }
    | Command::Update { target, .. }
    | Command::Move { target, .. }
    | Command::Remove { target }
    | Command::SetOrigin { target, .. } => target.to_string(),
    _ => String::new(),
  };
  format!("{target}: {err}")
}

/// The config on disk, the tasks, and the base, as they stand together:
/// what a reload or a write-back is planned against. Nothing changes.
pub async fn load(
  pc: &TaskContext,
  project: &Arc<Project>,
) -> anyhow::Result<(Arc<Config>, Live, Base)> {
  let _syncing = project.syncing().await;
  let (config, live) = tokio::join!(project.load_blocking(), Live::read(pc));
  Ok((Arc::new(config?), live, project.base()))
}

/// The tasks and the base, as they stand together: for a comparison with
/// a config read before, while what it is read from is unchanged.
pub async fn tasks(pc: &TaskContext, project: &Project) -> (Live, Base) {
  let _syncing = project.syncing().await;
  (Live::read(pc).await, project.base())
}

/// Loads the config and plans a reload against the runner's tasks.
pub async fn plan_reload(
  pc: &TaskContext,
  project: &Arc<Project>,
) -> anyhow::Result<Plan> {
  let (config, live, base) = load(pc, project).await?;
  Ok(plan(config, &base, &live))
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::config::task::TaskConfig;
  use crate::test_util::{live_with, source, spec};

  fn commands(plan: &Plan, live: &Live) -> Vec<Command> {
    steps(plan, live)
      .into_iter()
      .filter_map(|step| match step.act {
        Act::Command(command) => Some(command),
        _ => None,
      })
      .collect()
  }

  /// Each command as `verb target`, in order.
  fn verbs(plan: &Plan, live: &Live) -> Vec<String> {
    commands(plan, live)
      .iter()
      .filter_map(|command| {
        let (verb, target) = match command {
          Command::Add { target, .. } => ("add", target),
          Command::Update { target, .. } => ("update", target),
          Command::Move { target, .. } => ("move", target),
          Command::Remove { target } => ("remove", target),
          _ => return None,
        };
        Some(format!("{verb} {target}"))
      })
      .collect()
  }

  fn config(tasks: &[(&str, &str)]) -> Arc<Config> {
    let mut config = Config::make_default();
    config.tasks = tasks
      .iter()
      .map(|(path, yaml)| {
        let mut cfg: TaskConfig =
          spec(yaml).parse(path, std::path::Path::new("/p")).unwrap();
        cfg.source = Some(source(path));
        cfg
      })
      .collect();
    Arc::new(config)
  }

  /// The base: each entry, by key, with what it said.
  fn base(entries: &[(&str, &str)]) -> Base {
    entries
      .iter()
      .map(|(key, yaml)| (source(key), spec(yaml)))
      .collect()
  }

  /// A task at `path`, from the config entry `from` or the runner's own.
  fn task(
    id: usize,
    path: &str,
    here: &str,
    from: Option<&str>,
  ) -> TaskDescription {
    crate::test_util::task(id, path, spec(here), from.map(source))
  }

  /// What `reload` replies: the plan as the user sees it, and nothing of
  /// what carrying it out takes.
  #[test]
  fn a_reload_result_has_its_shape() {
    let text = concat!(
      r#"{"plan":{"changes":[{"kind":"changed","path":"api","#,
      r#""fields":[{"key":"cmd","base":["api"],"here":["api"],"#,
      r#""config":["api","--v2"],"conflict":false,"take":true}],"#,
      r#""apply":true},{"kind":"removed-here","#,
      r#""path":"gone","apply":false,"reason":"removed-here"},"#,
      r#"{"kind":"added","path":"new","apply":false,"#,
      r#""reason":{"in-the-way":"mine"}}],"#,
      r#""order":{"apply":true,"conflict":false},"#,
      r#""edited_here":["web"]},"applied":false,"errors":[]}"#
    );
    let result: ReloadResult = serde_json::from_str(text).unwrap();
    assert_eq!(serde_json::to_string(&result).unwrap(), text);
  }
  #[test]
  fn takes_what_changed_in_the_config_only() {
    let base = base(&[
      ("api", "{cmd: [api]}"),
      ("db", "{cmd: [pg]}"),
      ("old", "{cmd: [old]}"),
      ("gone", "{cmd: [gone]}"),
    ]);
    let live = live_with([
      task(1, "api", "{cmd: [api], cwd: /here}", Some("api")),
      task(2, "db", "{cmd: [pg], env: {A: '1'}}", Some("db")),
      task(3, "old", "{cmd: [old]}", Some("old")),
      task(4, "mine", "{cmd: [mine]}", None),
    ]);
    let config = config(&[
      ("api", "{cmd: [api, --v2]}"),
      ("db", "{cmd: [pg], env: {A: '2'}}"),
      ("new", "{cmd: [new]}"),
      ("gone", "{cmd: [gone]}"),
    ]);
    let plan = plan(config, &base, &live);
    let summary: Vec<(ChangeKind, &str, bool)> = plan
      .changes
      .iter()
      .map(|c| (c.kind, c.path.as_str(), c.apply))
      .collect();
    assert_eq!(
      summary,
      [
        (ChangeKind::Changed, "api", true),
        (ChangeKind::Changed, "db", false),
        (ChangeKind::Added, "new", true),
        (ChangeKind::RemovedHere, "gone", false),
        (ChangeKind::Removed, "old", true),
      ]
    );
    let api = &plan.changes[0];
    assert_eq!(api.fields.len(), 1, "{:?}", api.fields);
    assert_eq!(api.fields[0].key, "cmd");
    let db = &plan.changes[1];
    assert!(db.fields[0].conflict);
    assert_eq!(plan.edited_here, ["api"]);

    let commands = commands(&plan, &live);
    let update = commands
      .iter()
      .find_map(|c| match c {
        Command::Update { target, spec, .. } => Some((target, spec)),
        _ => None,
      })
      .unwrap();
    assert_eq!(update.0.to_string(), "api");
    // The config's cmd, and the cwd set here.
    assert_eq!(*update.1, spec("{cmd: [api, --v2], cwd: /here}"));
    assert!(commands.iter().any(|c| matches!(
      c,
      Command::Remove {
        target: Target::Id(TaskId(3))
      }
    )));
    assert!(commands.iter().any(|c| matches!(
      c,
      Command::Add { target, start: false, .. } if target.to_string() == "new"
    )));
    assert!(
      !commands
        .iter()
        .any(|c| matches!(c, Command::Add { target, .. } if target.to_string() == "gone"))
    );
  }

  /// What the config says as a task has it is no change: written back,
  /// by hand or by an agent, under the task's key, or at its path under
  /// another, which the task takes as its own. A task of the runner's own
  /// written in as it is becomes a config task when taken.
  #[test]
  fn what_the_config_says_as_here_is_no_change() {
    let base = base(&[("api", "{cmd: [api]}"), ("web", "{cmd: [old]}")]);
    let live = live_with([
      task(1, "api", "{cmd: [api, --v2]}", Some("api")),
      task(2, "web", "{cmd: [web]}", Some("web")),
      task(3, "build", "{cmd: [make]}", None),
      task(4, "mine", "{cmd: [mine]}", None),
    ]);
    let mut config = config(&[
      ("api", "{cmd: [api, --v2]}"),
      ("site", "{cmd: [web]}"),
      ("build", "{cmd: [make]}"),
      ("mine", "{cmd: [other]}"),
    ]);
    // `web` written back under another key, at the same path.
    let site = &mut Arc::get_mut(&mut config).unwrap().tasks[1];
    site.path = "web".into();
    let plan = plan(config, &base, &live);
    let changes: Vec<(ChangeKind, &str)> = plan
      .changes
      .iter()
      .map(|c| (c.kind, c.path.as_str()))
      .collect();
    assert_eq!(
      changes,
      [(ChangeKind::Written, "build"), (ChangeKind::Added, "mine")]
    );
    assert_eq!(
      plan.changes[1].reason,
      Some(Reason::InTheWay("mine".into()))
    );
    let adopted: Vec<(usize, &str)> = plan
      .adopt
      .iter()
      .map(|(id, s)| (id.0, s.key.as_str()))
      .collect();
    assert_eq!(adopted, [(2, "site")]);
    assert_eq!(
      verbs_and_origins(&plan, &live),
      ["origin {id: 2} site", "origin {id: 3} build"]
    );
  }

  fn verbs_and_origins(plan: &Plan, live: &Live) -> Vec<String> {
    commands(plan, live)
      .iter()
      .filter_map(|command| match command {
        Command::SetOrigin { target, origin } => Some(format!(
          "origin {target} {}",
          origin.as_ref().map_or("none", |o| o.key.as_str())
        )),
        _ => None,
      })
      .collect()
  }

  /// A task the config now has a dep on is there before the task that
  /// needs it is changed or added.
  #[test]
  fn new_deps_are_there_first() {
    let base = base(&[("api", "{cmd: [api]}"), ("old", "{cmd: [old]}")]);
    let live = live_with([
      task(1, "api", "{cmd: [api]}", Some("api")),
      task(2, "old", "{cmd: [old]}", Some("old")),
    ]);
    let config = config(&[
      ("web", "{cmd: [web], deps: [worker]}"),
      ("api", "{cmd: [api], deps: [cache]}"),
      ("worker", "{cmd: [worker]}"),
      ("cache", "{cmd: [cache]}"),
    ]);
    let plan = plan(config, &base, &live);
    assert_eq!(
      verbs(&plan, &live),
      [
        "remove {id: 2}",
        "add worker",
        "add cache",
        "add web",
        "update api"
      ]
    );
  }

  /// A dep the config names on a task moved here is on where it is now,
  /// as a move here renames the deps on it.
  #[test]
  fn deps_on_a_task_moved_here_follow_it() {
    let base = base(&[("db", "{cmd: [db]}"), ("web", "{cmd: [web]}")]);
    let live = live_with([
      task(1, "infra/db", "{cmd: [db]}", Some("db")),
      task(2, "web", "{cmd: [web]}", Some("web")),
    ]);
    let config = config(&[
      ("db", "{cmd: [db]}"),
      ("web", "{cmd: [web], deps: [db]}"),
      ("api", "{cmd: [api], deps: [db]}"),
    ]);
    let plan = plan(config, &base, &live);
    let deps: Vec<_> = commands(&plan, &live)
      .into_iter()
      .filter_map(|command| match command {
        Command::Add { spec, .. } | Command::Update { spec, .. } => {
          Some(spec.deps())
        }
        _ => None,
      })
      .collect();
    assert_eq!(deps, [["infra/db"], ["infra/db"]]);
  }

  /// A change that keeps deps from here keeps them as the removals and
  /// moves before it left them: none on a task the reload removes.
  #[test]
  fn an_update_keeps_deps_as_the_removals_left_them() {
    let base = base(&[
      ("api", "{cmd: [api], deps: [cache, db]}"),
      ("cache", "{cmd: [cache]}"),
      ("db", "{cmd: [db]}"),
    ]);
    let live = live_with([
      task(1, "api", "{cmd: [api], deps: [cache, db, x]}", Some("api")),
      task(2, "cache", "{cmd: [cache]}", Some("cache")),
      task(3, "db", "{cmd: [db]}", Some("db")),
    ]);
    let config = config(&[
      ("api", "{cmd: [api], deps: [db], env: {A: '1'}}"),
      ("db", "{cmd: [db]}"),
    ]);
    let plan = plan(config, &base, &live);
    let commands = commands(&plan, &live);
    let Some(Command::Update { spec, was, .. }) = commands
      .iter()
      .find(|command| matches!(command, Command::Update { .. }))
    else {
      panic!("{:?}", verbs(&plan, &live));
    };
    assert_eq!(spec.deps(), ["db", "x"]);
    assert_eq!(spec.value_at("env.A"), Some(&"1".into()));
    // What the kernel has once `cache` is gone.
    assert_eq!(
      was.as_ref().map(TaskSpec::deps),
      Some(vec!["db".into(), "x".into()])
    );
  }

  /// The fields taken go onto what the task has when the reload is
  /// carried out: a change made here since the plan stays.
  #[test]
  fn a_change_made_here_after_the_plan_stays() {
    let base = base(&[("api", "{cmd: [api]}")]);
    let at_plan = live_with([task(1, "api", "{cmd: [api]}", Some("api"))]);
    let config = config(&[("api", "{cmd: [api, --v2]}")]);
    let plan = plan(config, &base, &at_plan);
    let now =
      live_with([task(1, "api/v1", "{cmd: [api], cwd: /now}", Some("api"))]);
    let commands = commands(&plan, &now);
    let Some(Command::Update {
      target,
      spec: updated,
      was,
      ..
    }) = commands
      .iter()
      .find(|command| matches!(command, Command::Update { .. }))
    else {
      panic!("{:?}", verbs(&plan, &now));
    };
    assert_eq!(target.to_string(), "api/v1");
    assert_eq!(*updated, spec("{cmd: [api, --v2], cwd: /now}"));
    assert_eq!(was.as_ref(), Some(&now.tasks[0].spec.clone().unwrap()));
  }

  /// A new entry where a task gone from the config is, here a group by
  /// its name, comes in once that task is removed.
  #[test]
  fn a_task_gone_from_the_config_is_not_in_the_way() {
    let base = base(&[("web", "{cmd: [web]}")]);
    let live = live_with([task(1, "web", "{cmd: [web]}", Some("web"))]);
    let config = config(&[("web/api", "{cmd: [api]}")]);
    let plan = plan(config, &base, &live);
    assert!(
      plan.changes.iter().all(|c| c.apply && c.reason.is_none()),
      "{:?}",
      plan.changes
    );
    assert_eq!(verbs(&plan, &live), ["remove {id: 1}", "add web/api"]);

    // Kept, it is in the way again; taken again, it is not.
    let mut plan = plan;
    let removal = |plan: &Plan| {
      plan
        .changes
        .iter()
        .position(|c| c.kind == ChangeKind::Removed)
        .unwrap()
    };
    let added = |plan: &Plan| {
      let change = plan.changes.iter().find(|c| c.kind == ChangeKind::Added);
      let change = change.unwrap();
      (change.apply, change.reason.clone())
    };
    plan.set_apply(removal(&plan), false);
    assert_eq!(added(&plan), (false, Some(Reason::InTheWay("web".into()))));
    plan.set_apply(removal(&plan), true);
    assert_eq!(added(&plan), (true, None));
    // Left by hand, it stays left.
    let new = plan
      .changes
      .iter()
      .position(|c| c.kind == ChangeKind::Added);
    plan.set_apply(new.unwrap(), false);
    plan.set_apply(removal(&plan), false);
    plan.set_apply(removal(&plan), true);
    assert_eq!(added(&plan), (false, None));
    plan.set_apply(new.unwrap(), true);

    // All left, the removal with them; all taken, the new task too.
    plan.toggle_all();
    assert_eq!(added(&plan), (false, Some(Reason::InTheWay("web".into()))));
    plan.toggle_all();
    assert_eq!(added(&plan), (true, None));
  }

  /// Taking all leaves a new task that a task here stays in the way of.
  #[test]
  fn taking_all_leaves_what_is_in_the_way() {
    let live = live_with([
      task(1, "api", "{cmd: [api]}", Some("api")),
      task(2, "new", "{cmd: [mine]}", None),
    ]);
    let config =
      config(&[("api", "{cmd: [api, --v2]}"), ("new", "{cmd: [new]}")]);
    let mut plan = plan(config, &base(&[("api", "{cmd: [api]}")]), &live);
    let taken = |plan: &Plan| -> Vec<bool> {
      plan.changes.iter().map(|c| c.apply).collect()
    };
    assert_eq!(taken(&plan), [true, false]);
    plan.toggle_all();
    assert_eq!(taken(&plan), [false, false]);
    plan.toggle_all();
    assert_eq!(taken(&plan), [true, false]);
    assert_eq!(plan.changes[1].reason, Some(Reason::InTheWay("new".into())));
  }

  /// An entry at a task's path under a new key, or in another file, is
  /// that task's: changed, not removed and added again.
  #[test]
  fn an_entry_under_a_new_key_is_the_same_task() {
    let base = base(&[("old", "{cmd: [web]}")]);
    let live = live_with([task(1, "web", "{cmd: [web]}", Some("old"))]);
    let mut config = config(&[("web", "{cmd: [web, --v2]}")]);
    let entry = &mut Arc::get_mut(&mut config).unwrap().tasks[0];
    entry.source = Some(Source {
      file: "web/tasks.yaml".into(),
      key: "web".into(),
      mount: String::new(),
    });
    let plan = plan(config, &base, &live);
    let summary: Vec<(ChangeKind, &str)> = plan
      .changes
      .iter()
      .map(|c| (c.kind, c.path.as_str()))
      .collect();
    assert_eq!(summary, [(ChangeKind::Changed, "web")]);
    assert_eq!(plan.adopt[0].1.file, "web/tasks.yaml");
  }

  /// New tasks are listed where the config has them, though one is
  /// added after a task it needs.
  #[tokio::test]
  async fn new_tasks_are_listed_in_the_config_order() {
    let kernel = crate::kernel::kernel::Kernel::new();
    let pc = kernel.context();
    let handle = tokio::spawn(kernel.run());
    let project = Project::new(Arc::new(Config::make_default()));
    let config = config(&[
      ("web", "{cmd: [web], deps: [worker]}"),
      ("worker", "{cmd: [worker]}"),
    ]);
    let mut plan = plan(config, &project.base(), &Live::read(&pc).await);
    assert!(apply(&pc, &project, &mut plan).await.is_empty());
    let listed: Vec<String> = describe(&pc)
      .await
      .iter()
      .filter_map(|task| Some(task.info.path.as_ref()?.to_string()))
      .collect();
    assert_eq!(listed, ["web", "worker"]);
    pc.send(crate::kernel::kernel_message::KernelCommand::Quit);
    handle.await.unwrap();
  }

  /// A new task not added, left out or failing, is offered again: its
  /// entry stays out of the base, so it is still new.
  #[tokio::test]
  async fn a_new_task_not_added_is_offered_again() {
    let kernel = crate::kernel::kernel::Kernel::new();
    let pc = kernel.context();
    let handle = tokio::spawn(kernel.run());
    let in_the_way = Command::Add {
      target: Target::glob("new"),
      spec: spec("{cmd: [sleep, '60']}"),
      place: Default::default(),
      start: false,
      defaults: true,
      origin: None,
    };
    let none = Config::make_default();
    execute(&pc, &none, &in_the_way).await.unwrap();
    let project = Project::new(Arc::new(none));
    let config = config(&[("new", "{cmd: [new]}")]);
    let mut plan = plan(config, &project.base(), &Live::read(&pc).await);
    assert_eq!(plan.changes[0].reason, Some(Reason::InTheWay("new".into())));
    assert!(apply(&pc, &project, &mut plan).await.is_empty());
    assert_eq!(project.base(), Base::default());
    plan.changes[0].set_apply(true);
    let errors = apply(&pc, &project, &mut plan).await;
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert!(plan.changes[0].failed);
    assert_eq!(project.base(), Base::default());
    pc.send(crate::kernel::kernel_message::KernelCommand::Quit);
    handle.await.unwrap();
  }

  /// A task whose change fails keeps its entry in the base as it was:
  /// the change is offered again, and removed here later, it is removed
  /// here, not new again. A plan carried out again, as by two reloads at
  /// once, adds nothing twice.
  #[tokio::test]
  async fn a_task_here_keeps_its_entry_whatever_fails() {
    let kernel = crate::kernel::kernel::Kernel::new();
    let pc = kernel.context();
    let handle = tokio::spawn(kernel.run());
    let project = Project::new(Arc::new(Config::make_default()));
    let v1 = config(&[("api", "{cmd: [api]}")]);
    let mut first = plan(v1, &project.base(), &Live::read(&pc).await);
    let mut again = first.clone();
    assert!(apply(&pc, &project, &mut first).await.is_empty());
    assert!(apply(&pc, &project, &mut again).await.is_empty());
    assert_eq!(describe(&pc).await.len(), 1);

    // A dep on what is not there: the update is refused.
    let v2 = config(&[("api", "{cmd: [api], deps: [ghost]}")]);
    let mut second = plan(v2, &project.base(), &Live::read(&pc).await);
    let errors = apply(&pc, &project, &mut second).await;
    assert_eq!(errors.len(), 1, "{errors:?}");
    // What it said before: the change is offered again.
    assert_eq!(project.base(), base(&[("api", "{cmd: [api]}")]));
    pc.send(crate::kernel::kernel_message::KernelCommand::Quit);
    handle.await.unwrap();
  }

  /// A reload carried out records the config in the base, but a new task
  /// left out stays new, and an entry whose change failed says what it
  /// said: each is offered again.
  #[test]
  fn what_a_reload_leaves_in_the_base() {
    let base = base(&[
      ("api", "{cmd: [api]}"),
      ("gone", "{cmd: [gone]}"),
      ("old", "{cmd: [old]}"),
    ]);
    let live = live_with([
      task(1, "api", "{cmd: [api]}", Some("api")),
      task(2, "old", "{cmd: [old]}", Some("old")),
    ]);
    let config = config(&[
      ("api", "{cmd: [api, --v2]}"),
      ("gone", "{cmd: [gone]}"),
      ("new", "{cmd: [new]}"),
      ("skip", "{cmd: [skip]}"),
    ]);
    let mut plan = plan(config, &base, &live);
    let at = |plan: &Plan, path: &str| {
      plan.changes.iter().position(|c| c.path == path).unwrap()
    };
    let skip = at(&plan, "skip");
    plan.set_apply(skip, false);
    let gone = at(&plan, "gone");
    plan.set_apply(gone, true);
    for path in ["api", "gone", "old"] {
      let change = at(&plan, path);
      plan.changes[change].failed = true;
    }
    let left = carried_out(&plan);
    let entries: Vec<(&str, Option<Vec<String>>)> = left
      .sources()
      .map(|s| (s.key.as_str(), left.get(s).and_then(TaskSpec::argv)))
      .collect();
    let argv = |arg: &str| Some(vec![arg.to_string()]);
    assert_eq!(
      entries,
      [
        ("api", argv("api")),
        ("gone", argv("gone")),
        ("new", argv("new")),
        ("old", argv("old")),
      ]
    );
  }

  #[test]
  fn order_follows_the_config_when_only_it_changed() {
    let base = base(&[("a", "{cmd: [a]}"), ("b", "{cmd: [b]}")]);
    let live = live_with([
      task(1, "a", "{cmd: [a]}", Some("a")),
      task(2, "b", "{cmd: [b]}", Some("b")),
    ]);
    let config = config(&[("b", "{cmd: [b]}"), ("a", "{cmd: [a]}")]);
    let mut plan = plan(config, &base, &live);
    assert!(plan.order.as_ref().is_some_and(|o| o.apply));
    assert!(plan.adopt.is_empty(), "{:?}", plan.adopt);
    // Leaving all leaves the order too.
    plan.toggle_all();
    assert!(plan.order.as_ref().is_some_and(|o| !o.apply));
    plan.toggle_all();
    let moves: Vec<String> = commands(&plan, &live)
      .into_iter()
      .filter_map(|c| match c {
        Command::Move { target, place, .. } => {
          Some(format!("{target} {place:?}"))
        }
        _ => None,
      })
      .collect();
    assert_eq!(moves, ["b First", "a After(TaskPath(\"b\"))"]);
  }

  /// A config with these folder entries, `(key, collapsed, at)`, beside
  /// its tasks.
  fn with_folders(
    mut config: Arc<Config>,
    folders: &[(&str, bool, usize)],
  ) -> Arc<Config> {
    Arc::get_mut(&mut config).unwrap().folders = folders
      .iter()
      .map(|(key, collapsed, at)| crate::config::folder::FolderConfig {
        path: key.trim_end_matches('/').to_string(),
        collapsed: *collapsed,
        source: Some(source(key)),
        at: *at,
      })
      .collect();
    config
  }

  fn folder_here(
    id: usize,
    path: &str,
    collapsed: bool,
    from: Option<&str>,
  ) -> FolderInfo {
    FolderInfo {
      id: TaskId(id),
      space: TaskSpaceId::default_space(),
      path: TaskPath::new(path).unwrap(),
      after: None,
      settings: FolderSettings {
        collapsed,
        origin: from.map(source),
      },
    }
  }

  /// What the steps do to the folders, in order.
  fn folder_steps(plan: &Plan, live: &Live) -> Vec<String> {
    let origin = |s: &FolderSettings| {
      s.origin
        .as_ref()
        .map_or("own".to_string(), |o| o.key.clone())
    };
    steps(plan, live)
      .into_iter()
      .filter_map(|step| match step.act {
        Act::Command(_) => None,
        Act::AddFolder(path, place, s) => Some(format!(
          "add {path}/ {place:?} {} {}",
          s.collapsed,
          origin(&s)
        )),
        Act::DropFolder(id) => Some(format!("drop {}", id.0)),
        Act::SetFolder(id, s) => {
          Some(format!("set {} {} {}", id.0, s.collapsed, origin(&s)))
        }
      })
      .collect()
  }

  /// A folder entry comes, goes, and changes as a task's does; one that
  /// goes takes only the folder away.
  #[test]
  fn folders_follow_their_entries() {
    let base = base(&[
      ("tools/", "{}"),
      ("old/", "{}"),
      ("gone/", "{}"),
      ("db", "{cmd: [db]}"),
    ]);
    let mut live = live_with([task(1, "db", "{cmd: [db]}", Some("db"))]);
    live.folders = vec![
      folder_here(2, "tools", false, Some("tools/")),
      folder_here(3, "old", true, Some("old/")),
      folder_here(4, "svc", false, Some("svc/")),
      folder_here(5, "mine", false, None),
    ];
    let config = with_folders(
      config(&[("db", "{cmd: [db]}")]),
      &[
        ("apps/", true, 0),
        ("tools/", true, 1),
        ("gone/", false, 1),
        ("services/", false, 1),
        ("mine/", false, 1),
      ],
    );
    // `svc/` written back under another key: the folder takes it.
    let config = {
      let mut config = config;
      let services = &mut Arc::get_mut(&mut config).unwrap().folders[3];
      services.path = "svc".into();
      config
    };
    let plan = plan(config, &base, &live);
    let changes: Vec<(ChangeKind, &str, bool)> = plan
      .changes
      .iter()
      .map(|c| (c.kind, c.path.as_str(), c.apply))
      .collect();
    assert_eq!(
      changes,
      [
        (ChangeKind::Added, "apps/", true),
        (ChangeKind::Changed, "tools/", true),
        (ChangeKind::RemovedHere, "gone/", false),
        (ChangeKind::Written, "mine/", true),
        (ChangeKind::Removed, "old/", true),
      ]
    );
    assert_eq!(
      folder_steps(&plan, &live),
      [
        "set 4 false services/",
        "drop 3",
        "set 5 false mine/",
        "add apps/ First true apps/",
        "set 2 true tools/",
      ]
    );

    // Kept: the runner's own from now on.
    let mut plan = plan;
    let old = plan.changes.iter().position(|c| c.path == "old/").unwrap();
    plan.set_apply(old, false);
    assert!(folder_steps(&plan, &live).contains(&"set 3 true own".to_string()));
    let left = carried_out(&plan);
    let keys: Vec<&str> = left.sources().map(|s| s.key.as_str()).collect();
    assert_eq!(
      keys,
      ["apps/", "db", "tools/", "gone/", "services/", "mine/"]
    );
    assert!(crate::config::folder::collapsed(
      left.get(&source("tools/")).unwrap()
    ));
  }

  /// A new folder is listed where the config has it, and a reload that
  /// drops its entry leaves its tasks where they are.
  #[tokio::test]
  async fn a_reload_brings_folders_in_and_out() {
    let kernel = crate::kernel::kernel::Kernel::new();
    let pc = kernel.context();
    let handle = tokio::spawn(kernel.run());
    let project = Project::new(Arc::new(Config::make_default()));
    let v1 = with_folders(
      config(&[("db", "{cmd: [db]}"), ("apps/web", "{cmd: [web]}")]),
      &[("empty/", false, 1), ("apps/", true, 1)],
    );
    let mut first = plan(v1, &project.base(), &Live::read(&pc).await);
    assert!(apply(&pc, &project, &mut first).await.is_empty());
    let live = Live::read(&pc).await;
    let folders: Vec<(String, bool)> = live
      .folders
      .iter()
      .map(|f| (f.path.to_string(), f.settings.collapsed))
      .collect();
    assert_eq!(
      folders,
      [("empty".to_string(), false), ("apps".to_string(), true)]
    );
    let nodes = pc.folders(TaskSpaceId::default_space()).await;
    assert!(nodes.iter().all(|f| f.settings.origin.is_some()));

    let v2 = config(&[("db", "{cmd: [db]}"), ("apps/web", "{cmd: [web]}")]);
    let mut second = plan(v2, &project.base(), &Live::read(&pc).await);
    assert_eq!(second.len(), 2, "{:?}", second.changes);
    assert!(apply(&pc, &project, &mut second).await.is_empty());
    let live = Live::read(&pc).await;
    assert!(live.folders.is_empty());
    let listed: Vec<String> = live
      .tasks
      .iter()
      .filter_map(|task| Some(task.info.path.as_ref()?.to_string()))
      .collect();
    assert_eq!(listed, ["db", "apps/web"]);
    assert!(!project.base().sources().any(Source::is_folder));
    pc.send(crate::kernel::kernel_message::KernelCommand::Quit);
    handle.await.unwrap();
  }

  /// A new folder comes before the new tasks under it, so they are
  /// listed in it where the config has it.
  #[tokio::test]
  async fn a_new_folder_comes_before_its_tasks() {
    let kernel = crate::kernel::kernel::Kernel::new();
    let pc = kernel.context();
    let handle = tokio::spawn(kernel.run());
    let project = Project::new(Arc::new(Config::make_default()));
    let v1 = config(&[("a", "{cmd: [a]}"), ("b", "{cmd: [b]}")]);
    let mut first = plan(v1, &project.base(), &Live::read(&pc).await);
    assert!(apply(&pc, &project, &mut first).await.is_empty());
    let v2 = with_folders(
      config(&[
        ("a", "{cmd: [a]}"),
        ("f/x", "{cmd: [x]}"),
        ("b", "{cmd: [b]}"),
      ]),
      &[("f/", true, 1)],
    );
    let mut second = plan(v2, &project.base(), &Live::read(&pc).await);
    assert!(apply(&pc, &project, &mut second).await.is_empty());
    let live = Live::read(&pc).await;
    let listed: Vec<String> = live
      .tasks
      .iter()
      .filter_map(|task| Some(task.info.path.as_ref()?.to_string()))
      .collect();
    assert_eq!(listed, ["a", "f/x", "b"]);
    let a = live.tasks[0].info.id;
    assert_eq!(live.folders[0].after, Some(a));
    pc.send(crate::kernel::kernel_message::KernelCommand::Quit);
    handle.await.unwrap();
  }

  /// A folder kept here takes its entry back at its path, and one from
  /// an entry gone takes the entry at its path; either is in the way of
  /// a new task at it.
  #[test]
  fn folders_here_take_entries_or_are_in_the_way() {
    let mut live = live_with([]);
    live.folders = vec![
      folder_here(1, "apps", true, None),
      folder_here(2, "tools", false, Some("old/")),
    ];
    let config = with_folders(
      config(&[("tools", "{cmd: [t]}")]),
      &[("apps/", false, 0), ("tools/", false, 1)],
    );
    let plan = plan(config, &Base::default(), &live);
    let changes: Vec<(ChangeKind, &str, Option<String>)> = plan
      .changes
      .iter()
      .map(|c| (c.kind, c.path.as_str(), c.reason.as_ref().map(Reason::text)))
      .collect();
    assert_eq!(
      changes,
      [
        (
          ChangeKind::Added,
          "tools",
          Some("tools/ is in the way".into())
        ),
        (ChangeKind::Written, "apps/", None),
      ]
    );
    assert_eq!(
      folder_steps(&plan, &live),
      ["set 2 false tools/", "set 1 false apps/"]
    );
  }

  /// A config that only moves a folder among its tasks is a new order.
  #[test]
  fn a_folder_moved_in_the_config_is_a_new_order() {
    let base = base(&[("db", "{cmd: [db]}"), ("tools/", "{}")]);
    let mut live = live_with([task(1, "db", "{cmd: [db]}", Some("db"))]);
    let mut tools = folder_here(2, "tools", false, Some("tools/"));
    tools.after = Some(TaskId(1));
    live.folders = vec![tools];
    let config =
      with_folders(config(&[("db", "{cmd: [db]}")]), &[("tools/", false, 0)]);
    let plan = plan(config, &base, &live);
    assert!(plan.changes.is_empty(), "{:?}", plan.changes);
    let order = plan.order.as_ref().expect("a new order");
    assert!(order.apply && !order.conflict);
    let moves: Vec<String> = commands(&plan, &live)
      .iter()
      .filter_map(|command| match command {
        Command::Move { target, place, .. } => {
          Some(format!("{target} {place:?}"))
        }
        _ => None,
      })
      .collect();
    assert_eq!(moves, ["tools First", "db After(TaskPath(\"tools\"))"]);
  }
}
