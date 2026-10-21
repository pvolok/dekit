use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Instant;

use tokio::sync::mpsc::{
  UnboundedReceiver, UnboundedSender, unbounded_channel,
};

use crate::{
  command::Command,
  config::config::Config,
  console::{
    action::{Action, CopyMove, RunnerTab, ScrollUnit},
    app_layout::AppLayout,
    keymap::{Keymap, KeymapGroup},
    modal::{
      commands_menu::CommandsMenuModal,
      menu::{MenuItem, MenuModal},
      modal::{Modal, ModalResult},
      quit::QuitModal,
      runner_select::RunnerSelectModal,
    },
    state::{ConfigStatus, Scope, State},
    task_tree::{Node, TaskTree},
    task_view::TaskView,
    theme::{BlockTheme, Theme},
    ui_header::{HeaderInfo, render_header},
    ui_keymap::render_keymap,
    ui_tasks::render_tasks,
    ui_term::render_term,
    ui_zoom_tip::render_zoom_tip,
  },
  kernel::{
    copy_mode::CopyMove as KernelCopyMove,
    kernel_message::{KernelCommand, TaskContext, TaskRegistration},
    sub_trie::SubMode,
    task::{
      ChannelTask, ExitInfo, TaskCmd, TaskDef, TaskId, TaskKind,
      TaskNotification, TaskNotify, TaskState,
    },
    task_key::TaskKey,
    task_path::{TaskPath, overlaps, path_name, unique},
    task_screen::{
      DEFAULT_SIZE, ObserverId, ScreenNotify, ScrollUnit as KernelScrollUnit,
      TaskScreen, TaskScreenCmd, TaskScreenEffect,
    },
  },
  project::Project,
  protocol::caps::Background,
  spec::TaskSpec,
  target::Target,
  term::{
    CursorStyle, Geometry, Grid, Screen, ScreenHandle, TermEvent,
    grid::Rect,
    key::{Key, KeyEventKind},
    mouse::{MouseButton, MouseEventKind},
    style::Style,
  },
};

use crate::upgrade::snapshot::TaskKind as TaskKindSnapshot;

#[path = "app_edit.rs"]
mod edit;
use edit::{ConsoleMsg, Purpose};

fn kernel_copy_move(dir: CopyMove) -> KernelCopyMove {
  match dir {
    CopyMove::Up => KernelCopyMove::Up,
    CopyMove::Down => KernelCopyMove::Down,
    CopyMove::Left => KernelCopyMove::Left,
    CopyMove::Right => KernelCopyMove::Right,
  }
}

fn kernel_scroll_unit(unit: ScrollUnit) -> KernelScrollUnit {
  match unit {
    ScrollUnit::Line => KernelScrollUnit::Line,
    ScrollUnit::HalfScreen => KernelScrollUnit::HalfScreen,
    ScrollUnit::Screen => KernelScrollUnit::Screen,
  }
}

/// `groups` are the groups a saved console listed, and whether each was
/// collapsed: they are shown so again once listed.
pub fn console_task_registration(
  task_id: TaskId,
  def: TaskDef,
  project: Arc<Project>,
  keymap: Keymap,
  groups: BTreeMap<String, bool>,
) -> TaskRegistration {
  let screen = Screen::new(DEFAULT_SIZE, 0);
  let handle = ScreenHandle::new(screen.frame());
  TaskRegistration {
    task_id,
    def: TaskDef {
      vt: Some(handle.clone()),
      ..def
    },
    factory: Box::new(move |pc| {
      log::debug!("Creating console task (id: {})", pc.task_id.0);
      // Subscribe at registration, so tasks registered later are seen
      // before any message addressed to the console.
      pc.subscribe_path(
        TaskKey::default_space(TaskPath::root()),
        SubMode::Subtree,
      );
      let (tx, rx) = unbounded_channel();
      let app = App::new(project, keymap, rx, pc, screen, handle, groups);
      tokio::spawn(app.run());
      Box::new(ChannelTask::new(tx))
    }),
  }
}

/// The built-in console: one view state shared by every attachment, drawn
/// into its own task screen. It observes the screens of the tasks it
/// shows only while something is attached to it.
pub struct App {
  project: Arc<Project>,
  keymap: Keymap,
  theme: Theme,
  /// The terminal background the latest attached client reported.
  background: Option<Background>,
  state: State,
  modal: Option<Box<dyn Modal>>,
  /// How many modals were opened: the number of the one open.
  modal_seq: u64,
  receiver: UnboundedReceiver<TaskCmd>,
  pc: TaskContext,

