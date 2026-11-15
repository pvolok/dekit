//! The console's part in changing tasks: the task dialog, the task
//! menu, moving tasks by key and by drag, and the reload and write-back
//! dialogs. Whatever has to wait on a runner runs in a spawned future,
//! which posts back what to do on the console once it is done.

use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::command::Command;
use crate::console::{
  action::Action,
  modal::{
    checklist::Checklist,
    config_error::ConfigErrorModal,
    menu::{MenuItem, MenuModal},
    modal::{IssueError, Modal, ModalResult},
    reload::ReloadModal,
    remove_task::RemoveTaskModal,
    task_dialog::{DialogContext, TaskDialog, TaskDialogMode},
    write_back::{How, WriteBack, WriteBackModal},
  },
  state::{Drag, Drop, Scope, Toast},
  task_tree::{Group, Node},
};
use crate::kernel::{
  kernel_message::Place,
  task::TaskId,
  task_path::{TaskPath, is_at_or_under},
};
use crate::protocol::{RpcRequest, WaitFor, rpc::WriteBackResult};
use crate::reload::{Plan, ReloadResult};
use crate::spec::TaskSpec;
use crate::target::Target;
use crate::term::{
  grid::Rect,
  mouse::{MouseButton, MouseEvent, MouseEventKind},
};

use super::{App, AppMsg, RunnerId};
use crate::console::ui_tasks::{INDENT, PAD};

/// How long a toast stays.
const TOAST: Duration = Duration::from_secs(4);

/// What one of the console's own futures does on the console once it is
/// done, to the console as it is by then.
pub struct ConsoleMsg(Box<dyn FnOnce(&mut App) + Send>);

impl ConsoleMsg {
  pub(super) fn new(f: impl FnOnce(&mut App) + Send + 'static) -> Self {
    ConsoleMsg(Box::new(f))
  }
}

/// Posts to the console from its futures.
#[derive(Clone)]
pub(super) struct Poster(tokio::sync::mpsc::UnboundedSender<AppMsg>);

impl Poster {
  pub(super) fn post(&self, f: impl FnOnce(&mut App) + Send + 'static) {
    let _ = self.0.send(AppMsg::Post(ConsoleMsg::new(f)));
  }
}

#[derive(Clone, Copy)]
pub(super) enum Purpose {
  Edit,
  Duplicate,
  Copy,
}

impl App {
  pub(super) fn poster(&self) -> Poster {
    Poster(self.tx.clone())
  }

  /// Runs `f` off the console, then on the console what it returns.
  pub(super) fn spawn<F, R>(&self, f: F)
  where
    F: Future<Output = R> + Send + 'static,
    R: FnOnce(&mut App) + Send + 'static,
  {
    let post = self.poster();
    tokio::spawn(async move { post.post(f.await) });
  }

  /// Runs `f` off the console, then shows the modal it makes, or why the
  /// config does not load, unless another was opened meanwhile.
  pub(super) fn spawn_modal<F>(&self, f: F)
  where
    F: Future<Output = anyhow::Result<Option<Box<dyn Modal>>>> + Send + 'static,
  {
    let seq = self.modal_seq;
    let root = self.focused_root();
    self.spawn(async move {
      let modal = f.await;
      move |app: &mut App| {
        let modal = match modal {
          Ok(Some(modal)) => modal,
          Ok(None) => return,
          Err(err) => Box::new(ConfigErrorModal::new(&err, &root)),
        };
        app.open_if_current(seq, modal);
      }
    });
  }

  /// Shows a modal in place of any other.
  pub(super) fn open(&mut self, modal: Box<dyn Modal>) {
    self.modal = Some(modal);
    self.modal_seq += 1;
    self.modal_runner = None;
  }

  /// `open`, for a modal whose commands go to `runner`.
  fn open_for(&mut self, runner: RunnerId, modal: Box<dyn Modal>) {
    self.open(modal);
    self.modal_runner = Some(runner);
  }

