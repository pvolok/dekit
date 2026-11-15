//! The runners a console shows: each one's tasks in the list, under ids
//! of the list's own, and under a group named for the runner when the
//! list shows every runner; the others on this machine found as they
//! start; and what the config of each would change, kept current.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::mpsc::UnboundedSender;

use crate::config::config::Config;
use crate::console::{
  link::{Link, RunnerEvent, is_code, socket_connector},
  state::ConfigStatus,
  task_tree::Node,
  task_view::TaskView,
  ui_header::runner_name,
};
use crate::kernel::{
  task::{ExitInfo, TaskId, TaskKind, TaskState},
  task_path::{TaskPath, path_name, unique},
};
use crate::protocol::{
  RpcRequest, RpcState, RpcTaskInfo, codes,
  rpc::{RpcListed, WriteBackResult},
};
use crate::reload::ReloadResult;
use crate::runner::{RunnerSpec, lockfile};
use crate::target::Target;

use super::{App, AppMsg, list_path};

/// How often the console looks for runners started since.
const TICK: Duration = Duration::from_secs(2);
/// How long a change to the config's files settles before it is read:
/// an editor's save can be several writes. A burst of changes to the
/// tasks is compared once too.
const SETTLE: Duration = Duration::from_millis(50);
/// How long a runner that could not be shown waits to be tried again.
const RETRY: Duration = Duration::from_secs(30);

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct RunnerId(pub(crate) u64);

pub struct Runner {
  pub id: RunnerId,
  pub link: Link,
  /// The group its tasks are under; empty when its tasks are at the top.
  pub name: String,
  /// The folder in the list that its group is, while it has one.
  pub folder: Option<TaskId>,
  /// The list's id of each of its tasks and folders, by its own.
  local: HashMap<TaskId, TaskId>,
  /// Its list order, kept as the runner says it changes.
  order: Order,
  /// What its config would change, by the list's ids.
  pub status: ConfigStatus,
  status_dirty: bool,
  status_running: bool,
  /// Its list came in once.
  listed: bool,
}

/// A runner's list order, by its own ids: what each task and folder is
/// listed right after, none for the first.
#[derive(Default)]
struct Order(HashMap<TaskId, Option<TaskId>>);

impl Order {
  fn after(&self, id: TaskId) -> Option<Option<TaskId>> {
    self.0.get(&id).copied()
  }

  /// Lists `id` right after `after`, as the runner does: what was listed
  /// right after `after` is listed after `id` now.
  fn place(&mut self, id: TaskId, after: Option<TaskId>) {
    self.unplace(id);
    if let Some(next) = self.next_of(after) {
      self.0.insert(next, Some(id));
    }
    self.0.insert(id, after);
  }

  /// Takes `id` out: what was listed right after it is listed after what
  /// it was.
  fn unplace(&mut self, id: TaskId) {
    let Some(was) = self.0.remove(&id) else {
      return;
    };
    if let Some(next) = self.next_of(Some(id)) {
      self.0.insert(next, was);
    }
  }

  /// What is listed right after `after`, or first for none.
  fn next_of(&self, after: Option<TaskId>) -> Option<TaskId> {
    (self.0.iter())
      .find(|(_, listed)| **listed == after)
      .map(|(id, _)| *id)
  }
}

impl Runner {
  /// Its id for a task or folder the list has.
  pub fn remote_of(&self, local: TaskId) -> Option<TaskId> {
    self
      .local
      .iter()
      .find(|(_, id)| **id == local)
      .map(|(remote, _)| *remote)
  }
}

impl App {
  pub(super) fn runner(&self, id: RunnerId) -> Option<&Runner> {
    self.runners.iter().find(|runner| runner.id == id)
  }

  fn runner_mut(&mut self, id: RunnerId) -> Option<&mut Runner> {
    self.runners.iter_mut().find(|runner| runner.id == id)
  }

  fn alloc(&mut self) -> TaskId {
    self.next_id += 1;
    TaskId(self.next_id)
  }