  screen: TaskScreen,
  effects: Vec<TaskScreenEffect>,

  /// The console's identity as an observer of task screens, and where
  /// their notifications arrive.
  observer: ObserverId,
  sink: UnboundedSender<ScreenNotify>,
  notifies: UnboundedReceiver<ScreenNotify>,

  stop: bool,
  /// Something the picture depends on changed since the last render.
  dirty: bool,

  /// The tasks changed since the config was last compared with them.
  status_dirty: bool,
  /// A comparison is under way.
  status_running: bool,
  /// The config on disk as last read for the comparison, with the
  /// `config_generation` it was read at: reused while no file it is read
  /// from changed.
  disk_config: Option<(u64, Arc<Config>)>,
  /// Counts the changes to the files the config is read from.
  config_generation: u64,
}

impl App {
  fn new(
    project: Arc<Project>,
    keymap: Keymap,
    receiver: UnboundedReceiver<TaskCmd>,
    pc: TaskContext,
    screen: Screen,
    handle: ScreenHandle,
    groups: BTreeMap<String, bool>,
  ) -> Self {
    let (sink, notifies) = unbounded_channel();
    App {
      state: State {
        scope: Scope::Tasks,
        tasks: TaskTree::with_groups(groups),
        hide_keymap_window: !project.config().tui.tips.show,
        quitting: false,
        header: Default::default(),
        unsaved_hit: None,
        hover: None,
        toast: None,
        config: ConfigStatus::default(),
        drag: None,
      },
      theme: Theme::resolve(project.config().tui.theme, None),
      background: None,
      project,
      keymap,
      modal: None,
      modal_seq: 0,
      receiver,
      screen: TaskScreen::new(pc.task_id, screen, handle, 1),
      pc,
      effects: Vec::new(),
      observer: ObserverId::new(),
      sink,
      notifies,
      stop: false,
      dirty: true,
      status_dirty: true,
      status_running: false,
      disk_config: None,
      config_generation: 0,
    }
  }

  /// Repaints in the theme `tui.theme` asks for, given what the latest
  /// client said about its terminal's background.
  fn apply_theme(&mut self) {
    let theme =
      Theme::resolve(self.project.config().tui.theme, self.background);
    if theme.dark != self.theme.dark {
      self.theme = theme;
      self.dirty = true;
    }
  }

  async fn run(mut self) {
    let mut geometry = self.task_geometry();
    let mut cmds = Vec::new();
    let mut notifies = Vec::new();
    // Only a runner has a config on disk to compare with.
    let mut config_changes = self
      .project
      .config()
      .runner
      .is_some()
      .then(|| self.project.changes());
    while !self.stop {
      let now = self.task_geometry();
      if now != geometry {
        geometry = now;
        for task in self.state.tasks.iter() {
          self.pc.send_msg(
            task.id,
            TaskScreenCmd::Input {
              observer: self.observer,
              event: TermEvent::Resize(now),
            },
          );
        }
      }

      tokio::select! {
        n = self.receiver.recv_many(&mut cmds, 512) => {
          // Zero means the kernel is gone.
          if n == 0 {
            break;
          }
          for cmd in cmds.drain(..) {
            self.dirty = true;
            self.handle_task_cmd(cmd);
          }
        }
        _ = self.notifies.recv_many(&mut notifies, 512) => {
          for notify in notifies.drain(..) {
            self.handle_screen_notify(notify);
          }
        }
        () = changed(&mut config_changes) => {
          // The config on disk is read again for the next comparison.
          self.config_generation = self.config_generation.wrapping_add(1);
          self.status_dirty = true;
          // As the user config says since a reload took it in.
          self.keymap = self.project.config().keymap.build();
          self.apply_theme();
          self.dirty = true;
        }
        () = sleep_until(self.state.toast.as_ref().map(|toast| toast.until)) => {
          self.state.toast = None;
          self.dirty = true;
        }
      }

      self.refresh_status();
      if self.dirty && self.screen.has_observers() {
        self.dirty = false;
        self.render();
      }
    }
    self.pc.unsubscribe_path(
      TaskKey::default_space(TaskPath::root()),
      SubMode::Subtree,
    );
    self.pc.send(KernelCommand::TaskStopped(ExitInfo::code(0)));
  }

