use std::collections::{HashMap, VecDeque};

use anyhow::bail;
use tokio::sync::mpsc::UnboundedReceiver;

use crate::{
  config::{
    config::Config,
    task::{AUTOSTART_TAG, CmdConfig, TaskConfig},
    task_log::LogMode,
  },
  console::{
    action::{Action, CopyMove, ScrollUnit},
    app_client::ClientHandle,
    app_layout::AppLayout,
    keymap::Keymap,
    modal::{
      add_task::AddTaskModal, commands_menu::CommandsMenuModal, modal::Modal,
      quit::QuitModal, remove_task::RemoveTaskModal,
      rename_task::RenameTaskModal,
    },
    state::{Scope, State},
    task::view::TaskView,
    ui_keymap::render_keymap,
    ui_tasks::{render_tasks, tasks_check_hit, tasks_get_clicked_index},
    ui_term::{render_term, term_check_hit},
    ui_zoom_tip::render_zoom_tip,
    widgets::list::ListState,
  },
};
use crate::{
  console::server_message::{ClientId, ServerMessage},
  error::ResultLogger,
  kernel::{
    copy_mode::CopyMove as KernelCopyMove,
    kernel_message::{
      KernelCommand, KernelQuery, KernelQueryResponse, TaskContext,
      TaskSelector,
    },
    sub_trie::SubMode,
    task::{
      RestartMode, TaskCmd, TaskDef, TaskId, TaskNotification, TaskNotify,
    },
    task_path::TaskPath,
    task_screen::{
      FramedScreenNotify, ScrollUnit as KernelScrollUnit, TaskScreenCmd,
    },
  },
  process::process_spec::ProcessSpec,
  protocol::{Bye, CtlMsg, codes},
  task::{
    logger::{LogResolver, LogSink},
    process_task::{
      DuplicateTask, ProcessInput, ProcessTaskConfig,
      spawn_process_task_with_id,
    },
  },
  term::{
    Grid, Size, TermEvent, Winsize,
    attrs::Attrs,
    grid::Rect,
    key::{Key, KeyEventKind},
    mouse::{MouseButton, MouseEventKind},
  },
};

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

#[derive(Debug, Default, PartialEq)]
pub enum LoopAction {
  Render,
  #[default]
  Skip,
  ForceQuit,
}

impl LoopAction {
  pub fn render(&mut self) {
    match self {
      LoopAction::Render => (),
      LoopAction::Skip => *self = LoopAction::Render,
      LoopAction::ForceQuit => (),
    }
  }

  fn force_quit(&mut self) {
    *self = LoopAction::ForceQuit;
  }
}

pub struct App {
  config: Config,
  keymap: Keymap,
  state: State,
  grid: Grid,
  modal: Option<Box<dyn Modal>>,
  pr: tokio::sync::mpsc::UnboundedReceiver<TaskCmd>,
  pc: TaskContext,

  screen_size: Size,
  clients: Vec<ClientHandle>,
}

impl App {
  pub async fn run(self) -> anyhow::Result<()> {
    let result = self.main_loop().await;
    if let Err(err) = result {
      log::error!("App main loop error: {err}");
    }

    Ok(())
  }