  /// Shows a runner: last in the list, its group collapsed unless it is
  /// the first.
  pub(super) fn add_runner(&mut self, link: Link) -> RunnerId {
    self.next_runner += 1;
    let id = RunnerId(self.next_runner);
    if let Some(spec) = link.runner().cloned() {
      // A runner the console shows is a project used recently.
      tokio::task::spawn_blocking(move || crate::runner::recent::touch(&spec));
    }
    let (name, folder) = match self.all_runners {
      true => {
        let base = link
          .runner()
          .map_or("runner".to_string(), |spec| path_name(&runner_name(spec)));
        let names: Vec<&str> =
          self.runners.iter().map(|r| r.name.as_str()).collect();
        let name = unique(&base, |name| names.contains(&name));
        (name, Some(self.alloc()))
      }
      false => (String::new(), None),
    };
    if let (Some(folder), Ok(path)) = (folder, TaskPath::new(name.as_str())) {
      let after = self.runners.iter().rev().find_map(|runner| runner.folder);
      let collapsed = !self.runners.is_empty();
      self.state.tasks.add_folder(folder, path, after, collapsed);
    }
    self.runners.push(Runner {
      id,
      link: link.clone(),
      name,
      folder,
      local: HashMap::new(),
      order: Order::default(),
      status: ConfigStatus::default(),
      status_dirty: true,
      status_running: false,
      listed: false,
    });
    let tx = self.tx.clone();
    link.watch(move |event| {
      let _ = tx.send(AppMsg::Runner(id, event));
    });
    id
  }

  /// Stops showing a runner.
  fn drop_runner(&mut self, id: RunnerId) {
    let Some(at) = self.runners.iter().position(|runner| runner.id == id)
    else {
      return;
    };
    let runner = self.runners.remove(at);
    for local in runner.local.values() {
      self.state.tasks.remove(*local);
    }
    if let Some(folder) = runner.folder {
      self.state.tasks.remove(folder);
    }
    self.sync_status();
  }

  /// The runner the selection is in, or the first.
  pub(super) fn focused(&self) -> Option<&Runner> {
    let selected = match self.state.tasks.selected() {
      Some((_, Node::Task(task))) => Some(task.runner),
      Some((path, Node::Group(_))) => {
        self.runner_at(path.as_str()).map(|(runner, _)| runner)
      }
      None => None,
    };
    selected
      .and_then(|id| self.runner(id))
      .or(self.runners.first())
  }

  /// The runner a path in the list is in, and the path on it: none for
  /// its group.
  pub(super) fn runner_at(
    &self,
    path: &str,
  ) -> Option<(RunnerId, Option<String>)> {
    if !self.all_runners {
      let runner = self.runners.first()?;
      return Some((runner.id, (!path.is_empty()).then(|| path.to_string())));
    }
    let (name, rest) = match path.split_once('/') {
      Some((name, rest)) => (name, Some(rest.to_string())),
      None => (path, None),
    };
    let runner = self.runners.iter().find(|runner| runner.name == name)?;
    Some((runner.id, rest))
  }

  /// The paths of a runner's tasks, as it has them.
  pub(super) fn remote_paths(&self, runner: RunnerId) -> Vec<String> {
    self
      .state
      .tasks
      .iter()
      .filter(|task| task.runner == runner)
      .filter_map(|task| self.remote_path(task))
      .collect()
  }

  /// The task's path on its runner.
  pub(super) fn remote_path(&self, task: &TaskView) -> Option<String> {
    let runner = self.runner(task.runner)?;
    match runner.name.is_empty() {
      true => Some(task.path.to_string()),
      false => task
        .path
        .as_str()
        .strip_prefix(&format!("{}/", runner.name))
        .map(str::to_string),
    }
  }

  /// Where a runner's path is in the list.
  pub(super) fn local_path(
    &self,
    runner: RunnerId,
    remote: &str,
  ) -> Option<TaskPath> {
    list_path(&self.runner(runner)?.name, remote)
  }