  fn render(&mut self) {
    let layout = self.layout();
    let client_versions: Vec<String> =
      self.screen.client_versions().map(String::from).collect();
    let theme = &self.theme;
    let vt = self.screen.screen_mut();
    let grid = vt.grid_mut();
    let bg = Style::default()
      .bg(theme.block.bg.into())
      .fg(theme.block.text.into());
    let bg = grid.intern_style(bg);
    grid.erase_all(bg);
    grid.cursor_pos = None;
    grid.cursor_style = CursorStyle::Default;

    let config = self.project.config();
    let header = render_header(
      layout.header,
      grid,
      &HeaderInfo {
        runner: config.runner.as_ref(),
        version: env!("CARGO_PKG_VERSION"),
        client_versions: client_versions.iter().map(String::as_str).collect(),
        hover: self.state.hover,
        held: self.modal.as_ref().and_then(|modal| modal.unshaded()),
        config_changes: self.state.config.changes,
        config_error: self.state.config.broken,
      },
      theme,
    );
    self.state.header = header;
    render_tasks(
      layout.sidebar,
      grid,
      &mut self.state,
      &config,
      &self.keymap,
      theme,
    );
    render_term(layout.term, grid, &self.state, theme);
    render_keymap(layout.keymap, grid, &self.state, &self.keymap, theme);
    render_zoom_tip(layout.zoom_banner, grid, &self.keymap, theme);
    if let Some(modal) = &mut self.modal {
      shade(grid, &theme.block, modal.unshaded());
      grid.cursor_pos = None;
      grid.cursor_style = CursorStyle::Default;
      modal.render(grid, &self.keymap, theme);
    }

    // The frame takes the cursor from the screen, not the grid.
    let cursor_style = grid.cursor_style;
    match grid.cursor_pos {
      Some(pos) => {
        grid.set_pos(pos);
        vt.set_hide_cursor(false);
      }
      None => vt.set_hide_cursor(true),
    }
    vt.set_cursor_style(cursor_style);
    self.with_screen(TaskScreen::rendered);
  }

  /// The whole screen.
  fn frame(&self) -> Rect {
    let size = self.screen.size();
    Rect::new(0, 0, size.width, size.height)
  }

  fn layout(&self) -> AppLayout {
    AppLayout::new(
      self.frame(),
      self.state.scope.is_zoomed(),
      self.state.hide_keymap_window,
      &self.project.config(),
    )
  }

  fn add_task(
    &mut self,
    id: TaskId,
    label: Option<String>,
    path: Option<TaskPath>,
    after: Option<TaskId>,
    kind: TaskKind,
    status: TaskState,
    vt: Option<ScreenHandle>,
  ) {
    // Console tasks always have a path.
    let (Some(vt), Some(path)) = (vt, path) else {
      return;
    };
    self.state.tasks.add(
      TaskView {
        id,
        label,
        path: Arc::new(path),
        kind,
        status,
        vt,
        present: None,
      },
      after,
    );
    if self.screen.has_observers() {
      self.observe(id);
    }
  }

  fn observe(&self, id: TaskId) {
    self.pc.send_msg(
      id,
      TaskScreenCmd::Attach {
        observer: self.observer,
        geometry: self.task_geometry(),
        sink: self.sink.clone(),
        client_version: None,
        background: None,
      },
    );
  }

  /// What the task screens the console shows are told about their size:
  /// the terminal pane in cells, and the cells' size as the console's
  /// own clients report it.
  fn task_geometry(&self) -> Geometry {
    Geometry {
      size: self.layout().term_area().size(),
      cell_pixels: self.screen.reported_cell_pixels(),
    }
  }

  fn observe_all(&self) {
    for task in self.state.tasks.iter() {
      self.observe(task.id);
    }
  }

  fn unobserve_all(&mut self) {
    for task in self.state.tasks.iter_mut() {
      task.present = None;
      self.pc.send_msg(
        task.id,
        TaskScreenCmd::Detach {
          observer: self.observer,
        },
      );
    }
  }