  /// Shows a modal asked for while `seq` was the last one opened: one
  /// opened since is what the user moved on to.
  pub(super) fn open_if_current(&mut self, seq: u64, modal: Box<dyn Modal>) {
    if self.modal_seq == seq {
      self.open(modal);
    }
  }

  pub(super) fn toast(&mut self, text: impl Into<String>, error: bool) {
    self.state.toast = Some(Toast {
      text: text.into(),
      error,
      until: Instant::now() + TOAST,
    });
    self.dirty = true;
  }

  pub(super) fn handle_console_msg(&mut self, ConsoleMsg(f): ConsoleMsg) {
    f(self);
  }

  /// The directory relative paths typed for the focused runner are from.
  fn focused_root(&self) -> std::path::PathBuf {
    let runner = self.focused().map(|runner| runner.id);
    runner.map_or_else(std::env::temp_dir, |runner| self.root(runner))
  }

  /// A runner's project directory, or where `dekit mprocs` runs.
  fn root(&self, runner: RunnerId) -> std::path::PathBuf {
    match self.runner(runner).and_then(|r| r.link.runner()) {
      Some(spec) => spec.root.clone(),
      None => std::env::current_dir()
        .unwrap_or_else(|_| std::path::Path::new("/").into()),
    }
  }

  /// How the commands modal `issuer` issued went: to it while it is open,
  /// as a toast otherwise. A task it added is listed by now, and selected.
  fn issued(
    &mut self,
    issuer: u64,
    result: Result<(), IssueError>,
    added: Vec<String>,
  ) {
    for path in added {
      self.state.tasks.select_path(&path);
    }
    match &mut self.modal {
      Some(modal) if issuer == self.modal_seq => {
        let result = modal.issued(result);
        self.modal_result(result);
      }
      Some(_) | None => {
        if let Err(err) = result {
          self.toast(err.message, true);
        }
      }
    }
  }

  /// Carries a modal's result out.
  pub(super) fn modal_result(&mut self, result: ModalResult) {
    let runner = self
      .modal_runner
      .or_else(|| self.focused().map(|runner| runner.id));
    match result {
      ModalResult::Keep => (),
      ModalResult::Close => self.modal = None,
      ModalResult::Run(action) => {
        self.modal = None;
        self.handle_action(action);
      }
      ModalResult::Issue(commands) => {
        if let Some(runner) = runner {
          self.issue_for_modal(runner, commands);
        }
      }
      ModalResult::ApplyReload(plan) => {
        self.modal = None;
        if let Some(runner) = runner {
          self.apply_reload(runner, *plan);
        }
      }
      ModalResult::WriteBack {
        include,
        chosen,
        how,
      } => {
        self.modal = None;
        if let Some(runner) = runner {
          self.write_back(runner, include, chosen, how);
        }
      }
    }
  }

  /// Runs the commands one after another on a runner, then tells the
  /// modal.
  fn issue_for_modal(&mut self, runner: RunnerId, commands: Vec<Command>) {
    let Some(link) = self.runner(runner).map(|r| r.link.clone()) else {
      return;
    };
    let name = self
      .runner(runner)
      .map(|r| r.name.clone())
      .unwrap_or_default();
    let issuer = self.modal_seq;
    self.spawn(async move {
      let mut added = Vec::new();
      let mut result = Ok(());
      for (done, command) in commands.iter().enumerate() {
        let request = RpcRequest::Command(command.clone());
        if let Err(err) = link.request(request).await {
          let message = err.to_string();
          result = Err(IssueError { done, message });
          break;
        }
        if let Command::Add { target, .. } = command
          && let Ok(key) = target.key()
          && let Some(path) = super::list_path(&name, key.path.as_str())
        {
          added.push(path.to_string());
        }
      }
      move |app: &mut App| app.issued(issuer, result, added)
    });
  }

  /// Issues a command on the focused runner now; a failure shows as a
  /// toast.
  pub(super) fn issue(&self, command: Command) {
    if let Some(runner) = self.focused().map(|runner| runner.id) {
      self.issue_on(runner, command);
    }
  }

  pub(super) fn issue_on(&self, runner: RunnerId, command: Command) {
    self.issue_then(runner, command, |app, result| {
      if let Err(err) = result {
        app.toast(err, true);
      }
    });
  }

