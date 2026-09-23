use std::sync::Arc;

use tokio::sync::mpsc::{
  UnboundedReceiver, UnboundedSender, unbounded_channel,
};

use crate::{
  command::{Command, issue},
  config::{config::Config, task::CmdConfig},
  console::{
    action::{Action, CopyMove, ScrollUnit},
    app_layout::AppLayout,
    keymap::{Keymap, KeymapGroup},
    modal::{
      add_task::AddTaskModal,
      commands_menu::CommandsMenuModal,
      modal::{Modal, ModalResult},
      quit::QuitModal,
      remove_task::RemoveTaskModal,
      rename_task::RenameTaskModal,
    },
    state::{Scope, State},
    task_view::TaskView,
    ui_keymap::render_keymap,
    ui_tasks::{render_tasks, task_at},
    ui_term::render_term,
    ui_zoom_tip::render_zoom_tip,
    widgets::list::ListState,
  },
  kernel::{
    copy_mode::CopyMove as KernelCopyMove,
    kernel_message::{KernelCommand, SharedVt, TaskContext, TaskRegistration},
    sub_trie::SubMode,
    task::{
      ChannelTask, ExitInfo, TaskCmd, TaskDef, TaskId, TaskNotification,
      TaskNotify, TaskState,
    },
    task_key::TaskKey,
    task_path::{TaskPath, is_valid_component_char},
    task_screen::{
      DEFAULT_SIZE, ObserverId, ScreenNotify, ScrollUnit as KernelScrollUnit,
      TaskScreen, TaskScreenCmd, TaskScreenEffect,
    },
  },
  target::Target,
  term::{
    CursorStyle, Screen, Size, TermEvent, Winsize,
    attrs::Attrs,
    grid::Rect,
    key::{Key, KeyEventKind},
    mouse::{MouseButton, MouseEventKind},
  },
};

use crate::upgrade::snapshot::TaskKind as TaskKindSnapshot;

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

/// A task path component made from free text such as a shell command.
fn path_name(text: &str) -> String {
  let name: String = text
    .chars()
    .map(|c| if is_valid_component_char(c) { c } else { '-' })
    .collect();
  let name = name.trim_matches('-').to_string();
  if name.is_empty() {
    "task".to_string()
  } else {
    name
  }
}

/// `base`, or `base-2`, `base-3`, ... — the first that is not `taken`.
fn unique(base: &str, taken: impl Fn(&str) -> bool) -> String {
  if !taken(base) {
    return base.to_string();
  }
  (2..)
    .map(|n| format!("{}-{}", base, n))
    .find(|name| !taken(name))
    .unwrap()
}

fn winsize(size: Size) -> Winsize {
  Winsize {
    x: size.width,
    y: size.height,
    x_px: 0,
    y_px: 0,
  }
}

pub fn console_task_registration(
  task_id: TaskId,
  def: TaskDef,
  config: Arc<Config>,
  keymap: Keymap,
) -> TaskRegistration {
  let vt = SharedVt::new(Screen::new(DEFAULT_SIZE, 0));
  TaskRegistration {
    task_id,
    def: TaskDef {
      vt: Some(vt.clone()),
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
      tokio::spawn(App::new(config, keymap, rx, pc, vt).run());
      Box::new(ChannelTask::new(tx))
    }),
  }
}

/// The built-in console: one view state shared by every attachment, drawn
/// into its own task screen. It observes the screens of the tasks it
/// shows only while something is attached to it.
pub struct App {
  config: Arc<Config>,
  keymap: Keymap,
  state: State,
  modal: Option<Box<dyn Modal>>,
  receiver: UnboundedReceiver<TaskCmd>,
  pc: TaskContext,

  vt: SharedVt,
  screen: TaskScreen,
  effects: Vec<TaskScreenEffect>,

  /// The console's identity as an observer of task screens, and where
  /// their notifications arrive.
  observer: ObserverId,
  sink: UnboundedSender<ScreenNotify>,
  notifies: UnboundedReceiver<ScreenNotify>,

  stop: bool,
}