  fn handle_task_cmd(&mut self, cmd: TaskCmd) {
    let msg = match cmd {
      TaskCmd::Start(_) => {
        self.pc.send(KernelCommand::TaskStarted);
        self.pc.send(KernelCommand::TaskReady);
        return;
      }
      TaskCmd::Stop | TaskCmd::Kill => {
        self.stop = true;
        return;
      }
      TaskCmd::Thaw => return,
      // Attachments re-attach and the task list is replayed on resume:
      // what is carried is how the groups were shown.
      TaskCmd::Freeze(number) => {
        self.pc.send(KernelCommand::TaskFrozen(
          number,
          TaskKindSnapshot::Console {
            groups: self.state.tasks.groups(),
          },
        ));
        return;
      }
      TaskCmd::Msg(msg) => msg,
    };
    let msg = match msg.downcast::<Action>() {
      Ok(action) => return self.handle_action(None, *action),
      Err(msg) => msg,
    };
    let msg = match msg.downcast::<ConsoleMsg>() {
      Ok(msg) => return self.handle_console_msg(*msg),
      Err(msg) => msg,
    };
    let msg = match msg.downcast::<TaskScreenCmd>() {
      Ok(cmd) => return self.handle_screen_cmd(*cmd),
      Err(msg) => msg,
    };
    match msg.downcast::<TaskNotification>() {
      Ok(n) => self.handle_task_notify(n.from, n.notify),
      Err(_) => log::error!("Console received unknown message"),
    }
  }

  /// Commands addressed to the console's own screen. Keys, mouse, and
  /// pastes are the UI's input; attachments and their geometry go to the
  /// screen.
  fn handle_screen_cmd(&mut self, cmd: TaskScreenCmd) {
    let cmd = match cmd {
      TaskScreenCmd::Input {
        observer,
        event: TermEvent::Key(key),
      } => return self.handle_key(observer, key),
      TaskScreenCmd::Input {
        observer,
        event: TermEvent::Mouse(mouse),
      } => return self.handle_mouse(observer, mouse),
      TaskScreenCmd::Input {
        observer,
        event: TermEvent::Paste(text),
      } => {
        if let Some(modal) = &mut self.modal {
          let result = modal.handle_paste(&text);
          self.modal_result(Some(observer), result);
        } else if self.state.scope.is_term()
          && let Some(task) = self.state.current_task()
        {
          self.input_current(Some(task.id), TermEvent::Paste(text));
        }
        return;
      }
      cmd @ TaskScreenCmd::Detach { .. } => {
        // The pointer that left would otherwise stay lit for everyone.
        self.state.hover = None;
        cmd
      }
      // A console attachment is what makes a project recent.
      cmd @ TaskScreenCmd::Attach { background, .. } => {
        if let Some(background) = background {
          // The latest client to say decides for everyone attached.
          self.background = Some(background);
          self.apply_theme();
        }
        if let Some(runner) = self.project.config().runner.clone() {
          tokio::task::spawn_blocking(move || {
            crate::runner::recent::touch(&runner)
          });
        }
        cmd
      }
      cmd @ TaskScreenCmd::Input { .. } => cmd,
      // Copy mode and scrolling do not apply to a composed UI.
      TaskScreenCmd::CopyEnter
      | TaskScreenCmd::CopyLeave
      | TaskScreenCmd::CopyMove { .. }
      | TaskScreenCmd::CopyBeginSelection
      | TaskScreenCmd::Scroll { .. }
      | TaskScreenCmd::CopyYank => return,
    };
    self.with_screen(|screen, effects| screen.handle_cmd(cmd, effects));
  }

  /// Runs `f` on the console's own screen, then what it asked for.
  fn with_screen(
    &mut self,
    f: impl FnOnce(&mut TaskScreen, &mut Vec<TaskScreenEffect>),
  ) {
    let attached = self.screen.has_observers();
    f(&mut self.screen, &mut self.effects);
    self.apply_screen(attached);
  }

  /// Copies `text` for whoever is attached.
  pub(super) fn yank(&mut self, text: String) {
    self.with_screen(|screen, effects| screen.yank(text, effects));
  }

  /// Applies what the screen asked for, and starts or stops watching task
  /// screens when the first attachment arrives or the last one leaves.
  fn apply_screen(&mut self, attached: bool) {
    // The screen resizes the vt itself; nothing else applies to a UI.
    self.effects.clear();
    match (attached, self.screen.has_observers()) {
      (false, true) => self.observe_all(),
      (true, false) => self.unobserve_all(),
      (false, false) | (true, true) => (),
    }
  }