  /// Issues a command, then does `then` on the console with how it went.
  pub(super) fn issue_then(
    &self,
    runner: RunnerId,
    command: Command,
    then: impl FnOnce(&mut App, Result<(), String>) + Send + 'static,
  ) {
    let Some(link) = self.runner(runner).map(|r| r.link.clone()) else {
      return;
    };
    self.spawn(async move {
      let result = link
        .request(RpcRequest::Command(command))
        .await
        .map(drop)
        .map_err(|err| err.to_string());
      move |app: &mut App| then(app, result)
    });
  }

  fn dialog_context(&self, runner: RunnerId) -> DialogContext {
    DialogContext {
      root: self.root(runner),
      paths: self.remote_paths(runner),
      system_shell: self.config.system_shell,
    }
  }

  pub(super) fn show_add_task(&mut self) {
    let Some(runner) = self.focused().map(|runner| runner.id) else {
      return;
    };
    let (group, after) = match self.state.tasks.selected() {
      Some((path, Node::Group(_))) => {
        match self.runner_at(path.as_str()).and_then(|(_, rest)| rest) {
          Some(rest) => (format!("{rest}/"), None),
          None => (String::new(), None),
        }
      }
      Some((_, Node::Task(task))) => {
        let path = self.remote_path(task).and_then(|p| TaskPath::new(p).ok());
        let group = path
          .as_ref()
          .and_then(TaskPath::parent)
          .filter(|parent| !parent.is_root())
          .map(|parent| format!("{parent}/"))
          .unwrap_or_default();
        (group, path)
      }
      None => (String::new(), None),
    };
    // The project's defaults are the runner's: it adds them.
    let dialog = TaskDialog::new(
      TaskDialogMode::Add { after, copy: false },
      TaskSpec::default(),
      self.dialog_context(runner),
      group,
    );
    self.open_for(runner, Box::new(dialog));
  }

  pub(super) fn show_edit_task(&mut self, purpose: Purpose) {
    let Some(task) = self.state.current_task() else {
      return self.toast("Select a task first.", false);
    };
    let Some(spec) = task.spec.clone() else {
      return self.toast("This task can't be edited.", true);
    };
    let (runner, active) = (task.runner, task.status.is_active());
    let origin = task.origin.clone();
    let label = task.label.clone();
    let path = self.remote_path(task).unwrap_or_default();
    match purpose {
      Purpose::Copy => match spec.cmd_line() {
        Some(line) => {
          self.yank(line);
          self.toast("Copied the command.", false);
        }
        None => self.toast("This task runs no command.", true),
      },
      Purpose::Edit => {
        let mode = TaskDialogMode::Edit {
          path: path.clone(),
          running: active,
          origin,
        };
        let dialog =
          TaskDialog::new(mode, spec, self.dialog_context(runner), path);
        self.open_for(runner, Box::new(dialog));
      }
      Purpose::Duplicate => {
        let copy_path = self.unique_path(runner, &path);
        let root = self.root(runner);
        let mut spec = spec.for_copy(&path, &root);
        if let Some(label) = spec.label().map(String::from).or(label) {
          spec.set_label(Some(&self.unique_label(runner, &label, None)));
        }
        let dialog = TaskDialog::new(
          TaskDialogMode::Add {
            after: TaskPath::new(path).ok(),
            copy: true,
          },
          spec,
          self.dialog_context(runner),
          copy_path,
        );
        self.open_for(runner, Box::new(dialog));
      }
    }
  }

  pub(super) fn show_remove(&mut self) {
    let (runner, modal) = match self.state.tasks.selected() {
      Some((_, Node::Task(task))) => (
        task.runner,
        RemoveTaskModal {
          target: Target::Id(task.remote),
          what: format!("task {}", task.name()),
          running: task.status.is_active(),
        },
      ),
      Some((path, Node::Group(group))) => {
        let Some((runner, Some(rest))) = self.runner_at(path.as_str()) else {
          return self.toast("A runner's tasks are removed one by one.", false);
        };
        (
          runner,
          RemoveTaskModal {
            target: Target::glob(&rest),
            what: match group.len() {
              0 => format!("{} {rest}/", group_noun(group)),
              n => format!("{} {rest}/ and its {n} tasks", group_noun(group)),
            },
            running: self.state.tasks.any_active(path),
          },
        )
      }
      None => return,
    };
    self.open_for(runner, Box::new(modal));
  }

