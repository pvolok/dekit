//! The console: one user's view of the runners it shows, drawn into a
//! screen of its own that a host paints on a terminal (`host`). It runs
//! in the client, and speaks to each runner as any client does: a watch
//! for its task list, requests for what it does, and an attach for the
//! terminal it shows (`link`, `pane`).

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
    keymap::{Keymap, KeymapGroup, Lookup},
    link::{Link, RunnerEvent},
    modal::{
      commands_menu::CommandsMenuModal,
      menu::{MenuItem, MenuModal},
      modal::{Modal, ModalResult},
      quit::QuitModal,
      runner_select::RunnerSelectModal,
    },
    pane::{Pane, PaneCtl, PaneEvent},
    state::{ConfigStatus, Scope, State},
    task_tree::{Node, TaskTree},
    theme::{BlockTheme, Theme},
    ui_header::{HeaderInfo, render_header},
    ui_keymap::render_keymap,
    ui_tasks::render_tasks,
    ui_term::render_term,
    ui_zoom_tip::render_zoom_tip,
  },
  kernel::{
    task::TaskId,
    task_path::{TaskPath, path_name, unique},
  },
  protocol::{
    ScreenCommand, TermCaps, caps::Background, screen as wire_screen,
  },
  spec::TaskSpec,
  target::Target,
  term::{
    CursorStyle, Geometry, Grid, Screen, ScreenHandle, TermEvent,
    grid::Rect,
    key::{Key, KeyCode, KeyEventKind},
    mouse::{MouseButton, MouseEvent, MouseEventKind},
    style::Style,
  },
};

#[path = "app_edit.rs"]
mod edit;
#[path = "app_runners.rs"]
mod runners;
use edit::{ConsoleMsg, Purpose};
pub(crate) use runners::{Runner, RunnerId};

/// What a console is told.
pub enum AppMsg {
  /// From the terminal it is on.
  Input(TermEvent),
  Runner(RunnerId, RunnerEvent),
  /// From the session of the pane with this number.
  Pane(u64, PaneEvent),
  /// Done off the console's loop: what to do on it.
  Post(ConsoleMsg),
  /// An mprocs hook's or its ctl server's.
  Action(Action),
  /// The user config changed, as it reads now.
  UserConfig(Arc<Config>),
}

/// What a console tells its host.
pub enum HostMsg {
  /// The screen changed.
  Paint,
  /// Text to put on the user's clipboard.
  Copy(String),
  /// The console is done: the user left it, or else why.
  Exit(Option<String>),
}

/// How a console starts.
pub struct Setup {
  /// What it draws with: the user config, or mprocs'.
  pub config: Arc<Config>,
  pub keymap: Keymap,
  /// The terminal's size, and what it can show.
  pub geometry: Geometry,
  pub caps: TermCaps,
  /// The runners shown from the start, the first with the focus.
  pub links: Vec<Link>,
  /// Each runner's tasks under a group of its own, and the others that
  /// run on this machine shown too; otherwise the one runner's tasks at
  /// the top of the list, as `dekit mprocs` has them.
  pub all_runners: bool,
  /// Draw again as the user config changes.
  pub follow_user_config: bool,
}

/// A console started: where to tell it things, what it tells its host,
/// and the screen it draws into.
pub struct Started {
  pub app: UnboundedSender<AppMsg>,
  pub host: UnboundedReceiver<HostMsg>,
  pub screen: ScreenHandle,
}

pub fn start(setup: Setup) -> Started {
  let (tx, msgs) = unbounded_channel();
  let (host, host_rx) = unbounded_channel();
  let screen = Screen::new(setup.geometry.size, 0);
  let handle = ScreenHandle::new(screen.frame());
  if setup.follow_user_config {
    runners::follow_user_config(tx.clone());
  }
  let mut app = App {
    keymap: setup.keymap,
    theme: Theme::resolve(setup.config.tui.theme, setup.caps.background),
    background: setup.caps.background,
    state: State {
      scope: Scope::Tasks,
      tasks: TaskTree::new(),
      hide_keymap_window: !setup.config.tui.tips.show,
      pending_keys: Vec::new(),
      quitting: false,
      header: Default::default(),
      unsaved_hit: None,
      hover: None,
      toast: None,
      config: ConfigStatus::default(),
      drag: None,
      copy: false,
    },
    config: setup.config,
    caps: setup.caps,
    modal: None,
    modal_seq: 0,
    modal_runner: None,
    msgs,
    tx: tx.clone(),
    host,
    screen,
    handle: handle.clone(),
    geometry: setup.geometry,
    runners: Vec::new(),
    next_runner: 0,
    next_id: 1,
    all_runners: setup.all_runners,
    connecting: Default::default(),
    failed: Default::default(),
    pane: None,
    panes: 0,
    pane_failed: None,
    stop: false,
    dirty: true,
  };
  for link in setup.links {
    app.add_runner(link);
  }
  if app.all_runners {
    runners::discover(tx.clone());
  }
  tokio::spawn(app.run());
  Started {
    app: tx,
    host: host_rx,
    screen: handle,
  }
}