  fn handle_mouse(
    &mut self,
    observer: ObserverId,
    mouse: crate::term::mouse::MouseEvent,
  ) {
    let frame = self.frame();
    self.state.hover = Some((mouse.x as u16, mouse.y as u16));
    if let Some(modal) = &mut self.modal {
      let outside = !modal.area(frame).contains(mouse.x as u16, mouse.y as u16);
      let result = match mouse.kind {
        MouseEventKind::Down(_) if outside => modal.click_outside(),
        _ if outside => ModalResult::Keep,
        _ => modal.handle_mouse(&mouse),
      };
      self.modal_result(Some(observer), result);
      return;
    }
    // What is dragged from the task list is dragged until let go,
    // wherever the pointer is; a press means the release was missed.
    if self
      .state
      .drag
      .as_ref()
      .is_some_and(|drag| drag.observer == observer)
    {
      if matches!(mouse.kind, MouseEventKind::Down(_)) {
        self.state.drag = None;
      } else {
        self.sidebar_mouse(observer, &mouse);
        return;
      }
    }
    let layout = self.layout();
    let (x, y) = (mouse.x as u16, mouse.y as u16);
    let term_area = layout.term_area();
    // A selection dragged out of the terminal keeps driving it from the
    // nearest edge cell, so the kernel can scroll the view under it. The
    // release has to reach the kernel too, or the drag never ends.
    let drag_outside =
      matches!(
        mouse.kind,
        MouseEventKind::Drag(MouseButton::Left)
          | MouseEventKind::Up(MouseButton::Left)
      ) && matches!(self.state.scope, Scope::Term | Scope::TermZoom)
        && self.state.drag.is_none()
        && !term_area.contains(x, y)
        && term_area.width > 0
        && term_area.height > 0;
    if drag_outside {
      if let Some(task) = self.state.current_task() {
        let mut event = mouse;
        event.x = x.clamp(term_area.x, term_area.right() - 1) as i32;
        event.y = y.clamp(term_area.y, term_area.bottom() - 1) as i32;
        let event = event.translate(term_area);
        self.input_current(Some(task.id), TermEvent::Mouse(event));
      }
      return;
    }
    if layout.header.contains(x, y) {
      if mouse.kind == MouseEventKind::Down(MouseButton::Left) {
        let hits = self.state.header;
        if hits.logo.is_some_and(|hit| hit.contains(x, y)) {
          self.handle_action(Some(observer), Action::ShowMainMenu);
        } else if hits.reload.is_some_and(|hit| hit.contains(x, y)) {
          self.handle_action(Some(observer), Action::ShowReload);
        } else if hits.runner.is_some_and(|hit| hit.contains(x, y)) {
          self.handle_action(
            Some(observer),
            Action::ShowRunnerSelect {
              tab: RunnerTab::Switch,
            },
          );
        }
      }
      return;
    }
    let pressed = match mouse.kind {
      MouseEventKind::Down(_) => true,
      MouseEventKind::Up(_)
      | MouseEventKind::Drag(_)
      | MouseEventKind::Moved
      | MouseEventKind::ScrollDown
      | MouseEventKind::ScrollUp
      | MouseEventKind::ScrollLeft
      | MouseEventKind::ScrollRight => false,
    };
    if mouse.kind == MouseEventKind::Down(MouseButton::Left)
      && self.state.unsaved_hit.is_some_and(|hit| hit.contains(x, y))
    {
      self.handle_action(Some(observer), Action::ShowWriteBack);
      return;
    }
    let term_area = layout.term_area();
    if term_area.contains(x, y) {
      if pressed && self.state.scope == Scope::Tasks {
        self.state.scope = Scope::Term;
      }
      if let Some(task) = self.state.current_task() {
        let event = mouse.translate(term_area);
        self.input_current(Some(task.id), TermEvent::Mouse(event));
      }
    } else if layout.sidebar.contains(x, y) {
      if pressed && self.state.scope == Scope::Term {
        self.state.scope = Scope::Tasks;
      }
      self.sidebar_mouse(observer, &mouse);
    }
  }

  fn handle_key(&mut self, observer: ObserverId, key: Key) {
    match key.kind {
      KeyEventKind::Press | KeyEventKind::Repeat => (),
      KeyEventKind::Release => return,
    }
    if let Some(drag) = &self.state.drag
      && drag.observer == observer
      && key.code == crate::term::key::KeyCode::Esc
    {
      // Esc lets go of what is dragged.
      self.state.drag = None;
      return;
    }
    if let Some(modal) = &mut self.modal {
      let result = modal.handle_key(&key);
      self.modal_result(Some(observer), result);
      return;
    }
    let group = self.state.keymap_group();
    if let Some(action) = self.keymap.action(group, &key.plain()) {
      self.handle_action(Some(observer), action.clone());
    } else if group == KeymapGroup::Term {
      // Unbound keys go to the process; in copy mode the keymap is the
      // whole vocabulary, so they are dropped rather than fed to the
      // screen's own copy-mode keys.
      self.handle_action(Some(observer), Action::SendKey { key });
    }
  }