  /// The menu for the selected row, or with `none` for the list itself.
  pub(super) fn show_task_menu(&mut self, anchor: Option<Rect>, none: bool) {
    let keymap = &self.keymap;
    let item =
      |label: &str, action: Action| MenuItem::new(label, action, keymap);
    let selected_area = if none {
      None
    } else {
      self.state.tasks.selected_area()
    };
    let selected = if none {
      None
    } else {
      self.state.tasks.selected()
    };
    let (title, items) = match selected {
      Some((path, Node::Task(task))) => {
        let active = task.status.is_active();
        let own = self.state.config.own.contains(&task.id);
        let edited = self.state.config.edited.contains(&task.id);
        let mut items = vec![
          if active {
            item("Stop", Action::StopTask)
          } else {
            item("Start", Action::StartTask)
          },
          item("Restart", Action::RestartTask),
          item("Force restart", Action::ForceRestartTask),
          item("Kill", Action::KillTask).enabled(active),
          item("Edit…", Action::ShowEditTask).separated(),
          item("Duplicate…", Action::ShowDuplicateTask),
          item("Copy command", Action::CopyTaskCommand),
          item("Move up", Action::MoveTaskUp),
          item("Move down", Action::MoveTaskDown),
          item("Zoom", Action::Zoom).separated(),
        ];
        if own {
          items.push(item("Save to the config…", Action::SaveToConfig));
        } else if edited {
          items.push(item("Write back to the config…", Action::ShowWriteBack));
        }
        items.push(item("Remove…", Action::ShowRemoveTask).separated());
        (path.to_string(), items)
      }
      Some((path, Node::Group(group))) => {
        let runner = self.runner_at(path.as_str());
        let of_runner = runner.as_ref().is_some_and(|(_, rest)| rest.is_none());
        let mut items = vec![
          item("Start all", Action::StartTask),
          item("Stop all", Action::StopTask)
            .enabled(self.state.tasks.any_active(path)),
          item("Restart all", Action::RestartTask),
          item("Add a task here…", Action::ShowAddTask).separated(),
          if group.collapsed {
            item("Expand", Action::Expand)
          } else {
            item("Collapse", Action::Collapse)
          },
        ];
        if of_runner {
          if self.has_project() {
            items.push(item("Reload the config…", Action::ShowReload));
          }
        } else {
          items.extend([
            item("Move up", Action::MoveTaskUp),
            item("Move down", Action::MoveTaskDown),
            item(
              &format!("Remove {}…", group_noun(group)),
              Action::ShowRemoveTask,
            )
            .separated(),
          ]);
        }
        (format!("{path}/"), items)
      }
      None => {
        let mut items = vec![item("Add a task…", Action::ShowAddTask)];
        if self.has_project() {
          items.push(item("Reload the config…", Action::ShowReload));
        }
        ("tasks".to_string(), items)
      }
    };
    let anchor = anchor
      .or(selected_area.map(|area| Rect {
        x: area.x + PAD + 2,
        ..area
      }))
      .unwrap_or_else(|| self.state.tasks.area());
    self.open(Box::new(
      MenuModal::new(anchor, items)
        .title(title)
        .keep(selected_area),
    ));
  }