  /// The selected task, or every task in the selected group, on its
  /// runner.
  pub(super) fn selected_target(&self) -> Option<(RunnerId, Target)> {
    match self.state.tasks.selected()? {
      (_, Node::Task(task)) => Some((task.runner, Target::Id(task.remote))),
      (path, Node::Group(_)) => {
        let (runner, rest) = self.runner_at(path.as_str())?;
        let glob = match rest {
          Some(rest) => format!("{rest}/**"),
          None => "**".to_string(),
        };
        Some((runner, Target::glob(&glob)))
      }
    }
  }

  /// The list's id for what a runner lists after: in the runner's own
  /// group, its folder when it is listed first there.
  fn local_after(
    &self,
    runner: &Runner,
    after: Option<TaskId>,
  ) -> Option<TaskId> {
    match after {
      Some(after) => runner.local.get(&after).copied().or(runner.folder),
      None => runner.folder,
    }
  }

  pub(super) fn runner_event(&mut self, id: RunnerId, event: RunnerEvent) {
    if self.runner(id).is_none() {
      return;
    }
    match event {
      RunnerEvent::Tasks(tasks) => {
        self.replace_tasks(id, tasks.tasks, tasks.folders);
        self.touch_status(id);
      }
      RunnerEvent::Task(listed) => self.upsert_task(id, listed),
      RunnerEvent::Removed(remote) => {
        let local = self.runner_mut(id).and_then(|runner| {
          runner.order.unplace(remote);
          runner.local.remove(&remote)
        });
        if let Some(local) = local {
          self.state.tasks.remove(local);
        }
        self.touch_status(id);
      }
      RunnerEvent::Folder(folder) => self.upsert_folder(id, folder),
      RunnerEvent::ConfigChanged => self.touch_status(id),
      RunnerEvent::Lost(why) => {
        let name = self
          .runner(id)
          .and_then(|runner| runner.link.runner())
          .map_or("the runner".to_string(), runner_name);
        self.drop_runner(id);
        if self.runners.is_empty() {
          return self
            .exit(Some(why.unwrap_or_else(|| "the runner stopped".into())));
        }
        match why {
          Some(why) => self.toast(format!("Lost {name}: {why}"), true),
          None => self.toast(format!("{name} stopped."), false),
        }
      }
    }
  }

  fn add_task(&mut self, id: RunnerId, listed: RpcListed) {
    let RpcListed { info, after } = listed;
    let Some(path) = self.local_path(id, &info.path) else {
      log::warn!("no place in the list for '{}'", info.path);
      return;
    };
    let Some(runner) = self.runner(id) else {
      return;
    };
    let remote_after = after;
    let after = self.local_after(runner, after);
    let local = self.alloc();
    let view = TaskView {
      id: local,
      runner: id,
      remote: info.id,
      label: info.label,
      path: Arc::new(path),
      kind: task_kind(info.kind.as_deref()),
      status: task_state(&info.state),
      spec: info.spec,
      origin: info.origin,
    };
    if let Some(runner) = self.runner_mut(id) {
      runner.local.insert(info.id, local);
      runner.order.place(info.id, remote_after);
    }
    self.state.tasks.add(view, after);
  }

  /// A task the runner lists: added when it is new, else changed, and
  /// moved when it is listed elsewhere now.
  fn upsert_task(&mut self, id: RunnerId, listed: RpcListed) {
    let Some(runner) = self.runner(id) else {
      return;
    };
    let Some(&local) = runner.local.get(&listed.info.id) else {
      self.add_task(id, listed);
      return self.touch_status(id);
    };
    let RpcListed { info, after } = listed;
    let path = self.local_path(id, &info.path);
    let shown = self.state.tasks.task(local).map(|task| task.path.clone());
    let moved = runner.order.after(info.id) != Some(after)
      || path.as_ref() != shown.as_deref();
    if moved && let Some(path) = path {
      let to = self.local_after(runner, after);
      self.state.tasks.move_task(local, path, to);
      if let Some(runner) = self.runner_mut(id) {
        runner.order.place(info.id, after);
      }
      self.touch_status(id);
    }
    self.change_task(id, info);
  }