  /// Runs an action, retaining the attachment that originated it when it
  /// came from terminal input. A quit from an attachment detaches only that
  /// observer; an out-of-band quit action still addresses the runner.
  fn handle_action(&mut self, observer: Option<ObserverId>, action: Action) {
    let current = self.state.current_task().map(|t| t.id);
    match action {
      Action::Batch { cmds } => {
        for cmd in cmds {
          self.handle_action(observer, cmd);
        }
      }

      Action::ShowQuit => self.open(Box::new(QuitModal::default())),
      Action::Detach => {
        if let Some(observer) = observer {
          self.handle_screen_cmd(TaskScreenCmd::Detach { observer });
        }
      }
      Action::Down | Action::Quit => {
        let command = match action {
          Action::Down => Command::Down,
          _ => Command::Quit,
        };
        self.state.quitting = true;
        self.issue_with(command, |app| app.state.quitting = false);
      }
      Action::Command { command } => self.issue(command),

      Action::ToggleFocus => self.state.scope = self.state.scope.toggle(),
      Action::FocusTasks => self.state.scope = Scope::Tasks,
      Action::FocusTerm => self.state.scope = Scope::Term,
      Action::Zoom => self.state.scope = Scope::TermZoom,

      Action::ShowCommandsMenu => {
        self.open(Box::new(CommandsMenuModal::new()));
      }
      Action::ShowMainMenu => {
        let anchor = self.state.header.logo.unwrap_or_default();
        let keymap = &self.keymap;
        let item =
          |label: &str, action: Action| MenuItem::new(label, action, keymap);
        let mut items = vec![item("All commands", Action::ShowCommandsMenu)];
        if self.has_project() {
          items.extend([
            item(
              "Open project",
              Action::ShowRunnerSelect {
                tab: RunnerTab::Open,
              },
            ),
            item(
              "Switch project",
              Action::ShowRunnerSelect {
                tab: RunnerTab::Switch,
              },
            ),
            item("Reload the config…", Action::ShowReload).separated(),
            item("Write back to the config…", Action::ShowWriteBack),
          ]);
        }
        items.push(item("Quit", Action::ShowQuit).separated());
        self.open(Box::new(MenuModal::new(anchor, items)));
      }
      Action::ShowRunnerSelect { tab } => {
        if self.has_project() {
          let runner = self.project.config().runner.clone();
          self.spawn_modal(async move {
            // It reads the runners' records and a directory.
            let modal = tokio::task::spawn_blocking(move || {
              RunnerSelectModal::new(runner.as_ref(), tab)
            })
            .await;
            Ok(modal.ok().map(|modal| Box::new(modal) as Box<dyn Modal>))
          });
        }
      }
      Action::SwitchRunner { kind, root } => match observer {
        Some(observer) => {
          self
            .screen
            .notify(observer, ScreenNotify::Switch { kind, root });
        }
        None => log::warn!("switch-runner needs an attached client"),
      },
      Action::CloseCurrentModal => self.modal = None,
      Action::ToggleKeymapWindow => {
        self.state.hide_keymap_window = !self.state.hide_keymap_window;
      }
      Action::ToggleTheme => {
        self.theme = if self.theme.dark {
          Theme::light()
        } else {
          Theme::dark()
        };
      }

      Action::NextTask => self.state.tasks.next(),
      Action::PrevTask => self.state.tasks.prev(),
      Action::Expand => self.state.tasks.expand(),
      Action::Collapse => self.state.tasks.collapse(),
      Action::Fold { op, target } => {
        self.state.tasks.fold(op, target.as_deref())
      }
      Action::SelectTask { index } => self.state.tasks.select_task(index),

      Action::StartTask => {
        self.issue_selected(|target| Command::Start { target })
      }
      Action::StopTask => {
        self.issue_selected(|target| Command::Stop { target })
      }
      Action::KillTask => {
        self.issue_selected(|target| Command::Kill { target })
      }
      Action::VetoTask => {
        self.issue_selected(|target| Command::Veto { target })
      }
      Action::RestartTask => {
        self.issue_selected(|target| Command::Restart { target })
      }
      Action::ForceRestartTask => {
        self.issue_selected(|target| Command::ForceRestart { target })
      }
      Action::RestartAll => {
        self.issue_all(|target| Command::Restart { target })
      }
      Action::ForceRestartAll => {
        self.issue_all(|target| Command::ForceRestart { target })
      }

      Action::ShowAddTask => self.show_add_task(),
      Action::ShowEditTask => self.show_edit_task(Purpose::Edit),
      Action::ShowDuplicateTask => self.show_edit_task(Purpose::Duplicate),
      Action::CopyTaskCommand => self.show_edit_task(Purpose::Copy),
      Action::ShowTaskMenu => self.show_task_menu(None, false),
      Action::MoveTaskUp => self.move_selected(false),
      Action::MoveTaskDown => self.move_selected(true),
      Action::ShowReload => self.show_reload(),
      Action::ShowWriteBack => self.show_write_back(Vec::new()),
      Action::SaveToConfig => {
        if let Some(task) = self.state.current_task() {
          let id = task.id;
          self.show_write_back(vec![id]);
        }
      }
      Action::AddTask { cmd, name } => {
        let label =
          self.unique_label(&name.unwrap_or_else(|| cmd.clone()), None);
        let path = self.unique_path(&path_name(&label));
        let argv = crate::parse_shell::typed_argv(
          &cmd,
          self.project.config().system_shell,
        );
        match (path.parse::<Target>(), argv) {
          (Ok(target), Ok(argv)) => {
            let mut spec = TaskSpec::with_cmd(argv);
            spec.set_label(Some(&label));
            self.issue(Command::Add {
              target,
              spec,
              place: Default::default(),
              start: true,
              defaults: true,
              origin: None,
            })
          }
          (_, Err(err)) => self.toast(format!("`{cmd}`: {err}"), true),
          (Err(err), Ok(_)) => log::warn!("Cannot add task '{path}': {err}"),
        }
      }
      Action::DuplicateTask => {
        if let Some(task) = self.state.current_task() {
          let name = self.unique_label(&task.name(), None);
          self.issue(Command::Duplicate {
            target: Target::Id(task.id),
            name: Some(name),
            path: None,
            start: true,
          });
        }
      }
      Action::RenameTask { name } => {
        if let Some(id) = current {
          let name = self.unique_label(&name, Some(id));
          self.issue(Command::Rename {
            target: Target::Id(id),
            name,
          });
        }
      }
      Action::ShowRemoveTask => self.show_remove(),

      Action::ScrollUp { n, unit } => {
        self.send_current(
          current,
          TaskScreenCmd::Scroll {
            delta: n.min(i32::MAX as usize) as i32,
            unit: kernel_scroll_unit(unit),
          },
        );
      }
      Action::ScrollDown { n, unit } => {
        self.send_current(
          current,
          TaskScreenCmd::Scroll {
            delta: -(n.min(i32::MAX as usize) as i32),
            unit: kernel_scroll_unit(unit),
          },
        );
      }
      Action::CopyModeEnter => {
        if current.is_some() {
          self.state.scope = Scope::Term;
        }
        self.send_current(current, TaskScreenCmd::CopyEnter);
      }
      Action::CopyModeLeave => {
        self.send_current(current, TaskScreenCmd::CopyLeave)
      }
      Action::CopyModeMove { dir } => self.send_current(
        current,
        TaskScreenCmd::CopyMove {
          dir: kernel_copy_move(dir),
        },
      ),
      Action::CopyModeEnd => {
        self.send_current(current, TaskScreenCmd::CopyBeginSelection)
      }
      Action::CopyModeCopy => {
        self.send_current(current, TaskScreenCmd::CopyYank)
      }
      Action::SendKey { key } => {
        self.input_current(current, TermEvent::Key(key))
      }
    }
  }