  /// Swaps the selected task or group with the one beside it.
  pub(super) fn move_selected(&mut self, down: bool) {
    let Some((path, _)) = self.state.tasks.selected() else {
      return;
    };
    let path = path.clone();
    let Some((runner, Some(from))) = self.runner_at(path.as_str()) else {
      return;
    };
    let siblings = self.state.tasks.siblings(&path);
    let Some(at) = siblings.iter().position(|s| *s == path) else {
      return;
    };
    let beside = |at: usize| -> Option<TaskPath> {
      let (_, rest) = self.runner_at(siblings[at].as_str())?;
      TaskPath::new(rest?).ok()
    };
    let place = match (down, at) {
      (true, at) if at + 1 < siblings.len() => match beside(at + 1) {
        Some(after) => Place::After(after),
        None => return,
      },
      (false, 0) | (true, _) => return,
      (false, 1) => Place::First,
      (false, at) => match beside(at - 2) {
        Some(after) => Place::After(after),
        None => return,
      },
    };
    self.issue_on(
      runner,
      Command::Move {
        target: Target::glob(&from),
        to: None,
        place,
      },
    );
  }

  /// The reload dialog for the focused runner; with nothing in it to
  /// pick, the reload at once, for what the config has besides its
  /// tasks.
  pub(super) fn show_reload(&mut self) {
    let Some(runner) = self.focused() else { return };
    if runner.link.runner().is_none() {
      return;
    }
    let (id, link) = (runner.id, runner.link.clone());
    let post = self.poster();
    let seq = self.modal_seq;
    self.spawn_modal(async move {
      let dry = RpcRequest::Reload {
        apply: false,
        plan: None,
      };
      let result: ReloadResult =
        serde_json::from_value(link.request(dry).await?)?;
      let plan = result.plan;
      if plan.len() > 0 {
        post.post(move |app| {
          if app.modal_seq == seq {
            app.open_for(id, Box::new(ReloadModal::new(plan)));
          }
        });
        return Ok(None);
      }
      let apply = RpcRequest::Reload {
        apply: true,
        plan: None,
      };
      link.request(apply).await?;
      post.post(|app| app.toast(Plan::NOTHING, false));
      Ok(None)
    });
  }

  fn apply_reload(&mut self, runner: RunnerId, reloading: Plan) {
    let Some(link) = self.runner(runner).map(|r| r.link.clone()) else {
      return;
    };
    // Never the config's defaults in place of what the user chose.
    let plan = match serde_json::to_value(&reloading) {
      Ok(plan) => plan,
      Err(err) => {
        return self.toast(format!("Can't send the choices: {err}"), true);
      }
    };
    self.spawn(async move {
      let request = RpcRequest::Reload {
        apply: true,
        plan: Some(plan),
      };
      let result = async {
        let result: ReloadResult =
          serde_json::from_value(link.request(request).await?)?;
        anyhow::Ok(result.errors)
      }
      .await;
      move |app: &mut App| match result {
        Ok(errors) => match errors.first() {
          None => app.toast("Reloaded the config.", false),
          Some(first) => app.toast(
            format!("Reloaded, but {} failed: {first}", errors.len()),
            true,
          ),
        },
        Err(err) => app.toast(format!("{err:#}"), true),
      }
    });
  }

  /// The write-back dialog for the focused runner; `include` adds the
  /// runner's own tasks.
  pub(super) fn show_write_back(&mut self, include: Vec<TaskId>) {
    let runner = match include.first() {
      Some(id) => self.state.tasks.task(*id).map(|task| task.runner),
      None => self.focused().map(|runner| runner.id),
    };
    let Some(runner) = runner.and_then(|id| self.runner(id)) else {
      return;
    };
    if runner.link.runner().is_none() {
      return;
    }
    let (id, link) = (runner.id, runner.link.clone());
    let include: Vec<TaskId> = include
      .iter()
      .filter_map(|local| runner.remote_of(*local))
      .collect();
    let agent = self.agent_name();
    let post = self.poster();
    let seq = self.modal_seq;
    self.spawn_modal(async move {
      let request = RpcRequest::WriteBack {
        include: include.clone(),
        paths: None,
        prompt: false,
        plan: None,
      };
      let result: WriteBackResult =
        serde_json::from_value(link.request(request).await?)?;
      let write_back = WriteBack {
        items: result.items,
        include,
        agent,
      };
      post.post(move |app| {
        if app.modal_seq == seq {
          app.open_for(id, Box::new(WriteBackModal::new(write_back)));
        }
      });
      Ok(None)
    });
  }