  /// A folder the runner lists: added when it is new, else moved.
  fn upsert_folder(
    &mut self,
    id: RunnerId,
    folder: crate::protocol::rpc::RpcFolder,
  ) {
    let Some(path) = self.local_path(id, &folder.path) else {
      return;
    };
    let Some(runner) = self.runner(id) else {
      return;
    };
    let after = self.local_after(runner, folder.after);
    match runner.local.get(&folder.id).copied() {
      Some(local) => self.state.tasks.move_task(local, path, after),
      None => {
        let local = self.alloc();
        self
          .state
          .tasks
          .add_folder(local, path, after, folder.collapsed);
        if let Some(runner) = self.runner_mut(id) {
          runner.local.insert(folder.id, local);
        }
      }
    }
    if let Some(runner) = self.runner_mut(id) {
      runner.order.place(folder.id, folder.after);
    }
    self.touch_status(id);
  }

  fn change_task(&mut self, id: RunnerId, info: RpcTaskInfo) {
    let Some(local) = self.runner(id).and_then(|r| r.local.get(&info.id))
    else {
      return;
    };
    let local = *local;
    let mut redefined = false;
    if let Some(task) = self.state.tasks.task_mut(local) {
      redefined = task.spec != info.spec
        || task.origin != info.origin
        || task.label != info.label;
      task.label = info.label;
      task.kind = task_kind(info.kind.as_deref());
      task.spec = info.spec;
      task.origin = info.origin;
    }
    self.state.tasks.set_status(local, task_state(&info.state));
    if redefined {
      self.touch_status(id);
    }
  }

  /// The runner's list as it is, in place of what was known of it: the
  /// groups stay open or closed as they were, and the selection where
  /// it was.
  fn replace_tasks(
    &mut self,
    id: RunnerId,
    tasks: Vec<RpcListed>,
    folders: Vec<crate::protocol::rpc::RpcFolder>,
  ) {
    let selected = self
      .state
      .tasks
      .selected()
      .map(|(path, _)| path.to_string());
    let Some(runner) = self.runner(id) else {
      return;
    };
    let prefix = match runner.name.is_empty() {
      true => String::new(),
      false => format!("{}/", runner.name),
    };
    let old: Vec<TaskId> = runner.local.values().copied().collect();
    self.state.tasks.seed(&prefix);
    for local in old {
      self.state.tasks.remove(local);
    }
    if let Some(runner) = self.runner_mut(id) {
      runner.local.clear();
      runner.order = Order::default();
    }
    for listed in in_list_order(tasks, folders) {
      match listed {
        Listed::Task(listed) => self.add_task(id, *listed),
        Listed::Folder(folder) => self.upsert_folder(id, folder),
      }
    }
    if let Some(path) = selected {
      self.state.tasks.select_path(&path);
    }
    // The console opens on the first task of the runner it opened on.
    let first = self.runners.first().is_some_and(|runner| runner.id == id);
    let Some(runner) = self.runner_mut(id) else {
      return;
    };
    let listed = std::mem::replace(&mut runner.listed, true);
    let on_group = matches!(
      self.state.tasks.selected(),
      None | Some((_, Node::Group(_)))
    );
    if first && !listed && on_group {
      self.state.tasks.select_task(0);
    }
  }

  /// Something the config is compared with changed on a runner.
  fn touch_status(&mut self, id: RunnerId) {
    if let Some(runner) = self.runner_mut(id) {
      runner.status_dirty = true;
    }
  }