  async fn main_loop(mut self) -> anyhow::Result<()> {
    self.pc.subscribe_path(TaskPath::root(), SubMode::Subtree);
    self.refresh_tasks().await;

    self.start_tasks()?;

    let mut render_needed = true;
    let mut last_term_size = self.get_layout().term_area().size();

    let mut command_buf = Vec::new();

    loop {
      let layout = self.get_layout();

      let term_size = layout.term_area().size();
      if term_size != last_term_size {
        let observer_id = self.pc.task_id;
        for task_handle in &mut self.state.tasks {
          self.pc.send_msg(
            task_handle.id(),
            TaskScreenCmd::Resize {
              size: Winsize {
                x: term_size.width,
                y: term_size.height,
                x_px: 0,
                y_px: 0,
              },
              observer_id,
            },
          );
        }

        last_term_size = term_size;
      }

      if render_needed && self.clients.len() > 0 {
        let grid = &mut self.grid;
        grid.erase_all(Attrs::default());
        grid.cursor_pos = None;
        grid.cursor_style = crate::term::CursorStyle::Default;

        let state = &mut self.state;
        let config = &mut self.config;
        let keymap = &self.keymap;
        render_tasks(layout.sidebar.into(), grid, state, config);
        render_term(layout.term, grid, state);
        render_keymap(layout.keymap.into(), grid, state, keymap);
        render_zoom_tip(layout.zoom_banner.into(), grid, keymap);

        if let Some(modal) = &mut self.modal {
          grid.cursor_style = crate::term::CursorStyle::Default;
          modal.render(grid);
        }

        for client_handle in &mut self.clients {
          let mut out = String::new();
          client_handle.differ.diff(&mut out, grid).log_ignore();
          client_handle
            .sender
            .send_out(out.into_bytes().into())
            .await
            .log_ignore();
        }
      }

      let mut loop_action = LoopAction::default();
      self.pr.recv_many(&mut command_buf, 512).await;
      for command in command_buf.drain(..) {
        self.handle_task_command(&mut loop_action, command);
      }

      if self.state.quitting && self.state.all_tasks_down() {
        break;
      }

      match loop_action {
        LoopAction::Render => {
          render_needed = true;
        }
        LoopAction::Skip => {
          render_needed = false;
        }
        LoopAction::ForceQuit => break,
      };
    }

    for mut client in self.clients.into_iter() {
      client
        .sender
        .send_ctl(CtlMsg::Bye(Bye {
          code: codes::QUIT.to_string(),
          message: String::new(),
        }))
        .await
        .log_ignore();
    }

    for task in &self.state.tasks {
      self.pc.send_msg(
        task.id(),
        TaskScreenCmd::Unobserve {
          observer_id: self.pc.task_id,
        },
      );
    }
    self.pc.unsubscribe_path(TaskPath::root(), SubMode::Subtree);

    Ok(())
  }

  fn observe_task(&self, task_id: TaskId, size: Rect) {
    let sender = self.pc.get_task_sender(self.pc.task_id);
    self.pc.send_msg(
      task_id,
      TaskScreenCmd::Observe {
        size: Winsize {
          x: size.width,
          y: size.height,
          x_px: 0,
          y_px: 0,
        },
        sender,
      },
    );
  }

  async fn refresh_tasks(&mut self) {
    let resp = self.pc.query(KernelQuery::ListTasks(None)).await;
    let Ok(KernelQueryResponse::TaskList(list)) = resp else {
      return;
    };
    let size = self.get_layout().term_area();
    for task in list {
      let Some(vt) = task.vt else {
        continue;
      };
      if self.state.tasks.iter().any(|p| p.id() == task.id) {
        continue;
      }
      let name = task_display_name(task.label, task.path.as_ref(), task.id);
      self
        .state
        .tasks
        .push(TaskView::new(task.id, name, task.state, vt));
      self.observe_task(task.id, size);
    }
  }

  fn start_tasks(&mut self) -> anyhow::Result<()> {
    let task_ids: Vec<TaskId> = self
      .config
      .tasks
      .iter()
      .map(|_| self.pc.alloc_id())
      .collect();
    let deps_by_task = resolve_task_deps(&self.config.tasks, &task_ids)?;

    // Deps must be registered first (the kernel refuses a registration
    // with a missing dep), so register in dependency order.
    let order = dep_order(&task_ids, &deps_by_task)?;
    for i in order {
      let cfg = self.config.tasks[i].clone();
      let pinned = cfg.autostart();
      self.spawn_task(cfg, task_ids[i], deps_by_task[i].clone(), pinned);
    }

    Ok(())
  }

  /// `pinned` makes "registered and started" one kernel step, so a
  /// refused registration starts nothing.
  fn spawn_task(
    &self,
    cfg: TaskConfig,
    task_id: TaskId,
    deps: Vec<TaskId>,
    pinned: bool,
  ) {
    let merged = self.config.defaults.clone().overlay(cfg);
    // Legacy mprocs task names are arbitrary strings, so fall back to the
    // task id when the name is not a valid path. Dekit configs validate
    // paths at parse time, so the fallback never fires for them.
    let path = TaskPath::new(&merged.path)
      .or_else(|_| TaskPath::new(task_id.0.to_string()))
      .ok();
    let _ = spawn_process_task_with_id(
      &self.pc,
      task_id,
      path,
      process_task_config(&merged, task_id, deps, pinned),
    );
  }