  /// The agent's program: the one the user config names, or `claude`
  /// when it is installed.
  fn agent_cmd(&self) -> Option<Vec<String>> {
    // Looked for once: a search of PATH on the console's loop.
    static CLAUDE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    match &self.config.agent {
      Some(cmd) => Some(cmd.clone()),
      None => (*CLAUDE.get_or_init(|| which::which("claude").is_ok()))
        .then(|| vec!["claude".to_string()]),
    }
  }

  fn agent_name(&self) -> Option<String> {
    self.agent_cmd().and_then(|cmd| cmd.first().cloned())
  }

  fn write_back(
    &mut self,
    runner: RunnerId,
    include: Vec<TaskId>,
    chosen: Vec<String>,
    how: How,
  ) {
    let Some(link) = self.runner(runner).map(|r| r.link.clone()) else {
      return;
    };
    let request = |prompt| RpcRequest::WriteBack {
      include: include.clone(),
      paths: Some(chosen.clone()),
      prompt,
      plan: None,
    };
    match how {
      How::Write => {
        let request = request(false);
        self.spawn(async move {
          let result = async {
            let result: WriteBackResult =
              serde_json::from_value(link.request(request).await?)?;
            anyhow::Ok(result.failed)
          }
          .await;
          move |app: &mut App| match result.as_deref() {
            Ok([]) => app.toast("Written to the config.", false),
            Ok(failed @ [first, ..]) => app.toast(
              format!(
                "{} not written ({}: {}); an agent can do it",
                failed.len(),
                first.path,
                first.why
              ),
              true,
            ),
            Err(err) => app.toast(format!("{err:#}"), true),
          }
        });
      }
      How::Copy => {
        let request = request(true);
        self.spawn(async move {
          let prompt = async {
            let result: WriteBackResult =
              serde_json::from_value(link.request(request).await?)?;
            anyhow::Ok(result.prompt.unwrap_or_default())
          }
          .await;
          move |app: &mut App| match prompt {
            Ok(prompt) => {
              app.yank(prompt);
              app.toast("Copied a prompt for an agent.", false);
            }
            Err(err) => app.toast(format!("{err:#}"), true),
          }
        });
      }
      How::Agent => {
        let Some(argv) = self.agent_cmd() else {
          return self.toast(
            "No agent: set `agent: {cmd: [...]}` in the user config.",
            true,
          );
        };
        let root = self.root(runner);
        let path = self.unique_path(runner, "agent");
        let local = self
          .local_path(runner, &path)
          .map(|path| path.to_string())
          .unwrap_or_default();
        self.toast("The agent is on it; dekit checks its edits.", false);
        let request = request(true);
        let post = self.poster();
        self.spawn(async move {
          let run = async {
            let result: WriteBackResult =
              serde_json::from_value(link.request(request).await?)?;
            let (Some(prompt), Some(plan)) = (result.prompt, result.plan)
            else {
              anyhow::bail!("the runner wrote no prompt");
            };
            let mut argv = argv;
            argv.push(prompt);
            let mut spec = TaskSpec::with_cmd(argv);
            spec.set_label(Some("agent: write back"));
            spec.set("type", Some("job".into()));
            spec.set("cwd", Some(root.to_string_lossy().into_owned().into()));
            let add = Command::Add {
              target: Target::glob(&path),
              spec,
              place: Default::default(),
              start: true,
              defaults: true,
              origin: None,
            };
            link.request(RpcRequest::Command(add)).await?;
            // Listed by now: its terminal takes the focus.
            post.post(move |app| {
              app.state.tasks.select_path(&local);
              app.state.scope = Scope::Term;
            });
            // Its edits are checked once it ends, or is removed.
            wait_exit(&link, Target::glob(&path)).await?;
            let check = RpcRequest::WriteBack {
              include: Vec::new(),
              paths: None,
              prompt: false,
              plan: Some(plan),
            };
            let result: WriteBackResult =
              serde_json::from_value(link.request(check).await?)?;
            anyhow::Ok(result.failed)
          };
          let failed = run.await;
          move |app: &mut App| match failed.as_deref() {
            Err(err) => app.toast(format!("{err:#}"), true),
            Ok([]) => app.toast("The agent's edits are in.", false),
            Ok(failed @ [first, ..]) => app.toast(
              format!(
                "The agent left {} not written ({}: {})",
                failed.len(),
                first.path,
                first.why
              ),
              true,
            ),
          }
        });
      }
    }
  }