  /// Compares each runner's config with its tasks again once its files or
  /// the tasks changed; at most one comparison at a time per runner.
  pub(super) fn refresh_status(&mut self) {
    let mut asked = Vec::new();
    for runner in &mut self.runners {
      if !runner.status_dirty
        || runner.status_running
        || runner.link.runner().is_none()
      {
        continue;
      }
      runner.status_dirty = false;
      runner.status_running = true;
      asked.push((runner.id, runner.link.clone()));
    }
    for (id, link) in asked {
      self.spawn(async move {
        tokio::time::sleep(SETTLE).await;
        let status = status(&link).await;
        move |app: &mut App| app.status_in(id, status)
      });
    }
  }

  fn status_in(&mut self, id: RunnerId, status: anyhow::Result<RemoteStatus>) {
    let Some(runner) = self.runner(id) else {
      return;
    };
    let local = |remote: &TaskId| runner.local.get(remote).copied();
    let status = match status {
      Ok(status) => ConfigStatus {
        broken: false,
        changes: status.changes,
        edited: status.edited.iter().filter_map(local).collect(),
        removed: status.removed,
        own: HashSet::new(),
      },
      Err(err) => {
        if !is_code(&err, codes::BAD_CONFIG) {
          log::debug!("no config status: {err:#}");
        }
        ConfigStatus {
          broken: is_code(&err, codes::BAD_CONFIG),
          ..ConfigStatus::default()
        }
      }
    };
    if let Some(runner) = self.runner_mut(id) {
      runner.status = status;
      runner.status_running = false;
    }
    self.sync_status();
  }

  /// The status shown: the marks of every runner, and the counts of the
  /// one in focus.
  pub(super) fn sync_status(&mut self) {
    let own = (self.state.tasks.iter())
      .filter(|task| task.origin.is_none() && task.spec.is_some())
      .filter(|task| {
        self
          .runner(task.runner)
          .is_some_and(|runner| runner.link.runner().is_some())
      })
      .map(|task| task.id)
      .collect();
    let edited = (self.runners.iter())
      .flat_map(|runner| runner.status.edited.iter().copied())
      .collect();
    let focused = self.focused();
    self.state.config = ConfigStatus {
      broken: focused.is_some_and(|runner| runner.status.broken),
      changes: focused.map_or(0, |runner| runner.status.changes),
      removed: focused.map_or(0, |runner| runner.status.removed),
      edited,
      own,
    };
    self.dirty = true;
  }

  /// Shows the runner at `root`: its group, when it is shown already;
  /// otherwise it is connected to, started if it is not running.
  pub(super) fn switch_runner(&mut self, kind: &str, root: &str) {
    let Some(kind) = crate::runner::RunnerKind::from_name(kind) else {
      return self.toast(format!("No runner kind '{kind}'."), true);
    };
    let spec = match RunnerSpec::exact(kind, std::path::Path::new(root)) {
      Ok(spec) => spec,
      Err(err) => return self.toast(format!("{err:#}"), true),
    };
    if let Some(runner) =
      (self.runners.iter()).find(|runner| runner.link.runner() == Some(&spec))
    {
      let name = runner.name.clone();
      self.state.tasks.select_path(&name);
      self.state.tasks.expand();
      return;
    }
    self.connect(spec, true);
  }

  /// Connects to a runner off the loop, and shows it once it answers;
  /// with `focus`, selects it then, starting it when it is not running.
  fn connect(&mut self, spec: RunnerSpec, focus: bool) {
    if !self.connecting.insert(spec.clone()) {
      return;
    }
    if focus {
      self.toast(format!("Opening {}…", runner_name(&spec)), false);
    }
    self.spawn(async move {
      let opened = async {
        let conn =
          crate::runner::socket::connect_client_socket(&spec, focus).await?;
        Link::open_with(
          Some(spec.clone()),
          socket_connector(spec.clone()),
          conn,
        )
        .await
      }
      .await;
      move |app: &mut App| {
        app.connecting.remove(&spec);
        match opened {
          Ok(link) => {
            let id = app.add_runner(link);
            if focus && let Some(runner) = app.runner(id) {
              let name = runner.name.clone();
              app.state.tasks.select_path(&name);
              app.state.tasks.expand();
            }
          }
          Err(err) => {
            app.failed.insert(spec.clone(), Instant::now());
            if focus {
              app.toast(format!("{}: {err:#}", runner_name(&spec)), true);
            } else {
              log::debug!("not showing {}: {err:#}", spec.root.display());
            }
          }
        }
      }
    });
  }