pub struct App {
  /// What it draws with.
  config: Arc<Config>,
  keymap: Keymap,
  theme: Theme,
  /// The terminal's background, as the host said.
  background: Option<Background>,
  /// What the terminal can show, as the host said: the panes are painted
  /// for it.
  caps: TermCaps,
  state: State,
  modal: Option<Box<dyn Modal>>,
  /// How many modals were opened: the number of the one open.
  modal_seq: u64,
  /// The runner the open modal's commands go to.
  modal_runner: Option<RunnerId>,
  msgs: UnboundedReceiver<AppMsg>,
  tx: UnboundedSender<AppMsg>,
  host: UnboundedSender<HostMsg>,

  /// What it draws into, and where each picture of it goes.
  screen: Screen,
  handle: ScreenHandle,
  /// The terminal's size, and its cells' in pixels.
  geometry: Geometry,

  /// The runners shown, in list order.
  runners: Vec<Runner>,
  next_runner: u64,
  /// The next id for a task or folder in the list: the runners' own
  /// ids are each runner's.
  next_id: usize,
  all_runners: bool,
  /// Runners being connected to, not shown yet.
  connecting: std::collections::HashSet<crate::runner::RunnerSpec>,
  /// Runners that could not be shown, and when.
  failed: std::collections::HashMap<crate::runner::RunnerSpec, Instant>,

  /// The selected task's screen, by the list's id of the task, and its
  /// number.
  pane: Option<(TaskId, u64, Pane)>,
  panes: u64,
  /// The task whose screen could not be shown: not tried again until
  /// another is selected.
  pane_failed: Option<TaskId>,

  stop: bool,
  /// Something the picture depends on changed since the last render.
  dirty: bool,
}

impl App {
  /// Repaints in the theme `tui.theme` asks for, given what the terminal
  /// said about its background.
  fn apply_theme(&mut self) {
    let theme = Theme::resolve(self.config.tui.theme, self.background);
    if theme.dark != self.theme.dark {
      self.theme = theme;
      self.dirty = true;
    }
  }

  async fn run(mut self) {
    let mut msgs = Vec::new();
    while !self.stop {
      tokio::select! {
        n = self.msgs.recv_many(&mut msgs, 512) => {
          if n == 0 {
            break;
          }
          for msg in msgs.drain(..) {
            self.handle_msg(msg);
          }
        }
        () = sleep_until(self.state.toast.as_ref().map(|toast| toast.until)) => {
          self.state.toast = None;
          self.dirty = true;
        }
      }
      if self.stop {
        break;
      }
      self.sync_pane();
      self.refresh_status();
      if self.dirty {
        self.dirty = false;
        self.render();
      }
    }
  }

  fn handle_msg(&mut self, msg: AppMsg) {
    match msg {
      AppMsg::Input(event) => {
        self.dirty = true;
        self.handle_input(event);
      }
      AppMsg::Runner(id, event) => {
        self.dirty = true;
        self.runner_event(id, event);
      }
      AppMsg::Pane(number, event) => self.pane_event(number, event),
      AppMsg::Post(msg) => {
        self.dirty = true;
        self.handle_console_msg(msg);
      }
      AppMsg::Action(action) => {
        self.dirty = true;
        self.handle_action(action);
      }
      AppMsg::UserConfig(config) => {
        self.config = config;
        self.keymap = self.config.keymap.build();
        self.apply_theme();
        self.dirty = true;
      }
    }
  }

  /// Leaves, telling the host why unless the user left.
  pub(super) fn exit(&mut self, why: Option<String>) {
    self.stop = true;
    let _ = self.host.send(HostMsg::Exit(why));
  }