  /// Where a press, drag, and release on the task list go: a click picks
  /// the row, a drag moves what it started on.
  pub(super) fn sidebar_mouse(&mut self, mouse: &MouseEvent) {
    let (x, y) = (mouse.x as u16, mouse.y as u16);
    match mouse.kind {
      MouseEventKind::Down(MouseButton::Left) => {
        let row = self.state.tasks.row_at(x, y);
        self.state.drag = row.and_then(|row| {
          let (path, _) = self.state.tasks.row(row.index)?;
          Some(Drag {
            from: Arc::new(path.clone()),
            active: false,
            drop: None,
          })
        });
      }
      MouseEventKind::Drag(MouseButton::Left) => {
        let Some((from, was_active)) =
          self.state.drag.as_ref().map(|d| (d.from.clone(), d.active))
        else {
          return;
        };
        // Gone from the list meanwhile: nothing is dragged.
        let Some(from_row) = self.state.tasks.row_of(&from) else {
          self.state.drag = None;
          return;
        };
        let area = self.state.tasks.area();
        // At the top or bottom row the list scrolls under the pointer.
        if y <= area.y {
          self.state.tasks.scroll_by(-1);
        } else if y + 1 >= area.bottom() {
          self.state.tasks.scroll_by(1);
        }
        let row = self.row_near(y);
        let active = was_active || row != Some(from_row);
        // Off the list, letting go drops nothing.
        let drop = if active && (area.x..area.right()).contains(&x) {
          self.drop_at(x, row, &from)
        } else {
          None
        };
        if let Some(drag) = &mut self.state.drag {
          drag.active = active;
          drag.drop = drop;
        }
      }
      MouseEventKind::Up(MouseButton::Left) => {
        let Some(drag) = self.state.drag.take() else {
          return;
        };
        if !drag.active {
          // The row it is on now: the list can have changed since the press.
          if let Some(row) = self.state.tasks.row_of(&drag.from) {
            self.state.tasks.click(row);
          }
          return;
        }
        if let Some(drop) = drag.drop {
          self.drop_on(&drag.from, drop);
        }
      }
      MouseEventKind::Down(MouseButton::Right) => {
        let row = self.state.tasks.row_at(x, y);
        if let Some(row) = row {
          self.state.tasks.select_row(row.index);
        }
        self.show_task_menu(Some(Rect::new(x, y, 1, 1)), row.is_none());
      }
      MouseEventKind::ScrollDown => self.state.tasks.scroll_by(3),
      MouseEventKind::ScrollUp => self.state.tasks.scroll_by(-3),
      MouseEventKind::Down(MouseButton::Middle)
      | MouseEventKind::Up(_)
      | MouseEventKind::Drag(_)
      | MouseEventKind::Moved
      | MouseEventKind::ScrollLeft
      | MouseEventKind::ScrollRight => (),
    }
  }

  /// The row at a height, the first or last one past the ends.
  fn row_near(&mut self, y: u16) -> Option<usize> {
    let area = self.state.tasks.area();
    let count = self.state.tasks.row_count();
    if count == 0 || area.height == 0 {
      return None;
    }
    let x = area.x;
    match self
      .state
      .tasks
      .row_at(x, y.clamp(area.y, area.bottom() - 1))
    {
      Some(row) => Some(row.index),
      None => Some(count - 1),
    }
  }