  /// The runners found running: each one not shown yet is shown.
  pub(super) fn found(&mut self, running: Vec<RunnerSpec>) {
    let now = Instant::now();
    self.failed.retain(|_, at| now.duration_since(*at) < RETRY);
    for spec in running {
      let shown =
        (self.runners.iter()).any(|runner| runner.link.runner() == Some(&spec));
      if !shown && !self.failed.contains_key(&spec) {
        self.connect(spec, false);
      }
    }
  }
}

/// What a runner's config would change, by its ids.
struct RemoteStatus {
  changes: usize,
  edited: Vec<TaskId>,
  removed: usize,
}

/// Asks a runner what a reload would change, and what writing back would
/// write; only reads.
async fn status(link: &Link) -> anyhow::Result<RemoteStatus> {
  let reload = link.request(RpcRequest::Reload {
    apply: false,
    plan: None,
  });
  let written = link.request(RpcRequest::WriteBack {
    include: Vec::new(),
    paths: None,
    prompt: false,
    plan: None,
  });
  let reload: ReloadResult = serde_json::from_value(reload.await?)?;
  // As many as the reload dialog lists.
  let changes = reload.plan.len();
  let written: WriteBackResult = serde_json::from_value(written.await?)?;
  let edited = (written.items.iter())
    .filter(|item| item.kind == "changed")
    .filter_map(|item| item.task)
    .collect();
  let removed = (written.items.iter())
    .filter(|item| item.kind == "removed")
    .count();
  Ok(RemoteStatus {
    changes,
    edited,
    removed,
  })
}

enum Listed {
  Task(Box<RpcListed>),
  Folder(crate::protocol::rpc::RpcFolder),
}

/// The tasks and folders of a `tasks` event in list order: each after the
/// one it is listed after.
fn in_list_order(
  tasks: Vec<RpcListed>,
  folders: Vec<crate::protocol::rpc::RpcFolder>,
) -> Vec<Listed> {
  let mut next: HashMap<Option<TaskId>, Vec<Listed>> = HashMap::new();
  for task in tasks {
    next
      .entry(task.after)
      .or_default()
      .push(Listed::Task(Box::new(task)));
  }
  for folder in folders {
    next
      .entry(folder.after)
      .or_default()
      .push(Listed::Folder(folder));
  }
  let mut ordered = Vec::new();
  let mut after = None;
  while let Some(mut listed) = next.remove(&after) {
    let first = listed.remove(0);
    if !listed.is_empty() {
      next.insert(after, listed);
    }
    after = Some(match &first {
      Listed::Task(task) => task.info.id,
      Listed::Folder(folder) => folder.id,
    });
    ordered.push(first);
  }
  // After what is not listed: last, as they came.
  let mut rest: Vec<(Option<TaskId>, Vec<Listed>)> = next.into_iter().collect();
  rest.sort_by_key(|(after, _)| after.map(|id| id.0));
  ordered.extend(rest.into_iter().flat_map(|(_, listed)| listed));
  ordered
}

/// A task state as the wire has it.
pub fn task_state(state: &RpcState) -> TaskState {
  let info = ExitInfo {
    code: state.exit_code,
    signal: state.signal,
    ready_timeout: state.reason.as_deref() == Some("ready_timeout"),
  };
  match state.state.as_str() {
    "starting" => TaskState::Starting,
    "running" => TaskState::Running,
    "ready" => TaskState::Ready,
    "stopping" => TaskState::Stopping,
    "backoff" => TaskState::Backoff(info),
    "done" => TaskState::Done(info),
    "exited" => TaskState::Exited(info),
    _ => TaskState::Idle,
  }
}