  fn unique_task_name(&self, base: &str, exclude: Option<TaskId>) -> String {
    let taken = |name: &str| {
      self
        .state
        .tasks
        .iter()
        .any(|p| Some(p.id()) != exclude && p.name() == name)
    };
    if !taken(base) {
      return base.to_string();
    }
    (2..)
      .map(|n| format!("{}-{}", base, n))
      .find(|name| !taken(name))
      .unwrap()
  }

  fn handle_server_message(
    &mut self,
    loop_action: &mut LoopAction,
    msg: ServerMessage,
  ) -> anyhow::Result<()> {
    match msg {
      ServerMessage::ClientInput { client_id, event } => {
        self.state.current_client_id = Some(client_id);
        self.handle_input(loop_action, client_id, event);
        self.state.current_client_id = None;
      }
      ServerMessage::ClientConnected { handle } => {
        self.clients.push(handle);
        self.update_screen_size();
        loop_action.render();
      }
      ServerMessage::ClientDisconnected { client_id } => {
        self.clients.retain(|c| c.id != client_id);
        self.update_screen_size();
        loop_action.render();
      }
    }
    Ok(())
  }

  fn update_screen_size(&mut self) {
    if let Some(client) = self.clients.first_mut() {
      self.screen_size = client.size();
      self.grid.set_size(client.size());
    }
  }

  fn handle_input(
    &mut self,
    loop_action: &mut LoopAction,
    client_id: ClientId,
    event: TermEvent,
  ) {
    if let TermEvent::Key(Key {
      kind: KeyEventKind::Release,
      ..
    }) = event
    {
      return;
    }

    if let Some(modal) = &mut self.modal {
      let handled = modal.handle_input(&mut self.state, loop_action, &event);
      if handled {
        return;
      }
    }

    match event {
      TermEvent::Key(Key {
        code,
        mods,
        kind: KeyEventKind::Press | KeyEventKind::Repeat,
        state: _,
      }) => {
        let key = Key::new(code, mods);
        let group = self.state.get_keymap_group();
        if let Some(bound) = self.keymap.resolve(group, &key) {
          let bound = bound.clone();
          self.handle_event(loop_action, &bound)
        } else {
          match self.state.scope {
            Scope::Tasks => (),
            Scope::Term | Scope::TermZoom => {
              self.handle_event(loop_action, &Action::SendKey { key })
            }
          }
        }
      }
      TermEvent::Key(Key {
        kind: KeyEventKind::Release,
        ..
      }) => (),
      TermEvent::Mouse(mouse_event) => {
        let layout = self.get_layout();
        if term_check_hit(
          layout.term_area(),
          mouse_event.x as u16,
          mouse_event.y as u16,
        ) {
          if let (Scope::Tasks, MouseEventKind::Down(_)) =
            (self.state.scope, mouse_event.kind)
          {
            self.state.scope = Scope::Term
          }
          if let Some(task) = self.state.get_current_task() {
            let local_event = mouse_event.translate(layout.term_area());
            self
              .pc
              .send_msg(task.id, TaskScreenCmd::Mouse { event: local_event });
          }
        } else if tasks_check_hit(
          layout.sidebar.into(),
          mouse_event.x as u16,
          mouse_event.y as u16,
        ) {
          if let (Scope::Term, MouseEventKind::Down(_)) =
            (self.state.scope, mouse_event.kind)
          {
            self.state.scope = Scope::Tasks
          }
          match mouse_event.kind {
            MouseEventKind::Down(btn) => match btn {
              MouseButton::Left => {
                if let Some(index) = tasks_get_clicked_index(
                  layout.sidebar.into(),
                  mouse_event.x as u16,
                  mouse_event.y as u16,
                  &self.state,
                ) {
                  self.state.select_task(index);
                }
              }
              MouseButton::Right | MouseButton::Middle => (),
            },
            MouseEventKind::Up(_) => (),
            MouseEventKind::Drag(_) => (),
            MouseEventKind::Moved => (),
            MouseEventKind::ScrollDown => {
              if self.state.selected()
                < self.state.tasks.len().saturating_sub(1)
              {
                let index = self.state.selected() + 1;
                self.state.select_task(index);
              }
            }
            MouseEventKind::ScrollUp => {
              if self.state.selected() > 0 {
                let index = self.state.selected() - 1;
                self.state.select_task(index);
              }
            }
            MouseEventKind::ScrollLeft => (),
            MouseEventKind::ScrollRight => (),
          }
        }
        loop_action.render();
      }
      TermEvent::Resize(width, height) => {
        if let Some(client) =
          self.clients.iter_mut().find(|c| c.id == client_id)
        {
          let size = Size { width, height };
          client.resize(size);
        }
        self.update_screen_size();

        loop_action.render();
      }
      TermEvent::FocusGained => {
        log::debug!("Ignore input event: {:?}", event);
      }
      TermEvent::FocusLost => {
        log::debug!("Ignore input event: {:?}", event);
      }
      TermEvent::Paste(_) => {
        log::debug!("Ignore input event: {:?}", event);
      }
    }
  }