  /// Whether the console is a runner's, with a project to switch from,
  /// reload, and write back to; `dekit mprocs` has none.
  fn has_project(&self) -> bool {
    self.project.config().runner.is_some()
  }

  /// `unique`, a path taken while a task is there or under it.
  fn unique_path(&self, base: &str) -> String {
    unique(base, |path| {
      self
        .state
        .tasks
        .iter()
        .any(|t| overlaps(t.path.as_str(), path))
    })
  }

  fn unique_label(&self, base: &str, exclude: Option<TaskId>) -> String {
    unique(base, |name| {
      self
        .state
        .tasks
        .iter()
        .any(|t| Some(t.id) != exclude && t.name() == name)
    })
  }

  fn send_current(&self, current: Option<TaskId>, cmd: TaskScreenCmd) {
    if let Some(id) = current {
      self.pc.send_msg(id, cmd);
    }
  }

  fn input_current(&self, current: Option<TaskId>, event: TermEvent) {
    self.send_current(
      current,
      TaskScreenCmd::Input {
        observer: self.observer,
        event,
      },
    );
  }

  /// Runs a command on the selected task or every task in the selected group.
  fn issue_selected(&self, make: fn(Target) -> Command) {
    let target = match self.state.tasks.selected() {
      Some((_, Node::Task(task))) => Target::Id(task.id),
      Some((path, Node::Group(_))) => Target::glob(&format!("{path}/**")),
      None => return,
    };
    self.issue(make(target));
  }

