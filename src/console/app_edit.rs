//! The console's part in changing tasks: the task dialog, the task
//! menu, moving tasks by key and by drag, and the reload and write-back
//! dialogs. Whatever has to wait on the kernel runs in a spawned future,
//! which posts back what to do on the console once it is done.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::command::{Command, execute};
use crate::config::config::Config;
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
    write_back::{WriteBack, WriteBackModal},
  },
  state::{ConfigStatus, Drag, Drop, Scope, Toast},
  task_tree::{Group, Node},
};
use crate::kernel::{
  kernel_message::{Place, TaskContext, TaskDescription, TaskSelector},
  task::TaskId,
  task_path::{TaskPath, is_at_or_under},
  task_screen::ObserverId,
};
use crate::reload;
use crate::spec::TaskSpec;
use crate::target::Target;
use crate::task::config_tasks::{add_defaults, project_dir};
use crate::term::{
  grid::Rect,
  mouse::{MouseButton, MouseEvent, MouseEventKind},
};
use crate::write_back::{self, How, WriteKind, WritePlan};

use super::App;
use crate::console::ui_tasks::{INDENT, PAD};

/// How long a toast stays.
const TOAST: Duration = Duration::from_secs(4);
/// How long a change to the config's files settles before it is read:
/// an editor's save can be several writes. A burst of changes to the
/// tasks is compared once too.
const SETTLE: Duration = Duration::from_millis(50);

/// What one of the console's own futures does on the console once it is
/// done, to the console as it is by then.
pub(super) struct ConsoleMsg(Box<dyn FnOnce(&mut App) + Send>);

/// Posts to the console from its futures.
#[derive(Clone)]
pub(super) struct Poster(TaskContext);

impl Poster {
  pub(super) fn post(&self, f: impl FnOnce(&mut App) + Send + 'static) {
    self.0.send_msg(self.0.task_id, ConsoleMsg(Box::new(f)));
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
    Poster(self.pc.clone())
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
    self.spawn(async move {
      let modal = f.await;
      move |app: &mut App| {
        let modal = match modal {
          Ok(Some(modal)) => modal,
          Ok(None) => return,
          Err(err) => {
            let root = project_dir(&app.project.config());
            Box::new(ConfigErrorModal::new(&err, &root))
          }
        };
        app.open_if_current(seq, modal);
      }
    });
  }