  fn scroll(
    &self,
    loop_action: &mut LoopAction,
    delta: i32,
    unit: KernelScrollUnit,
  ) {
    if let Some(task) = self.state.get_current_task() {
      self
        .pc
        .send_msg(task.id, TaskScreenCmd::Scroll { delta, unit });
      loop_action.render();
    }
  }

  fn handle_event(&mut self, loop_action: &mut LoopAction, event: &Action) {
    let pc = self.pc.clone();
    match event {
      Action::Batch { cmds } => {
        for cmd in cmds {
          self.handle_event(loop_action, cmd);
          if *loop_action == LoopAction::ForceQuit {
            return;
          }
        }
      }

      Action::QuitOrAsk => {
        self.modal = Some(QuitModal::new(self.pc.clone()).boxed());
        loop_action.render();
      }
      Action::Quit => {
        self.state.quitting = true;
        pc.send(KernelCommand::Quit);
        loop_action.render();
      }
      Action::ForceQuit => {
        pc.send(KernelCommand::Quit);
        for task in self.state.tasks.iter() {
          if task.is_up() {
            pc.send(KernelCommand::Kill(TaskSelector::Id(task.id()), None));
          }
        }
        loop_action.force_quit();
      }
      Action::Detach { client_id } => {
        self.clients.retain_mut(|c| c.id != *client_id);
        self.update_screen_size();
        loop_action.render();
      }

      Action::ToggleFocus => {
        self.state.scope = self.state.scope.toggle();
        loop_action.render();
      }
      Action::FocusTasks => {
        self.state.scope = Scope::Tasks;
        loop_action.render();
      }
      Action::FocusTerm => {
        self.state.scope = Scope::Term;
        loop_action.render();
      }
      Action::Zoom => {
        self.state.scope = Scope::TermZoom;
        loop_action.render();
      }

      Action::ShowCommandsMenu => {
        self.modal =
          Some(CommandsMenuModal::new(self.pc.clone(), &self.keymap).boxed());
        loop_action.render();
      }
      Action::NextTask => {
        let mut next = self.state.selected() + 1;
        if next >= self.state.tasks.len() {
          next = 0;
        }
        self.state.select_task(next);
        loop_action.render();
      }
      Action::PrevTask => {
        let next = if self.state.selected() > 0 {
          self.state.selected() - 1
        } else {
          self.state.tasks.len().saturating_sub(1)
        };
        self.state.select_task(next);
        loop_action.render();
      }
      Action::SelectTask { index } => {
        self.state.select_task(*index);
        loop_action.render();
      }

      Action::StartTask => {
        if let Some(task) = self.state.get_current_task() {
          pc.send(KernelCommand::Start(TaskSelector::Id(task.id), None));
        }
      }
      Action::StopTask => {
        if let Some(task) = self.state.get_current_task() {
          pc.send(KernelCommand::Stop(TaskSelector::Id(task.id), None));
        }
      }
      Action::KillTask => {
        if let Some(task) = self.state.get_current_task() {
          pc.send(KernelCommand::Kill(TaskSelector::Id(task.id), None));
        }
      }
      Action::VetoTask => {
        if let Some(task) = self.state.get_current_task() {
          pc.send(KernelCommand::Veto(TaskSelector::Id(task.id), None));
        }
      }
      Action::RestartTask => {
        if let Some(task) = self.state.get_current_task() {
          pc.send(KernelCommand::Restart(TaskSelector::Id(task.id), None));
        }
      }
      Action::RestartAll => {
        for task in &self.state.tasks {
          pc.send(KernelCommand::Restart(TaskSelector::Id(task.id), None));
        }
      }
      Action::ForceRestartTask => {
        if let Some(task) = self.state.get_current_task() {
          pc.send(KernelCommand::Kill(TaskSelector::Id(task.id), None));
          pc.send(KernelCommand::Start(TaskSelector::Id(task.id), None));
        }
      }
      Action::ForceRestartAll => {
        for task in &self.state.tasks {
          pc.send(KernelCommand::Kill(TaskSelector::Id(task.id), None));
          pc.send(KernelCommand::Start(TaskSelector::Id(task.id), None));
        }
      }

      Action::ScrollUp { n, unit } => {
        let n = (*n).min(i32::MAX as usize) as i32;
        self.scroll(loop_action, n, kernel_scroll_unit(*unit));
      }
      Action::ScrollDown { n, unit } => {
        let n = (*n).min(i32::MAX as usize) as i32;
        self.scroll(loop_action, -n, kernel_scroll_unit(*unit));
      }
      Action::ShowAddTask => {
        self.modal = Some(AddTaskModal::new(self.pc.clone()).boxed());
        loop_action.render();
      }
      Action::AddTask { cmd, name } => {
        let name = name.clone().unwrap_or_else(|| cmd.clone());
        let task_config = TaskConfig {
          path: self.unique_task_name(&name, None),
          cmd: Some(CmdConfig::Shell {
            shell: cmd.to_string(),
          }),
          ..TaskConfig::default()
        };
        let id = self.pc.alloc_id();
        self.spawn_task(task_config, id, Vec::new(), true);
        loop_action.render();
      }
      Action::DuplicateTask => {
        if let Some(task) = self.state.get_current_task() {
          let name = self.unique_task_name(task.name(), None);
          pc.send_msg(task.id(), DuplicateTask(Some(name)));
          loop_action.render();
        }
      }
      Action::ShowRemoveTask => {
        let id = match self.state.get_current_task() {
          Some(task) if !task.is_up() => Some(task.id()),
          _ => None,
        };
        if let Some(id) = id {
          self.modal = Some(RemoveTaskModal::new(id, self.pc.clone()).boxed());
          loop_action.render();
        }
      }
      Action::RemoveTask { id } => {
        self.pc.send(KernelCommand::RemoveTask(*id));
        loop_action.render();
      }

      Action::CloseCurrentModal => {
        self.modal = None;
        loop_action.render();
      }

      Action::ShowRenameTask => {
        self.modal = Some(RenameTaskModal::new(self.pc.clone()).boxed());
        loop_action.render();
      }
      Action::RenameTask { name } => {
        if let Some(task) = self.state.get_current_task() {
          let id = task.id();
          let name = self.unique_task_name(name, Some(id));
          self.pc.set_task_label(id, Some(name));
          loop_action.render();
        }
      }

      Action::CopyModeEnter => {
        if let Some(task) = self.state.get_current_task() {
          pc.send_msg(task.id, TaskScreenCmd::CopyEnter);
          self.state.scope = Scope::Term;
          loop_action.render();
        };
      }
      Action::CopyModeLeave => {
        if let Some(task) = self.state.get_current_task() {
          pc.send_msg(task.id, TaskScreenCmd::CopyLeave);
        }
      }
      Action::CopyModeMove { dir } => {
        if let Some(task) = self.state.get_current_task() {
          pc.send_msg(
            task.id,
            TaskScreenCmd::CopyMove {
              dir: kernel_copy_move(*dir),
            },
          );
        }
      }
      Action::CopyModeEnd => {
        if let Some(task) = self.state.get_current_task() {
          pc.send_msg(task.id, TaskScreenCmd::CopyBeginSelection);
        }
      }
      Action::CopyModeCopy => {
        if let Some(task) = self.state.get_current_task() {
          pc.send_msg(task.id, TaskScreenCmd::CopyYank);
        }
      }

      Action::ToggleKeymapWindow => {
        self.state.toggle_keymap_window();
        loop_action.render();
      }

      Action::SendKey { key } => {
        if let Some(task) = self.state.get_current_task() {
          pc.send_msg(task.id, ProcessInput(*key));
        }
      }
    }
  }