fn task_kind(kind: Option<&str>) -> TaskKind {
  match kind {
    Some("job") => TaskKind::Job,
    _ => TaskKind::Service,
  }
}

/// Tells the console each time the user config changes, as it reads
/// then.
pub(super) fn follow_user_config(tx: UnboundedSender<AppMsg>) {
  tokio::spawn(async move {
    let (changes, mut rx) = tokio::sync::watch::channel(0u64);
    let changes = Arc::new(changes);
    // Watches the files each read of the user config names, and goes
    // with the console.
    let made = tokio::task::spawn_blocking(move || {
      let config = Config::user();
      let watch = crate::config::watch::ConfigWatch::new(changes);
      let mut watch = watch
        .map_err(|err| log::warn!("Can't watch the user config: {err}"))
        .ok()?;
      watch.set(&config.watched);
      Some(watch)
    })
    .await;
    let Ok(Some(mut watch)) = made else { return };
    loop {
      tokio::select! {
        changed = rx.changed() => if changed.is_err() { break },
        () = tx.closed() => break,
      }
      tokio::time::sleep(SETTLE).await;
      let read = tokio::task::spawn_blocking(move || {
        let config = Config::user();
        watch.set(&config.watched);
        (watch, config)
      })
      .await;
      let Ok((kept, config)) = read else { break };
      watch = kept;
      if tx.send(AppMsg::UserConfig(Arc::new(config))).is_err() {
        break;
      }
    }
  });
}

/// Looks for the runners on this machine every `TICK`, for as long as
/// the console runs.
pub(super) fn discover(tx: UnboundedSender<AppMsg>) {
  tokio::spawn(async move {
    loop {
      let running = tokio::task::spawn_blocking(|| {
        lockfile::list_runners()
          .unwrap_or_default()
          .into_iter()
          .filter(|info| info.is_running)
          .map(|info| info.contents.spec())
          .collect::<Vec<_>>()
      })
      .await
      .unwrap_or_default();
      let msg = super::edit::ConsoleMsg::new(move |app| app.found(running));
      if tx.send(AppMsg::Post(msg)).is_err() {
        break;
      }
      tokio::time::sleep(TICK).await;
    }
  });
}

#[cfg(test)]
mod tests {
  use super::*;