  fn issue_all(&self, make: fn(Target) -> Command) {
    let commands = self
      .state
      .tasks
      .iter()
      .map(|t| make(Target::Id(t.id)))
      .collect();
    self.issue(Command::Batch { commands });
  }

  fn handle_screen_notify(&mut self, notify: ScreenNotify) {
    match notify {
      // Only the shown task's screen is part of the picture.
      ScreenNotify::Render { screen } => {
        if self.shown_task() == Some(screen) {
          self.dirty = true;
        }
      }
      ScreenNotify::Attached => self.dirty = true,
      // Addressed to attach sessions, never to a screen's observer.
      ScreenNotify::Bell | ScreenNotify::Switch { .. } => (),
      ScreenNotify::CopyPresent { screen, handle } => {
        if let Some(task) = self.state.tasks.task_mut(screen) {
          task.present = handle;
          self.dirty = true;
        }
      }
      // Copied on behalf of whoever is attached here.
      ScreenNotify::Yank { text } => self.yank(text),
    }
  }

  /// The task whose terminal the console shows.
  fn shown_task(&self) -> Option<TaskId> {
    match self.state.tasks.selected() {
      Some((_, Node::Task(task))) => Some(task.id),
      _ => None,
    }
  }

  fn handle_task_notify(&mut self, id: TaskId, notify: TaskNotify) {
    // What the config is compared with: the tasks, not their states.
    self.status_dirty |= !matches!(notify, TaskNotify::StateChanged(_));
    match notify {
      TaskNotify::Added {
        path,
        label,
        after,
        kind,
        state,
        vt,
      } => self.add_task(id, label, path, after, kind, state, vt),
      TaskNotify::FolderAdded {
        path,
        after,
        collapsed,
      } => self.state.tasks.add_folder(id, path, after, collapsed),
      TaskNotify::StateChanged(state) => self.state.tasks.set_status(id, state),
      TaskNotify::Removed => self.state.tasks.remove(id),
      TaskNotify::Changed { label, kind } => {
        if let Some(task) = self.state.tasks.task_mut(id) {
          task.label = label;
          task.kind = kind;
        }
      }
      TaskNotify::Moved { path, after } => {
        if let Some(path) = path {
          self.state.tasks.move_task(id, path, after);
        }
      }
    }
  }
}

/// Waits until `at`, or for ever without one.
async fn sleep_until(at: Option<Instant>) {
  match at {
    Some(at) => tokio::time::sleep_until(at.into()).await,
    None => std::future::pending().await,
  }
}

/// Waits for the config's files to change, or for ever without a watch.
async fn changed(changes: &mut Option<tokio::sync::watch::Receiver<u64>>) {
  if let Some(changes) = changes
    && changes.changed().await.is_ok()
  {
    return;
  }
  std::future::pending().await
}

/// Dims everything but `keep`.
fn shade(grid: &mut Grid, theme: &BlockTheme, keep: Option<Rect>) {
  let area = grid.area();
  let dim = |style: Style| Style {
    fg: style.fg.blend(theme.shade, theme.shade_amount),
    bg: style.bg.blend(theme.shade, theme.shade_amount),
    ..style
  };
  let Some(keep) = keep.map(|keep| keep.intersect(area)) else {
    grid.map_styles(area, dim);
    return;
  };
  // The four bands around `keep`.
  let above = Rect::new(area.x, area.y, area.width, keep.y - area.y);
  let below = Rect::new(
    area.x,
    keep.bottom(),
    area.width,
    area.bottom() - keep.bottom(),
  );
  let left = Rect::new(area.x, keep.y, keep.x - area.x, keep.height);
  let right = Rect::new(
    keep.right(),
    keep.y,
    area.right() - keep.right(),
    keep.height,
  );
  for band in [above, below, left, right] {
    if band.width > 0 && band.height > 0 {
      grid.map_styles(band, dim);
    }
  }
}