  fn handle_task_command(
    &mut self,
    loop_action: &mut LoopAction,
    command: TaskCmd,
  ) {
    match command {
      TaskCmd::Start | TaskCmd::Stop | TaskCmd::Kill => (),

      TaskCmd::Msg(msg) => {
        let msg = match msg.downcast::<Action>() {
          Ok(app_event) => {
            self.handle_event(loop_action, &app_event);
            return;
          }
          Err(msg) => msg,
        };
        let msg = match msg.downcast::<ServerMessage>() {
          Ok(server_msg) => {
            let r = self.handle_server_message(loop_action, *server_msg);
            if let Err(err) = r {
              log::debug!("ServerMessage error: {:?}", err);
            }
            return;
          }
          Err(msg) => msg,
        };
        let msg = match msg.downcast::<FramedScreenNotify>() {
          Ok(notify) => {
            self.handle_screen_notify(loop_action, *notify);
            return;
          }
          Err(msg) => msg,
        };
        if let Ok(n) = msg.downcast::<TaskNotification>() {
          self.handle_notification(loop_action, n.from, n.notify);
          return;
        }
        log::error!("App received unknown Msg");
      }
    }
  }

  fn handle_screen_notify(
    &mut self,
    loop_action: &mut LoopAction,
    notify: FramedScreenNotify,
  ) {
    match notify {
      FramedScreenNotify::ObserveStarted { task_id } => {
        let is_current = self
          .state
          .get_current_task()
          .is_some_and(|p| p.id() == task_id);
        if is_current {
          loop_action.render();
        }
      }
      FramedScreenNotify::Render { task_id } => {
        let is_current = self
          .state
          .get_current_task()
          .is_some_and(|p| p.id() == task_id);
        if let Some(task) = self.state.get_task_mut(task_id) {
          if !is_current {
            task.changed = true;
          }
          loop_action.render();
        }
      }
      FramedScreenNotify::Bell { .. } => (),
      FramedScreenNotify::CopyPresent { task_id, vt } => {
        if let Some(task) = self.state.get_task_mut(task_id) {
          task.present = vt;
          loop_action.render();
        }
      }
      FramedScreenNotify::Yank { text } => {
        crate::clipboard::copy(text.as_str());
      }
    }
  }