  fn render(&mut self) {
    let layout = self.layout();
    let focused = self.focused();
    let runner = focused.and_then(|runner| runner.link.runner().cloned());
    let runner_version =
      focused.map(|runner| runner.link.hello().version.clone());
    let shown = self.shown_frame();
    let theme = &self.theme;
    let vt = &mut self.screen;
    let grid = vt.grid_mut();
    let bg = Style::default()
      .bg(theme.block.bg.into())
      .fg(theme.block.text.into());
    let bg = grid.intern_style(bg);
    grid.erase_all(bg);
    grid.cursor_pos = None;
    grid.cursor_style = CursorStyle::Default;

    let own = env!("CARGO_PKG_VERSION");
    let header = render_header(
      layout.header,
      grid,
      &HeaderInfo {
        runner: runner.as_ref(),
        version: runner_version.as_deref().unwrap_or(own),
        client_versions: vec![own],
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
      &self.config,
      &self.keymap,
      theme,
    );
    render_term(layout.term, grid, &self.state, shown.as_deref(), theme);
    let mut hints = layout.keymap;
    if hints.height == 0 && !self.state.pending_keys.is_empty() {
      // Zoomed, or without the hints: the keys that can follow still
      // show, over the bottom of the screen, as nothing else says that
      // a binding is half typed.
      hints = grid.area().take_bottom(AppLayout::HINTS_HEIGHT);
    }
    render_keymap(hints, grid, &self.state, &self.keymap, theme);
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
    self.handle.publish(vt.frame());
    let _ = self.host.send(HostMsg::Paint);
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
      &self.config,
    )
  }

  /// What the pane's task is told about its size: the terminal pane in
  /// cells, and the cells' size as the terminal reports it.
  fn pane_geometry(&self) -> Geometry {
    Geometry {
      size: self.layout().term_area().size(),
      cell_pixels: self.geometry.cell_pixels,
    }
  }

  fn handle_input(&mut self, event: TermEvent) {
    match event {
      TermEvent::Key(key) => self.handle_key(key),
      TermEvent::Mouse(mouse) => self.handle_mouse(mouse),
      TermEvent::Paste(text) => {
        if let Some(modal) = &mut self.modal {
          let result = modal.handle_paste(&text);
          self.modal_result(result);
        } else if self.state.scope.is_term() {
          self.to_pane(PaneCtl::Input(TermEvent::Paste(text)));
        }
      }
      TermEvent::Resize(geometry) => {
        self.geometry = geometry;
        self
          .screen
          .set_size(geometry.size.height, geometry.size.width);
      }
      TermEvent::FocusGained | TermEvent::FocusLost => {
        self.to_pane(PaneCtl::Input(event));
      }
    }
  }

  /// Copies `text` for the user.
  pub(super) fn yank(&mut self, text: String) {
    let _ = self.host.send(HostMsg::Copy(text));
  }