  /// The console follows the runner's list from its events: a task
  /// added in the middle, a change that moves nothing, a move, and a
  /// removal, as drawn.
  #[tokio::test]
  async fn the_list_follows_the_runners_events() {
    use crate::{
      command::{Command, execute},
      console::app::{Setup, Started, start},
      kernel::{
        kernel::Kernel,
        kernel_message::{KernelCommand, Place},
      },
      term::Geometry,
    };

    let kernel = Kernel::new();
    let pc = kernel.context();
    let kernel = tokio::spawn(kernel.run());
    let config = Arc::new(Config::make_default());
    let ctx =
      crate::dekit::server::ServerCtx::local(pc.clone(), config.clone());
    let link =
      Link::open(None, crate::mprocs::mprocs::local_connector(ctx)).await;
    let Started { screen, .. } = start(Setup {
      keymap: config.keymap.build(),
      config: config.clone(),
      geometry: Geometry::cells(80, 16),
      caps: Default::default(),
      links: vec![link.unwrap()],
      all_runners: false,
      follow_user_config: false,
    });
    // The names down the list, as drawn.
    let listed = || {
      let frame = screen.load();
      (0..frame.size.height)
        .filter_map(|row| {
          let text = frame.row_text(row, 0, 30);
          ["alpha", "bravo", "xray", "renamed"]
            .into_iter()
            .find(|name| text.contains(name))
        })
        .collect::<Vec<_>>()
    };
    let shows = async |want: &[&str]| {
      for _ in 0..200 {
        if listed() == want {
          return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
      }
      panic!("the list is {:?}, not {want:?}", listed());
    };
    let run = async |command: Command| {
      execute(&pc, &config, &command).await.unwrap();
    };
    let add = |path: &str, place: Place| Command::Add {
      target: Target::glob(path),
      spec: serde_yaml::from_str("{cmd: [sh, -c, 'true']}").unwrap(),
      place,
      start: false,
      defaults: true,
      origin: None,
    };

    run(add("alpha", Place::Keep)).await;
    run(add("bravo", Place::Keep)).await;
    shows(&["alpha", "bravo"]).await;
    run(add("xray", Place::After(TaskPath::new("alpha").unwrap()))).await;
    shows(&["alpha", "xray", "bravo"]).await;
    // A change, as a rename is, leaves the task where it is.
    run(Command::Rename {
      target: Target::glob("bravo"),
      name: "renamed".to_string(),
    })
    .await;
    shows(&["alpha", "xray", "renamed"]).await;
    run(Command::Move {
      target: Target::glob("bravo"),
      to: None,
      place: Place::First,
    })
    .await;
    shows(&["renamed", "alpha", "xray"]).await;
    run(Command::Remove {
      target: Target::glob("xray"),
    })
    .await;
    shows(&["renamed", "alpha"]).await;

    pc.send(KernelCommand::Quit);
    tokio::time::timeout(Duration::from_secs(5), kernel)
      .await
      .unwrap()
      .unwrap();
  }

  /// The order follows inserts, moves, and removals, so a task whose
  /// neighbour changed is not taken for moved.
  #[test]
  fn order_follows_its_changes() {
    let [a, b, x] = [TaskId(1), TaskId(2), TaskId(3)];
    let mut order = Order::default();
    order.place(a, None);
    order.place(b, Some(a));
    order.place(x, Some(a));
    assert_eq!(order.after(b), Some(Some(x)));
    order.unplace(x);
    assert_eq!(order.after(b), Some(Some(a)));
    // A move to the front: what was first follows it.
    order.place(b, None);
    assert_eq!(order.after(a), Some(Some(b)));
    assert_eq!(order.after(b), Some(None));
  }
  use crate::protocol::rpc::RpcFolder;

  fn listed(id: usize, after: Option<usize>) -> RpcListed {
    RpcListed {
      info: RpcTaskInfo {
        id: TaskId(id),
        path: format!("t{id}"),
        label: None,
        kind: None,
        state: RpcState {
          state: "idle".to_string(),
          exit_code: None,
          signal: None,
          reason: None,
        },
        spec: None,
        origin: None,
      },
      after: after.map(TaskId),
    }
  }

  /// Tasks and folders come apart in a `tasks` event; each goes after
  /// the one it is listed after, whatever the order they came in.
  #[test]
  fn a_tasks_event_is_put_in_list_order() {
    let tasks = vec![listed(3, Some(2)), listed(1, None), listed(9, Some(7))];
    let folders = vec![RpcFolder {
      id: TaskId(2),
      path: "f".to_string(),
      after: Some(TaskId(1)),
      collapsed: false,
    }];
    let order: Vec<usize> = in_list_order(tasks, folders)
      .iter()
      .map(|listed| match listed {
        Listed::Task(task) => task.info.id.0,
        Listed::Folder(folder) => folder.id.0,
      })
      .collect();
    // 9 is after what is not listed: last.
    assert_eq!(order, [1, 2, 3, 9]);
  }

  #[test]
  fn a_wire_state_reads_back() {
    let state = |token: &str, code| RpcState {
      state: token.to_string(),
      exit_code: code,
      signal: None,
      reason: None,
    };
    assert_eq!(task_state(&state("ready", None)), TaskState::Ready);
    assert_eq!(
      task_state(&state("exited", Some(1))),
      TaskState::Exited(ExitInfo::code(1))
    );
    assert_eq!(task_state(&state("someday", None)), TaskState::Idle);
  }
}