  fn handle_notification(
    &mut self,
    loop_action: &mut LoopAction,
    task_id: TaskId,
    notify: TaskNotify,
  ) {
    match notify {
      TaskNotify::Added {
        path,
        label,
        state,
        vt,
      } => {
        let Some(vt) = vt else {
          return;
        };
        if self.state.tasks.iter().any(|p| p.id() == task_id) {
          return;
        }
        let name = task_display_name(label, path.as_ref(), task_id);
        self
          .state
          .tasks
          .push(TaskView::new(task_id, name, state, vt));
        let size = self.get_layout().term_area();
        self.observe_task(task_id, size);
        loop_action.render();
      }
      TaskNotify::StateChanged(state) => {
        let known = if let Some(task) = self.state.get_task_mut(task_id) {
          task.status = state;
          true
        } else {
          false
        };
        if known {
          if !state.is_active() && self.state.all_tasks_down() {
            if let Some(hook) = &self.config.on_all_finished {
              let event = hook.as_action().clone();
              self.handle_event(loop_action, &event);
            }
          }
          loop_action.render();
        }
      }
      TaskNotify::Removed => {
        self.state.tasks.retain(|p| p.id() != task_id);
        loop_action.render();
      }
      TaskNotify::PathChanged(_, new) => {
        if let Some(new) = new
          && let Some(task) = self.state.get_task_mut(task_id)
        {
          task.set_name(new.name().to_string());
        }
      }
      TaskNotify::LabelChanged(label) => {
        if let Some(task) = self.state.get_task_mut(task_id) {
          task.set_name(task_display_name(label, None, task_id));
          loop_action.render();
        }
      }
    }
  }

  fn get_layout(&mut self) -> AppLayout {
    let size = self.screen_size;
    AppLayout::new(
      Rect::new(0, 0, size.width, size.height),
      self.state.scope.is_zoomed(),
      self.state.hide_keymap_window,
      &self.config,
    )
  }
}

fn task_display_name(
  label: Option<String>,
  path: Option<&TaskPath>,
  id: TaskId,
) -> String {
  label
    .or_else(|| path.map(|p| p.name().to_string()))
    .unwrap_or_else(|| format!("task-{}", id.0))
}