  fn handle_mouse(&mut self, mouse: MouseEvent) {
    let frame = self.frame();
    self.state.hover = Some((mouse.x as u16, mouse.y as u16));
    if let Some(modal) = &mut self.modal {
      let outside = !modal.area(frame).contains(mouse.x as u16, mouse.y as u16);
      let result = match mouse.kind {
        MouseEventKind::Down(_) if outside => modal.click_outside(),
        _ if outside => ModalResult::Keep,
        _ => modal.handle_mouse(&mouse),
      };
      self.modal_result(result);
      return;
    }
    // What is dragged from the task list is dragged until let go,
    // wherever the pointer is; a press means the release was missed.
    if self.state.drag.is_some() {
      if matches!(mouse.kind, MouseEventKind::Down(_)) {
        self.state.drag = None;
      } else {
        self.sidebar_mouse(&mouse);
        return;
      }
    }
    let layout = self.layout();
    let (x, y) = (mouse.x as u16, mouse.y as u16);
    let term_area = layout.term_area();
    // A selection dragged out of the terminal keeps driving it from the
    // nearest edge cell, so the runner can scroll the view under it. The
    // release has to reach it too, or the drag never ends.
    let drag_outside =
      matches!(
        mouse.kind,
        MouseEventKind::Drag(MouseButton::Left)
          | MouseEventKind::Up(MouseButton::Left)
      ) && matches!(self.state.scope, Scope::Term | Scope::TermZoom)
        && !term_area.contains(x, y)
        && term_area.width > 0
        && term_area.height > 0;
    if drag_outside {
      let mut event = mouse;
      event.x = x.clamp(term_area.x, term_area.right() - 1) as i32;
      event.y = y.clamp(term_area.y, term_area.bottom() - 1) as i32;
      let event = event.translate(term_area);
      self.to_pane(PaneCtl::Input(TermEvent::Mouse(event)));
      return;
    }
    if layout.header.contains(x, y) {
      if mouse.kind == MouseEventKind::Down(MouseButton::Left) {
        let hits = self.state.header;
        if hits.logo.is_some_and(|hit| hit.contains(x, y)) {
          self.handle_action(Action::ShowMainMenu);
        } else if hits.reload.is_some_and(|hit| hit.contains(x, y)) {
          self.handle_action(Action::ShowReload);
        } else if hits.runner.is_some_and(|hit| hit.contains(x, y)) {
          self.handle_action(Action::ShowRunnerSelect {
            tab: RunnerTab::Switch,
          });
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
      self.handle_action(Action::ShowWriteBack);
      return;
    }
    if term_area.contains(x, y) {
      if pressed && self.state.scope == Scope::Tasks {
        self.state.scope = Scope::Term;
      }
      let event = mouse.translate(term_area);
      self.to_pane(PaneCtl::Input(TermEvent::Mouse(event)));
    } else if layout.sidebar.contains(x, y) {
      if pressed && self.state.scope == Scope::Term {
        self.state.scope = Scope::Tasks;
      }
      self.sidebar_mouse(&mouse);
    }
  }

  fn handle_key(&mut self, key: Key) {
    match key.kind {
      KeyEventKind::Press | KeyEventKind::Repeat => (),
      KeyEventKind::Release => return,
    }
    if self.state.drag.is_some() && key.code == KeyCode::Esc {
      // Esc lets go of what is dragged.
      self.state.drag = None;
      return;
    }
    if let Some(modal) = &mut self.modal {
      self.state.pending_keys.clear();
      let result = modal.handle_key(&key);
      self.modal_result(result);
      return;
    }
    let mid_binding = !self.state.pending_keys.is_empty();
    if mid_binding && matches!(key.code, KeyCode::Modifier(_)) {
      // Shift on its way to the next key.
      return;
    }
    let group = self.state.keymap_group();
    match self
      .keymap
      .lookup(group, &mut self.state.pending_keys, &key)
    {
      Lookup::Action(action) => {
        let action = match action {
          // As it was typed, which is the leader's character on the
          // layout in use.
          Action::SendLeader => Action::SendKey { key },
          action => action.clone(),
        };
        self.handle_action(action)
      }
      // Unbound keys go to the process; in copy mode the keymap is the
      // whole vocabulary, so they are dropped rather than fed to the
      // screen's own copy-mode keys. So is a key that ends a binding
      // half typed, with what was typed of it.
      Lookup::None if group == KeymapGroup::Term && !mid_binding => {
        self.handle_action(Action::SendKey { key });
      }
      Lookup::Prefix | Lookup::None => (),
    }
  }

  pub(super) fn handle_action(&mut self, action: Action) {
    match action {
      Action::Batch { cmds } => {
        for cmd in cmds {
          self.handle_action(cmd);
        }
      }

      Action::ShowQuit => self.open(Box::new(QuitModal::default())),
      Action::Detach => self.exit(None),
      Action::Down | Action::Quit => {
        let command = match action {
          Action::Down => Command::Down,
          _ => Command::Quit,
        };
        let Some(runner) = self.focused().map(|runner| runner.id) else {
          return self.exit(None);
        };
        self.state.quitting = true;
        // The runner goes, and the console with it.
        self.issue_then(runner, command, |app, result| match result {
          Ok(()) => app.exit(None),
          Err(err) => {
            app.state.quitting = false;
            app.toast(err, true);
          }
        });
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
        if self.all_runners {
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
          ]);
        }
        if self.has_project() {
          items.extend([
            item("Reload the config…", Action::ShowReload).separated(),
            item("Write back to the config…", Action::ShowWriteBack),
          ]);
        }
        items.push(item("Quit", Action::ShowQuit).separated());
        self.open(Box::new(MenuModal::new(anchor, items)));
      }
      Action::ShowRunnerSelect { tab } => {
        if self.all_runners {
          let runner = self.focused().and_then(|r| r.link.runner().cloned());
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
      Action::SwitchRunner { kind, root } => self.switch_runner(&kind, &root),
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
      // Every task of the runner in focus, not of every runner shown.
      Action::RestartAll => self.issue(Command::Restart {
        target: Target::glob("**"),
      }),
      Action::ForceRestartAll => self.issue(Command::ForceRestart {
        target: Target::glob("**"),
      }),

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
        let Some(runner) = self.focused().map(|runner| runner.id) else {
          return;
        };
        let label =
          self.unique_label(runner, &name.unwrap_or_else(|| cmd.clone()), None);
        let path = self.unique_path(runner, &path_name(&label));
        let argv =
          crate::parse_shell::typed_argv(&cmd, self.config.system_shell);
        match (path.parse::<Target>(), argv) {
          (Ok(target), Ok(argv)) => {
            let mut spec = TaskSpec::with_cmd(argv);
            spec.set_label(Some(&label));
            self.issue_on(
              runner,
              Command::Add {
                target,
                spec,
                place: Default::default(),
                start: true,
                defaults: true,
                origin: None,
              },
            )
          }
          (_, Err(err)) => self.toast(format!("`{cmd}`: {err}"), true),
          (Err(err), Ok(_)) => log::warn!("Cannot add task '{path}': {err}"),
        }
      }
      Action::DuplicateTask => {
        if let Some(task) = self.state.current_task() {
          let (runner, remote) = (task.runner, task.remote);
          let name = self.unique_label(runner, &task.name(), None);
          self.issue_on(
            runner,
            Command::Duplicate {
              target: Target::Id(remote),
              name: Some(name),
              path: None,
              start: true,
            },
          );
        }
      }
      Action::RenameTask { name } => {
        if let Some(task) = self.state.current_task() {
          let (runner, remote, id) = (task.runner, task.remote, task.id);
          let name = self.unique_label(runner, &name, Some(id));
          self.issue_on(
            runner,
            Command::Rename {
              target: Target::Id(remote),
              name,
            },
          );
        }
      }
      Action::ShowRemoveTask => self.show_remove(),

      Action::ScrollUp { n, unit } => {
        self.screen_command(ScreenCommand::Scroll {
          delta: n.min(i32::MAX as usize) as i32,
          unit: wire_scroll_unit(unit),
        });
      }
      Action::ScrollDown { n, unit } => {
        self.screen_command(ScreenCommand::Scroll {
          delta: -(n.min(i32::MAX as usize) as i32),
          unit: wire_scroll_unit(unit),
        });
      }
      Action::ScrollReset => self.screen_command(ScreenCommand::ScrollReset),
      Action::CopyModeEnter => {
        if self.state.current_task().is_some() {
          self.state.scope = Scope::Term;
        }
        self.screen_command(ScreenCommand::CopyEnter);
      }
      Action::CopyModeLeave => self.screen_command(ScreenCommand::CopyLeave),
      Action::CopyModeMove { dir } => {
        self.screen_command(ScreenCommand::CopyMove {
          dir: wire_copy_move(dir),
        })
      }
      Action::CopyModeEnd => self.screen_command(ScreenCommand::CopySelect),
      Action::CopyModeCopy => self.screen_command(ScreenCommand::CopyYank),
      Action::SendKey { key } => {
        self.to_pane(PaneCtl::Input(TermEvent::Key(key)))
      }
      // Only a key press has a key to send, and `handle_key` sends it.
      Action::SendLeader => (),
    }
  }

  /// Whether a runner shown has a project to reload and write back to;
  /// `dekit mprocs` has none.
  fn has_project(&self) -> bool {
    self
      .runners
      .iter()
      .any(|runner| runner.link.runner().is_some())
  }

  /// `unique`, among a runner's paths: one taken while a task is there
  /// or under it.
  fn unique_path(&self, runner: RunnerId, base: &str) -> String {
    let paths = self.remote_paths(runner);
    unique(base, |path| {
      paths
        .iter()
        .any(|taken| crate::kernel::task_path::overlaps(taken, path))
    })
  }

  fn unique_label(
    &self,
    runner: RunnerId,
    base: &str,
    exclude: Option<TaskId>,
  ) -> String {
    unique(base, |name| {
      self.state.tasks.iter().any(|t| {
        t.runner == runner && Some(t.id) != exclude && t.name() == name
      })
    })
  }

  fn screen_command(&self, command: ScreenCommand) {
    self.to_pane(PaneCtl::Screen(command));
  }

  /// To the selected task's screen, when it is shown.
  fn to_pane(&self, ctl: PaneCtl) {
    if let Some((id, _, pane)) = &self.pane
      && self.shown_task() == Some(*id)
    {
      pane.send(ctl);
    }
  }

  /// Runs a command on the selected task or every task in the selected
  /// group.
  fn issue_selected(&self, make: fn(Target) -> Command) {
    if let Some((runner, target)) = self.selected_target() {
      self.issue_on(runner, make(target));
    }
  }

  fn pane_event(&mut self, number: u64, event: PaneEvent) {
    let Some((id, current, pane)) = &mut self.pane else {
      return;
    };
    if *current != number {
      return;
    }
    let id = *id;
    match event {
      // Only the shown task's screen is part of the picture.
      PaneEvent::Render => self.dirty = true,
      PaneEvent::State(state) => {
        pane.state = state;
        self.state.copy = pane.state.copy;
        self.dirty = true;
      }
      PaneEvent::Copy(text) => self.yank(text),
      PaneEvent::Ended(why) => {
        self.pane = None;
        self.state.copy = false;
        if let Some(why) = why {
          // Not tried again for as long as it stays selected.
          self.pane_failed = Some(id);
          log::debug!("the pane closed: {why}");
        }
        self.dirty = true;
      }
    }
  }

  /// The task whose terminal the console shows.
  fn shown_task(&self) -> Option<TaskId> {
    match self.state.tasks.selected() {
      Some((_, Node::Task(task))) => Some(task.id),
      _ => None,
    }
  }

  fn shown_frame(&self) -> Option<Arc<crate::term::Frame>> {
    let (id, _, pane) = self.pane.as_ref()?;
    (self.shown_task() == Some(*id)).then(|| pane.handle.load())
  }

  /// Attaches to the selected task's screen once it is selected, and
  /// keeps it the size of the pane.
  fn sync_pane(&mut self) {
    let shown = self.shown_task();
    if self.pane_failed.is_some() && self.pane_failed != shown {
      self.pane_failed = None;
    }
    let geometry = self.pane_geometry();
    match &mut self.pane {
      Some((id, _, pane)) if Some(*id) == shown => {
        let size = geometry.size;
        if (pane.state.width, pane.state.height) != (size.width, size.height)
          && pane.asked != Some(geometry)
        {
          pane.asked = Some(geometry);
          pane.send(PaneCtl::Input(TermEvent::Resize(geometry)));
        }
        return;
      }
      _ => (),
    }
    self.pane = None;
    self.state.copy = false;
    let Some(task) = shown.and_then(|id| self.state.tasks.task(id)) else {
      return;
    };
    if self.pane_failed == shown
      || geometry.size.width == 0
      || geometry.size.height == 0
    {
      return;
    }
    let (id, remote) = (task.id, task.remote);
    let Some(link) = self.runner(task.runner).map(|r| r.link.clone()) else {
      return;
    };
    self.panes += 1;
    let number = self.panes;
    let tx = self.tx.clone();
    let mut pane =
      Pane::open(&link, remote, geometry, self.caps, move |event| {
        let _ = tx.send(AppMsg::Pane(number, event));
      });
    pane.asked = Some(geometry);
    self.pane = Some((id, number, pane));
  }
}

fn wire_copy_move(dir: CopyMove) -> wire_screen::CopyMove {
  match dir {
    CopyMove::Up => wire_screen::CopyMove::Up,
    CopyMove::Down => wire_screen::CopyMove::Down,
    CopyMove::Left => wire_screen::CopyMove::Left,
    CopyMove::Right => wire_screen::CopyMove::Right,
  }
}

fn wire_scroll_unit(unit: ScrollUnit) -> wire_screen::ScrollUnit {
  match unit {
    ScrollUnit::Line => wire_screen::ScrollUnit::Line,
    ScrollUnit::HalfScreen => wire_screen::ScrollUnit::HalfScreen,
    ScrollUnit::Screen => wire_screen::ScrollUnit::Screen,
  }
}

/// Waits until `at`, or for ever without one.
async fn sleep_until(at: Option<Instant>) {
  match at {
    Some(at) => tokio::time::sleep_until(at.into()).await,
    None => std::future::pending().await,
  }
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

/// A path in the list, as a `TaskPath`.
fn list_path(name: &str, remote: &str) -> Option<TaskPath> {
  match name.is_empty() {
    true => TaskPath::new(remote).ok(),
    false => TaskPath::new(format!("{name}/{remote}")).ok(),
  }
}