  /// Where dropping at row `row` puts `from`, by where it is listed now:
  /// after the row when dragging down, before it when dragging up. An
  /// open group takes it first; further left goes out of groups. Never
  /// onto another runner, nor a runner's group anywhere.
  fn drop_at(
    &self,
    x: u16,
    row: Option<usize>,
    from: &TaskPath,
  ) -> Option<Drop> {
    let row = row?;
    let from_row = self.state.tasks.row_of(from)?;
    let after_row = match row.cmp(&from_row) {
      std::cmp::Ordering::Greater => Some(row),
      std::cmp::Ordering::Less => row.checked_sub(1),
      std::cmp::Ordering::Equal => return None,
    };
    // The shallowest a drop goes: into its runner's group.
    let floor = usize::from(self.all_runners);
    let drop = match after_row {
      None => Drop {
        parent: TaskPath::root(),
        after: None,
        row: None,
      },
      Some(after_row) => {
        let (path, node) = self.state.tasks.row(after_row)?;
        match node {
          Node::Group(group) if !group.collapsed => Drop {
            parent: path.clone(),
            after: None,
            row: Some(after_row),
          },
          Node::Group(_) | Node::Task(_) => {
            let left = self.state.tasks.area().x + PAD;
            let wanted = (x.saturating_sub(left) / INDENT) as usize;
            let depth = wanted.max(floor).min(path.depth().saturating_sub(1));
            Drop {
              parent: path.ancestor(depth),
              after: Some(path.ancestor(depth + 1)),
              row: Some(after_row),
            }
          }
        }
      }
    };
    // Not into itself, and not after itself.
    let inside = |p: &TaskPath| is_at_or_under(p.as_str(), from.as_str());
    if inside(&drop.parent) || drop.after.as_ref().is_some_and(inside) {
      return None;
    }
    // In the runner it is on, under its group.
    let (from_runner, from_rest) = self.runner_at(from.as_str())?;
    from_rest.as_ref()?;
    let (to_runner, _) = self.runner_at(drop.parent.as_str())?;
    if from_runner != to_runner || drop.parent.depth() < floor {
      return None;
    }
    Some(drop)
  }

  fn drop_on(&mut self, from: &TaskPath, drop: Drop) {
    let Some((runner, Some(remote_from))) = self.runner_at(from.as_str())
    else {
      return;
    };
    let remote = |path: &TaskPath| -> Option<TaskPath> {
      let (_, rest) = self.runner_at(path.as_str())?;
      TaskPath::new(rest?).ok()
    };
    let parent = remote(&drop.parent).unwrap_or_else(TaskPath::root);
    let from_path = match TaskPath::new(remote_from) {
      Ok(path) => path,
      Err(_) => return,
    };
    let to = from_path.with_parent(&parent);
    let place = match drop.after.as_ref().and_then(remote) {
      Some(after) => Place::After(after),
      None => Place::First,
    };
    self.issue_on(
      runner,
      Command::Move {
        target: Target::glob(from_path.as_str()),
        to: (to != from_path).then(|| Target::glob(to.as_str())),
        place,
      },
    );
  }
}

/// Waits on a connection of its own until the task `target` names has
/// ended, or is gone.
async fn wait_exit(link: &super::Link, target: Target) -> anyhow::Result<()> {
  use crate::protocol::{CtlMsg, Request};
  let (mut sender, mut receiver, _) = link.session(Vec::new()).await?;
  let (method, params) = RpcRequest::Wait {
    target: Some(target),
    command: None,
    wait_for: Some(WaitFor::Exit),
    timeout_ms: None,
  }
  .to_wire();
  sender
    .send_ctl(CtlMsg::Request(Request {
      id: 1,
      method,
      params,
    }))
    .await?;
  loop {
    match receiver.recv_ctl().await {
      Ok(CtlMsg::Response(response)) => {
        if let Some(error) = response.error {
          return Err(error.into());
        }
      }
      Ok(CtlMsg::Event(event))
        if event.name == crate::protocol::rpc::EVENT_WAIT_DONE =>
      {
        return Ok(());
      }
      Ok(_) => (),
      // The wait closes after its verdict, or with the runner.
      Err(_) => return Ok(()),
    }
  }
}

/// What a group row is called: a folder, or a group made by the paths
/// under it.
fn group_noun(group: &Group) -> &'static str {
  match group.folder {
    Some(_) => "folder",
    None => "group",
  }
}