  /// Shows a modal in place of any other.
  pub(super) fn open(&mut self, modal: Box<dyn Modal>) {
    self.modal = Some(modal);
    self.modal_seq += 1;
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
        self.modal_result(None, result);
      }
      Some(_) | None => {
        if let Err(err) = result {
          self.toast(err.message, true);
        }
      }
    }
  }

  /// Carries a modal's result out.
  pub(super) fn modal_result(
    &mut self,
    observer: Option<ObserverId>,
    result: ModalResult,
  ) {
    match result {
      ModalResult::Keep => (),
      ModalResult::Close => self.modal = None,
      ModalResult::Run(action) => {
        self.modal = None;
        self.handle_action(observer, action);
      }
      ModalResult::Issue(commands) => self.issue_for_modal(commands),
      ModalResult::ApplyReload(mut plan) => {
        self.modal = None;
        let pc = self.pc.clone();
        let project = self.project.clone();
        self.spawn(async move {
          let errors = reload::apply(&pc, &project, &mut plan).await;
          move |app: &mut App| match errors.first() {
            None => app.toast("Reloaded the config.", false),
            Some(first) => app.toast(
              format!("Reloaded, but {} failed: {first}", errors.len()),
              true,
            ),
          }
        });
      }
      ModalResult::WriteBack(plan, how) => {
        self.modal = None;
        self.write_back(*plan, how);
      }
    }
  }

  /// Runs the commands one after another, then tells the modal.
  fn issue_for_modal(&mut self, commands: Vec<Command>) {
    let pc = self.pc.clone();
    let config = self.project.config();
    let issuer = self.modal_seq;
    self.spawn(async move {
      let mut added = Vec::new();
      let mut result = Ok(());
      for (done, command) in commands.iter().enumerate() {
        if let Err(err) = execute(&pc, &config, command).await {
          let message = err.to_string();
          result = Err(IssueError { done, message });
          break;
        }
        if let Command::Add { target, .. } = command
          && let Ok(key) = target.key()
        {
          added.push(key.path.to_string());
        }
      }
      move |app: &mut App| app.issued(issuer, result, added)
    });
  }

  /// Issues a command now; a failure shows as a toast.
  pub(super) fn issue(&self, command: Command) {
    self.issue_with(command, |_| ());
  }

  /// `issue`, and `failed` on the console if it fails.
  pub(super) fn issue_with(
    &self,
    command: Command,
    failed: impl FnOnce(&mut App) + Send + 'static,
  ) {
    let post = self.poster();
    crate::command::issue_then(
      &self.pc,
      &self.project.config(),
      command,
      move |result| {
        if let Err(err) = result {
          post.post(move |app| {
            failed(app);
            app.toast(err.to_string(), true);
          });
        }
      },
    );
  }

  fn describe_then(&self, id: TaskId, purpose: Purpose) {
    let pc = self.pc.clone();
    let seq = self.modal_seq;
    self.spawn(async move {
      let task = pc.describe(TaskSelector::Id(id)).await;
      move |app: &mut App| match task.into_iter().next() {
        Some(task) => app.described(seq, purpose, task),
        None => app.toast("That task is gone.", true),
      }
    });
  }

  fn dialog_context(&self) -> DialogContext {
    let config = self.project.config();
    DialogContext {
      root: project_dir(&config),
      paths: self
        .state
        .tasks
        .iter()
        .map(|task| task.path.to_string())
        .collect(),
      system_shell: config.system_shell,
    }
  }

  pub(super) fn show_add_task(&mut self) {
    let (group, after) = match self.state.tasks.selected() {
      Some((path, Node::Group(_))) => (format!("{path}/"), None),
      Some((path, Node::Task(_))) => (
        path
          .parent()
          .filter(|parent| !parent.is_root())
          .map(|parent| format!("{parent}/"))
          .unwrap_or_default(),
        Some(path.clone()),
      ),
      None => (String::new(), None),
    };
    // What it gets unless the dialog says otherwise.
    let defaults = TaskSpec::from_config(&add_defaults(&self.project.config()));
    self.open(Box::new(TaskDialog::new(
      TaskDialogMode::Add { after, copy: false },
      defaults,
      self.dialog_context(),
      group,
    )));
  }

  pub(super) fn show_edit_task(&mut self, purpose: Purpose) {
    match self.state.current_task() {
      Some(task) => self.describe_then(task.id, purpose),
      None => self.toast("Select a task first.", false),
    }
  }

  fn described(&mut self, seq: u64, purpose: Purpose, task: TaskDescription) {
    let Some(spec) = task.spec.clone() else {
      return self.toast("This task can't be edited.", true);
    };
    let path = task.info.path.as_ref().map_or("", |p| p.as_str());
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
          path: path.to_string(),
          running: task.info.state.is_active(),
          origin: task.origin.clone(),
        };
        let dialog =
          TaskDialog::new(mode, spec, self.dialog_context(), path.to_string());
        self.open_if_current(seq, Box::new(dialog));
      }
      Purpose::Duplicate => {
        let copy_path = self.unique_path(path);
        let root = project_dir(&self.project.config());
        let mut spec = spec.for_copy(path, &root);
        if let Some(label) = spec.label().map(String::from) {
          spec.set_label(Some(&self.unique_label(&label, None)));
        }
        let dialog = TaskDialog::new(
          TaskDialogMode::Add {
            after: task.info.path.clone(),
            copy: true,
          },
          spec,
          self.dialog_context(),
          copy_path,
        );
        self.open_if_current(seq, Box::new(dialog));
      }
    }
  }

  pub(super) fn show_remove(&mut self) {
    let modal = match self.state.tasks.selected() {
      Some((_, Node::Task(task))) => RemoveTaskModal {
        target: Target::Id(task.id),
        what: format!("task {}", task.name()),
        running: task.status.is_active(),
      },
      Some((path, Node::Group(group))) => RemoveTaskModal {
        target: Target::glob(path.as_str()),
        what: match group.len() {
          0 => format!("{} {path}/", group_noun(group)),
          n => format!("{} {path}/ and its {n} tasks", group_noun(group)),
        },
        running: self.state.tasks.any_active(path),
      },
      None => return,
    };
    self.open(Box::new(modal));
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
        let items = vec![
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
          item("Move up", Action::MoveTaskUp),
          item("Move down", Action::MoveTaskDown),
          item(
            &format!("Remove {}…", group_noun(group)),
            Action::ShowRemoveTask,
          )
          .separated(),
        ];
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
    let siblings = self.state.tasks.siblings(&path);
    let Some(at) = siblings.iter().position(|s| *s == path) else {
      return;
    };
    let place = match (down, at) {
      (true, at) if at + 1 < siblings.len() => {
        Place::After(siblings[at + 1].clone())
      }
      (false, 0) | (true, _) => return,
      (false, 1) => Place::First,
      (false, at) => Place::After(siblings[at - 2].clone()),
    };
    self.issue(Command::Move {
      target: Target::glob(path.as_str()),
      to: None,
      place,
    });
  }

  /// The reload dialog; with nothing in it to pick, the reload at once,
  /// for what the config has besides its tasks.
  pub(super) fn show_reload(&mut self) {
    if !self.has_project() {
      return;
    }
    let pc = self.pc.clone();
    let project = self.project.clone();
    let post = self.poster();
    self.spawn_modal(async move {
      let mut plan = reload::plan_reload(&pc, &project).await?;
      if plan.len() > 0 {
        return Ok(Some(Box::new(ReloadModal::new(plan)) as Box<dyn Modal>));
      }
      reload::apply(&pc, &project, &mut plan).await;
      post.post(|app| app.toast(reload::Plan::NOTHING, false));
      Ok(None)
    });
  }

  /// The write-back dialog; `include` adds the runner's own tasks.
  pub(super) fn show_write_back(&mut self, include: Vec<TaskId>) {
    if !self.has_project() {
      return;
    }
    let pc = self.pc.clone();
    let project = self.project.clone();
    let agent = self.agent_name();
    self.spawn_modal(async move {
      let (config, live, base) = reload::load(&pc, &project).await?;
      let root = project_dir(&config);
      let plan = write_back::plan(&config, &root, &base, &live, &include);
      let write_back = WriteBack { plan, agent };
      Ok(Some(
        Box::new(WriteBackModal::new(write_back)) as Box<dyn Modal>
      ))
    });
  }

  /// The agent's program: the one the user config names, or `claude`
  /// when it is installed.
  fn agent_cmd(&self) -> Option<Vec<String>> {
    // Looked for once: a search of PATH on the console's loop.
    static CLAUDE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    match &self.project.config().agent {
      Some(cmd) => Some(cmd.clone()),
      None => (*CLAUDE.get_or_init(|| which::which("claude").is_ok()))
        .then(|| vec!["claude".to_string()]),
    }
  }

  fn agent_name(&self) -> Option<String> {
    self.agent_cmd().and_then(|cmd| cmd.first().cloned())
  }

  fn write_back(&mut self, plan: WritePlan, how: How) {
    match how {
      How::Write => {
        let pc = self.pc.clone();
        let project = self.project.clone();
        self.spawn(async move {
          let plan = Arc::new(plan);
          let to_write = plan.clone();
          let mut failed =
            tokio::task::spawn_blocking(move || write_back::write(&to_write))
              .await
              .unwrap_or_default();
          write_back::written(&pc, &project, &plan, &mut failed).await;
          move |app: &mut App| match failed.first() {
            None => app.toast("Written to the config.", false),
            Some((path, why)) => app.toast(
              format!(
                "{} not written ({path}: {why}); an agent can do it",
                failed.len()
              ),
              true,
            ),
          }
        });
      }
      How::Copy => {
        self.yank(write_back::prompt(&plan));
        self.toast("Copied a prompt for an agent.", false);
      }
      How::Agent => {
        let Some(mut argv) = self.agent_cmd() else {
          return self.toast(
            "No agent: set `agent: {cmd: [...]}` in the user config.",
            true,
          );
        };
        argv.push(write_back::prompt(&plan));
        let config = self.project.config();
        let path = self.unique_path("agent");
        let mut spec = TaskSpec::with_cmd(argv);
        spec.set_label(Some("agent: write back"));
        spec.set("type", Some("job".into()));
        spec.set(
          "cwd",
          Some(project_dir(&config).to_string_lossy().into_owned().into()),
        );
        self.toast("The agent is on it; dekit checks its edits.", false);
        let pc = self.pc.clone();
        let project = self.project.clone();
        let post = self.poster();
        self.spawn(async move {
          let run = async {
            let target = Target::glob(&path);
            let selector = target
              .selector()
              .map_err(|_| format!("No agent task at {path}"))?;
            let mut active = pc.watch_active(selector);
            let add = Command::Add {
              target,
              spec,
              place: Default::default(),
              start: true,
              defaults: true,
              origin: None,
            };
            execute(&pc, &config, &add)
              .await
              .map_err(|err| err.to_string())?;
            // Listed by now: its terminal takes the focus.
            post.post(move |app| {
              app.state.tasks.select_path(&path);
              app.state.scope = Scope::Term;
            });
            // Its edits are checked once it ends, or is removed.
            while active.recv().await == Some(true) {}
            let mut failed = Vec::new();
            write_back::written(&pc, &project, &plan, &mut failed).await;
            Ok::<_, String>(failed)
          };
          let failed = run.await;
          move |app: &mut App| match failed.as_deref() {
            Err(message) => app.toast(message, true),
            Ok([]) => app.toast("The agent's edits are in.", false),
            Ok(failed @ [(path, why), ..]) => app.toast(
              format!(
                "The agent left {} not written ({path}: {why})",
                failed.len()
              ),
              true,
            ),
          }
        });
      }
    }
  }

  /// Compares the config with the tasks again when its files or the
  /// tasks changed since the last time; at most one at a time. The
  /// config on disk is read again only once a file it is read from
  /// changed; a change to the tasks compares them with the last read.
  pub(super) fn refresh_status(&mut self) {
    if !self.status_dirty || self.status_running || !self.has_project() {
      return;
    }
    self.status_dirty = false;
    self.status_running = true;
    let pc = self.pc.clone();
    let project = self.project.clone();
    let generation = self.config_generation;
    let read = self
      .disk_config
      .as_ref()
      .filter(|(at, _)| *at == generation)
      .map(|(_, config)| config.clone());
    self.spawn(async move {
      let status = status(&pc, &project, read).await;
      move |app: &mut App| {
        app.status_running = false;
        match status {
          Ok((config, status)) => {
            app.disk_config = Some((generation, config));
            app.state.config = status;
          }
          Err(_) => {
            app.disk_config = None;
            app.state.config.broken = true;
          }
        }
      }
    });
  }

  /// Where a press, drag, and release on the task list go: a click picks
  /// the row, a drag moves what it started on.
  pub(super) fn sidebar_mouse(
    &mut self,
    observer: ObserverId,
    mouse: &MouseEvent,
  ) {
    let (x, y) = (mouse.x as u16, mouse.y as u16);
    match mouse.kind {
      MouseEventKind::Down(MouseButton::Left) => {
        let row = self.state.tasks.row_at(x, y);
        self.state.drag = row.and_then(|row| {
          let (path, _) = self.state.tasks.row(row.index)?;
          Some(Drag {
            observer,
            from: Arc::new(path.clone()),
            active: false,
            drop: None,
          })
        });
      }
      MouseEventKind::Drag(MouseButton::Left) => {
        let Some((from, was_active)) = self
          .state
          .drag
          .as_ref()
          .filter(|d| d.observer == observer)
          .map(|d| (d.from.clone(), d.active))
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
        let Some(drag) =
          self.state.drag.take_if(|drag| drag.observer == observer)
        else {
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
  /// open group takes it first; further left goes out of groups.
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
            let depth = wanted.min(path.depth().saturating_sub(1));
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
    Some(drop)
  }

  fn drop_on(&mut self, from: &TaskPath, drop: Drop) {
    let to = from.with_parent(&drop.parent);
    self.issue(Command::Move {
      target: Target::glob(from.as_str()),
      to: (to != *from).then(|| Target::glob(to.as_str())),
      place: drop.after.map_or(Place::First, Place::After),
    });
  }
}

/// The config against the tasks: how many changes each way, and which
/// tasks are changed here. It only reads. The config on disk is `read`,
/// as read before while its files are unchanged, or is read now; either
/// way it is returned, for the next comparison.
async fn status(
  pc: &TaskContext,
  project: &Arc<crate::project::Project>,
  read: Option<Arc<Config>>,
) -> anyhow::Result<(Arc<Config>, ConfigStatus)> {
  tokio::time::sleep(SETTLE).await;
  let (config, live, base) = match read {
    Some(config) => {
      let (live, base) = reload::tasks(pc, project).await;
      (config, live, base)
    }
    None => reload::load(pc, project).await?,
  };
  let plan = reload::plan(config.clone(), &base, &live);
  // As many as the reload dialog lists.
  let changes = plan.len();
  let written =
    write_back::plan(&config, &project_dir(&config), &base, &live, &[]);
  let edited: HashSet<TaskId> = written
    .items
    .iter()
    .filter(|item| item.kind == WriteKind::Changed)
    .filter_map(|item| item.here())
    .collect();
  let removed = written
    .items
    .iter()
    .filter(|item| item.kind == WriteKind::Removed)
    .count();
  let own = live
    .tasks
    .iter()
    .filter(|t| t.origin.is_none() && t.spec.is_some())
    .map(|t| t.info.id)
    .collect();
  let status = ConfigStatus {
    broken: false,
    changes,
    edited,
    removed,
    own,
  };
  Ok((config, status))
}

/// What a group row is called: a folder, or a group made by the paths
/// under it.
fn group_noun(group: &Group) -> &'static str {
  match group.folder {
    Some(_) => "folder",
    None => "group",
  }
}