fn process_task_config(
  cfg: &TaskConfig,
  task_id: TaskId,
  deps: Vec<TaskId>,
  pinned: bool,
) -> ProcessTaskConfig {
  let log = cfg.log.clone().map(|log_cfg| {
    let name = cfg.path.clone();
    let id = task_id.0;
    Box::new(move |pid: u32| {
      log_cfg.file_path(&name, id, pid).map(|path| LogSink {
        path,
        append: log_cfg.mode() == LogMode::Append,
      })
    }) as LogResolver
  });
  ProcessTaskConfig {
    spec: ProcessSpec::from(cfg),
    stop: cfg.stop(),
    log,
    restart: if cfg.autorestart() {
      RestartMode::OnFailure
    } else {
      RestartMode::Never
    },
    ready_log: cfg.ready_log.clone(),
    scrollback_len: cfg.scrollback_len(),
    mouse_scroll_speed: cfg.mouse_scroll_speed(),
    deps,
    label: Some(cfg.path.clone()),
    tags: {
      let mut tags = cfg.tags.clone();
      if cfg.autostart() {
        tags.push(AUTOSTART_TAG.to_string());
      }
      tags
    },
    pinned,
  }
}

fn resolve_task_deps(
  task_configs: &[TaskConfig],
  task_ids: &[TaskId],
) -> anyhow::Result<Vec<Vec<TaskId>>> {
  if task_configs.len() != task_ids.len() {
    bail!("Internal error: task and task id counts differ.");
  }

  let mut name_to_id = HashMap::new();
  let mut name_to_index = HashMap::new();
  for (index, (task_config, task_id)) in
    task_configs.iter().zip(task_ids.iter()).enumerate()
  {
    if name_to_id
      .insert(task_config.path.as_str(), *task_id)
      .is_some()
    {
      bail!("Duplicate task name '{}'.", task_config.path);
    }
    name_to_index.insert(task_config.path.as_str(), index);
  }

  let mut deps_by_task = Vec::with_capacity(task_configs.len());
  let mut dep_indexes_by_task = Vec::with_capacity(task_configs.len());
  for task_config in task_configs {
    let mut deps = Vec::with_capacity(task_config.deps.len());
    let mut dep_indexes = Vec::with_capacity(task_config.deps.len());
    for dep_name in &task_config.deps {
      let Some(dep_id) = name_to_id.get(dep_name.as_str()) else {
        bail!(
          "Process '{}' depends on unknown process '{}'.",
          task_config.path,
          dep_name
        );
      };
      let Some(dep_index) = name_to_index.get(dep_name.as_str()) else {
        bail!(
          "Process '{}' depends on unknown process '{}'.",
          task_config.path,
          dep_name
        );
      };
      deps.push(*dep_id);
      dep_indexes.push(*dep_index);
    }
    deps_by_task.push(deps);
    dep_indexes_by_task.push(dep_indexes);
  }

  validate_task_dep_cycles(task_configs, &dep_indexes_by_task)?;

  Ok(deps_by_task)
}

/// Order task indices so every dep comes before its dependent. Deps are
/// already validated acyclic (`validate_task_dep_cycles`).
fn dep_order(
  task_ids: &[TaskId],
  deps_by_task: &[Vec<TaskId>],
) -> anyhow::Result<Vec<usize>> {
  let index_of: HashMap<TaskId, usize> = task_ids
    .iter()
    .enumerate()
    .map(|(i, id)| (*id, i))
    .collect();
  let n = task_ids.len();
  let mut missing_deps = vec![0usize; n];
  let mut dependents: Vec<Vec<usize>> = vec![Vec::new(); n];
  for (i, deps) in deps_by_task.iter().enumerate() {
    missing_deps[i] = deps.len();
    for dep in deps {
      dependents[index_of[dep]].push(i);
    }
  }
  let mut queue: VecDeque<usize> =
    (0..n).filter(|i| missing_deps[*i] == 0).collect();
  let mut order = Vec::with_capacity(n);
  while let Some(i) = queue.pop_front() {
    order.push(i);
    for &k in &dependents[i] {
      missing_deps[k] -= 1;
      if missing_deps[k] == 0 {
        queue.push_back(k);
      }
    }
  }
  if order.len() != n {
    bail!("Dependency cycle among config tasks.");
  }
  Ok(order)
}

#[derive(Clone, Copy, PartialEq)]
enum VisitState {
  Unvisited,
  Visiting,
  Visited,
}

fn validate_task_dep_cycles(
  task_configs: &[TaskConfig],
  deps_by_task: &[Vec<usize>],
) -> anyhow::Result<()> {
  let mut states = vec![VisitState::Unvisited; task_configs.len()];
  let mut stack = Vec::new();

  for index in 0..task_configs.len() {
    visit_task_deps(
      index,
      task_configs,
      deps_by_task,
      &mut states,
      &mut stack,
    )?;
  }

  Ok(())
}