impl App {
  fn new(
    config: Arc<Config>,
    keymap: Keymap,
    receiver: UnboundedReceiver<TaskCmd>,
    pc: TaskContext,
    vt: SharedVt,
  ) -> Self {
    let (sink, notifies) = unbounded_channel();
    App {
      state: State {
        scope: Scope::Tasks,
        tasks: Vec::new(),
        tasks_list: ListState::default(),
        hide_keymap_window: !config.tui.tips.show,
        quitting: false,
      },
      config,
      keymap,
      modal: None,
      receiver,
      screen: TaskScreen::new(pc.task_id, vt.clone(), 1),
      pc,
      vt,
      effects: Vec::new(),
      observer: ObserverId::new(),
      sink,
      notifies,
      stop: false,
    }
  }

  async fn run(mut self) {
    let mut term_size = self.layout().term_area().size();
    let mut cmds = Vec::new();
    let mut notifies = Vec::new();
    while !self.stop {
      let size = self.layout().term_area().size();
      if size != term_size {
        term_size = size;
        for task in &self.state.tasks {
          self.pc.send_msg(
            task.id,
            TaskScreenCmd::Input {
              observer: self.observer,
              event: TermEvent::Resize(size.width, size.height),
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
            self.handle_task_cmd(cmd);
          }
        }
        _ = self.notifies.recv_many(&mut notifies, 512) => {
          for notify in notifies.drain(..) {
            self.handle_screen_notify(notify);
          }
        }
      }

      if self.screen.has_observers() {
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
    let Ok(mut vt) = self.vt.write() else {
      return;
    };
    let grid = vt.grid_mut();
    grid.erase_all(Attrs::default());
    grid.cursor_pos = None;
    grid.cursor_style = CursorStyle::Default;

    render_tasks(layout.sidebar, grid, &mut self.state, &self.config);
    render_term(layout.term, grid, &self.state);
    render_keymap(layout.keymap, grid, &self.state, &self.keymap);
    render_zoom_tip(layout.zoom_banner, grid, &self.keymap);
    if let Some(modal) = &mut self.modal {
      grid.cursor_pos = None;
      grid.cursor_style = CursorStyle::Default;
      modal.render(grid, &self.keymap);
    }

    match grid.cursor_pos {
      Some(pos) => {
        grid.set_pos(pos);
        vt.set_hide_cursor(false);
      }
      None => vt.set_hide_cursor(true),
    }
    drop(vt);
    let attached = self.screen.has_observers();
    self.screen.rendered(&mut self.effects);
    self.apply_screen(attached);
  }

  fn layout(&self) -> AppLayout {
    let size = self.vt.read().map(|vt| vt.size()).unwrap_or(DEFAULT_SIZE);
    AppLayout::new(
      Rect::new(0, 0, size.width, size.height),
      self.state.scope.is_zoomed(),
      self.state.hide_keymap_window,
      &self.config,
    )
  }

  fn add_task(
    &mut self,
    id: TaskId,
    label: Option<String>,
    path: Option<TaskPath>,
    status: TaskState,
    vt: Option<SharedVt>,
  ) {
    let Some(vt) = vt else {
      return;
    };
    self.state.tasks.push(TaskView {
      id,
      label,
      path,
      status,
      vt,
      present: None,
    });
    if self.screen.has_observers() {
      self.observe(id);
    }
  }

  fn observe(&self, id: TaskId) {
    self.pc.send_msg(
      id,
      TaskScreenCmd::Attach {
        observer: self.observer,
        size: winsize(self.layout().term_area().size()),
        sink: self.sink.clone(),
      },
    );
  }

  fn observe_all(&self) {
    for task in &self.state.tasks {
      self.observe(task.id);
    }
  }

  fn unobserve_all(&mut self) {
    for task in &mut self.state.tasks {
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
      TaskCmd::Start => {
        self.pc.send(KernelCommand::TaskStarted);
        self.pc.send(KernelCommand::TaskReady);
        return;
      }
      TaskCmd::Stop | TaskCmd::Kill => {
        self.stop = true;
        return;
      }
      TaskCmd::Duplicate(_) | TaskCmd::Thaw => return,
      // The console keeps no state worth carrying: attachments re-attach
      // and the task list is replayed on resume.
      TaskCmd::Freeze(number) => {
        self
          .pc
          .send(KernelCommand::TaskFrozen(number, TaskKindSnapshot::Console));
        return;
      }
      TaskCmd::Msg(msg) => msg,
    };
    let msg = match msg.downcast::<Action>() {
      Ok(action) => return self.handle_action(None, *action),
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
        event: TermEvent::Mouse(mouse),
        ..
      } => return self.handle_mouse(mouse),
      TaskScreenCmd::Input {
        event: TermEvent::Paste(text),
        ..
      } => {
        if self.modal.is_none()
          && self.state.scope.is_term()
          && let Some(task) = self.state.current_task()
        {
          self.input_current(Some(task.id), TermEvent::Paste(text));
        }
        return;
      }
      cmd @ (TaskScreenCmd::Attach { .. }
      | TaskScreenCmd::Detach { .. }
      | TaskScreenCmd::Input { .. }) => cmd,
      // Copy mode and scrolling do not apply to a composed UI.
      TaskScreenCmd::CopyEnter
      | TaskScreenCmd::CopyLeave
      | TaskScreenCmd::CopyMove { .. }
      | TaskScreenCmd::CopyBeginSelection
      | TaskScreenCmd::Scroll { .. }
      | TaskScreenCmd::CopyYank => return,
    };
    let attached = self.screen.has_observers();
    self.screen.handle_cmd(cmd, &mut self.effects);
    self.apply_screen(attached);
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

  fn handle_mouse(&mut self, mouse: crate::term::mouse::MouseEvent) {
    if self.modal.is_some() {
      return;
    }
    let layout = self.layout();
    let (x, y) = (mouse.x as u16, mouse.y as u16);
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
      let selected = self.state.selected();
      match mouse.kind {
        MouseEventKind::Down(MouseButton::Left) => {
          if let Some(index) = task_at(layout.sidebar, x, y, &self.state) {
            self.state.select(index);
          }
        }
        MouseEventKind::ScrollDown => {
          self.state.select(selected + 1);
        }
        MouseEventKind::ScrollUp => {
          self.state.select(selected.saturating_sub(1));
        }
        MouseEventKind::Down(MouseButton::Right | MouseButton::Middle)
        | MouseEventKind::Up(_)
        | MouseEventKind::Drag(_)
        | MouseEventKind::Moved
        | MouseEventKind::ScrollLeft
        | MouseEventKind::ScrollRight => (),
      }
    }
  }

  fn handle_key(&mut self, observer: ObserverId, key: Key) {
    match key.kind {
      KeyEventKind::Press | KeyEventKind::Repeat => (),
      KeyEventKind::Release => return,
    }
    if let Some(modal) = &mut self.modal {
      match modal.handle_key(&key) {
        ModalResult::Keep => (),
        ModalResult::Close => self.modal = None,
        ModalResult::Run(action) => {
          self.modal = None;
          self.handle_action(Some(observer), action);
        }
        ModalResult::Detach => {
          self.modal = None;
          self.handle_screen_cmd(TaskScreenCmd::Detach { observer });
        }
      }
      return;
    }
    let key = Key::new(key.code, key.mods);
    let group = self.state.keymap_group();
    if let Some(action) = self.keymap.action(group, &key) {
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

      Action::QuitOrAsk => self.modal = Some(Box::new(QuitModal)),
      Action::Quit => match observer {
        Some(observer) => {
          self.handle_screen_cmd(TaskScreenCmd::Detach { observer });
        }
        None => {
          self.state.quitting = true;
          self.issue(Command::Quit);
        }
      },
      Action::ForceQuit => {
        self.state.quitting = true;
        self.issue(Command::Batch {
          commands: vec![
            Command::Kill {
              target: Target::glob("**"),
            },
            Command::Quit,
          ],
        });
      }
      Action::Command { command } => self.issue(command),

      Action::ToggleFocus => self.state.scope = self.state.scope.toggle(),
      Action::FocusTasks => self.state.scope = Scope::Tasks,
      Action::FocusTerm => self.state.scope = Scope::Term,
      Action::Zoom => self.state.scope = Scope::TermZoom,

      Action::ShowCommandsMenu => {
        self.modal = Some(Box::new(CommandsMenuModal::new()));
      }
      Action::CloseCurrentModal => self.modal = None,
      Action::ToggleKeymapWindow => {
        self.state.hide_keymap_window = !self.state.hide_keymap_window;
      }

      Action::NextTask => {
        let count = self.state.tasks.len();
        if count > 0 {
          self.state.select((self.state.selected() + 1) % count);
        }
      }
      Action::PrevTask => {
        let count = self.state.tasks.len();
        if count > 0 {
          self
            .state
            .select((self.state.selected() + count - 1) % count);
        }
      }
      Action::SelectTask { index } => self.state.select(index),

      Action::StartTask => {
        self.issue_current(current, |target| Command::Start { target })
      }
      Action::StopTask => {
        self.issue_current(current, |target| Command::Stop { target })
      }
      Action::KillTask => {
        self.issue_current(current, |target| Command::Kill { target })
      }
      Action::VetoTask => {
        self.issue_current(current, |target| Command::Veto { target })
      }
      Action::RestartTask => {
        self.issue_current(current, |target| Command::Restart { target })
      }
      Action::ForceRestartTask => {
        self.issue_current(current, |target| Command::ForceRestart { target })
      }
      Action::RestartAll => {
        self.issue_all(|target| Command::Restart { target })
      }
      Action::ForceRestartAll => {
        self.issue_all(|target| Command::ForceRestart { target })
      }

      Action::ShowAddTask => {
        self.modal = Some(Box::new(AddTaskModal::default()))
      }
      Action::AddTask { cmd, name } => {
        let label =
          self.unique_label(&name.unwrap_or_else(|| cmd.clone()), None);
        let path = unique(&path_name(&label), |path| {
          self
            .state
            .tasks
            .iter()
            .any(|t| t.path.as_ref().is_some_and(|p| p.as_str() == path))
        });
        match path.parse::<Target>() {
          Ok(target) => self.issue(Command::Add {
            target,
            label: Some(label),
            cmd: CmdConfig::Shell { shell: cmd },
            cwd: None,
            env: None,
            deps: Vec::new(),
            tags: Vec::new(),
          }),
          Err(err) => log::warn!("Cannot add task '{path}': {err}"),
        }
      }
      Action::DuplicateTask => {
        if let Some(task) = self.state.current_task() {
          let name = self.unique_label(&task.name(), None);
          self.issue(Command::Duplicate {
            target: Target::Id(task.id),
            name: Some(name),
          });
        }
      }
      Action::ShowRenameTask => {
        self.modal = Some(Box::new(RenameTaskModal::default()));
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
      Action::ShowRemoveTask => {
        if let Some(task) = self.state.current_task()
          && !task.is_up()
        {
          self.modal = Some(Box::new(RemoveTaskModal { id: task.id }));
        }
      }

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

  fn issue(&self, command: Command) {
    issue(&self.pc, &self.config, command);
  }

  fn issue_current(
    &self,
    current: Option<TaskId>,
    make: fn(Target) -> Command,
  ) {
    if let Some(id) = current {
      self.issue(make(Target::Id(id)));
    }
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
      ScreenNotify::Attached | ScreenNotify::Render | ScreenNotify::Bell => (),
      ScreenNotify::CopyPresent { screen, vt } => {
        if let Some(task) = self.state.task_mut(screen) {
          task.present = vt;
        }
      }
      // Copied on behalf of whoever is attached here.
      ScreenNotify::Yank { text } => {
        let attached = self.screen.has_observers();
        self.screen.yank(text, &mut self.effects);
        self.apply_screen(attached);
      }
    }
  }

  fn handle_task_notify(&mut self, id: TaskId, notify: TaskNotify) {
    match notify {
      TaskNotify::Added {
        path,
        label,
        state,
        vt,
      } => self.add_task(id, label, path, state, vt),
      TaskNotify::StateChanged(state) => {
        if let Some(task) = self.state.task_mut(id) {
          task.status = state;
        }
      }
      TaskNotify::Removed => {
        let index = self.state.tasks.iter().position(|t| t.id == id);
        self.state.tasks.retain(|t| t.id != id);
        let selected = self.state.selected();
        match index {
          Some(index) if index < selected => self.state.select(selected - 1),
          _ => self.state.select(selected),
        }
      }
      TaskNotify::LabelChanged(label) => {
        if let Some(task) = self.state.task_mut(id) {
          task.label = label;
        }
      }
    }
  }
}