fn visit_task_deps(
  index: usize,
  task_configs: &[TaskConfig],
  deps_by_task: &[Vec<usize>],
  states: &mut [VisitState],
  stack: &mut Vec<usize>,
) -> anyhow::Result<()> {
  match states[index] {
    VisitState::Visited => return Ok(()),
    VisitState::Visiting => {
      let cycle_start = stack.iter().position(|&i| i == index).unwrap_or(0);
      let mut cycle = stack[cycle_start..]
        .iter()
        .map(|&i| task_configs[i].path.as_str())
        .collect::<Vec<_>>();
      cycle.push(task_configs[index].path.as_str());
      bail!("Process dependency cycle detected: {}.", cycle.join(" -> "));
    }
    VisitState::Unvisited => {}
  }

  states[index] = VisitState::Visiting;
  stack.push(index);
  for dep_index in &deps_by_task[index] {
    visit_task_deps(*dep_index, task_configs, deps_by_task, states, stack)?;
  }
  stack.pop();
  states[index] = VisitState::Visited;

  Ok(())
}

pub fn create_app_task(
  config: Config,
  keymap: Keymap,
  pc: &TaskContext,
) -> TaskId {
  pc.spawn_async(TaskDef::default(), |pc, receiver| async move {
    log::debug!("Creating app task (id: {})", pc.task_id.0);
    let r = server_main(config, keymap, receiver, pc.clone()).await;
    match r {
      Ok(()) => (),
      Err(err) => log::error!("App task finished with error: {:?}", err),
    };
    pc.send(KernelCommand::Quit);
  })
}

pub async fn server_main(
  config: Config,
  keymap: Keymap,
  pr: UnboundedReceiver<TaskCmd>,
  pc: TaskContext,
) -> anyhow::Result<()> {
  let state = State {
    current_client_id: None,

    scope: Scope::Tasks,
    tasks: Vec::new(),
    tasks_list: ListState::default(),
    hide_keymap_window: !config.tui.tips.show,

    quitting: false,
  };

  let size = Size {
    width: 160,
    height: 50,
  };
  let scrollback_len = config.defaults.scrollback_len();

  let app = App {
    config,
    keymap,
    state,
    grid: Grid::new(size, scrollback_len),
    modal: None,
    pr,
    pc,

    screen_size: size,
    clients: Vec::new(),
  };

  if let Some(hook) = &app.config.on_init {
    app.pc.send_self_custom(hook.as_action().clone());
  }

  app.run().await?;

  Ok(())
}

#[cfg(test)]
mod tests {
  use super::*;

  fn task_config(name: &str, deps: &[&str]) -> TaskConfig {
    TaskConfig {
      path: name.to_string(),
      cmd: Some(CmdConfig::Shell {
        shell: "true".to_string(),
      }),
      deps: deps.iter().map(|dep| dep.to_string()).collect(),
      ..TaskConfig::default()
    }
  }

  #[test]
  fn resolve_task_deps_maps_names_to_task_ids() {
    let task_configs = vec![
      task_config("db", &[]),
      task_config("api", &["db"]),
      task_config("web", &["api", "db"]),
    ];
    let task_ids = vec![TaskId(1), TaskId(2), TaskId(3)];

    let deps = resolve_task_deps(&task_configs, &task_ids).unwrap();

    assert_eq!(
      deps,
      vec![vec![], vec![TaskId(1)], vec![TaskId(2), TaskId(1)]]
    );
  }

  #[test]
  fn resolve_task_deps_rejects_unknown_dependency() {
    let task_configs = vec![task_config("api", &["db"])];
    let task_ids = vec![TaskId(1)];

    let err = resolve_task_deps(&task_configs, &task_ids).unwrap_err();

    assert_eq!(
      err.to_string(),
      "Process 'api' depends on unknown process 'db'."
    );
  }

  #[test]
  fn resolve_task_deps_rejects_dependency_cycles() {
    let task_configs = vec![
      task_config("api", &["worker"]),
      task_config("worker", &["db"]),
      task_config("db", &["api"]),
    ];
    let task_ids = vec![TaskId(1), TaskId(2), TaskId(3)];

    let err = resolve_task_deps(&task_configs, &task_ids).unwrap_err();

    assert_eq!(
      err.to_string(),
      "Process dependency cycle detected: api -> worker -> db -> api."
    );
  }
}
