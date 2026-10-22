//! The client side of a `wait`: the request on a connection of its own,
//! its events up to the verdict, and how the CLI shows them.

use std::{
  cell::RefCell,
  collections::HashMap,
  io::{IsTerminal, Write},
  time::{Duration, Instant},
};

use anyhow::bail;
use unicode_width::UnicodeWidthStr;

use crate::{
  dekit::rpc_client::RpcConn,
  kernel::task::TaskId,
  protocol::{
    CtlMsg, RpcRequest, RpcWaitTask, RpcWhy, ScreenResult, WaitDoneEvent,
    WaitMark, WaitOutcome, WaitTasksEvent,
    rpc::{
      EVENT_TASK_CHANGED, EVENT_TASK_REMOVED, EVENT_TASKS, EVENT_WAIT_DONE,
      FEATURE_WAIT,
    },
  },
  runner::RunnerSpec,
  target::Target,
  term::vt::{ScanMode, Scanner, Seq},
};

/// The exit status of a wait that ran out of time, as `timeout(1)` has.
pub const TIMED_OUT: i32 = 124;

/// The process status a wait's outcome ends with.
pub fn exit_code(outcome: WaitOutcome) -> i32 {
  match outcome {
    WaitOutcome::Met => 0,
    WaitOutcome::Failed => 1,
    WaitOutcome::Timeout => TIMED_OUT,
  }
}

/// A wait's limit as the command line writes it: a duration as the config
/// writes one, or zero (`0`, `0s`, ...) to check once.
pub fn timeout_ms(text: Option<&str>) -> anyhow::Result<Option<u64>> {
  let Some(text) = text else {
    return Ok(None);
  };
  let number = text.trim_end_matches(|c: char| c.is_ascii_alphabetic());
  let unit = &text[number.len()..];
  if !number.is_empty()
    && number.bytes().all(|b| b == b'0')
    && matches!(unit, "" | "ms" | "s" | "m" | "h")
  {
    return Ok(Some(0));
  }
  let duration = crate::config::duration::parse_duration(text)?;
  Ok(Some(duration.as_millis() as u64))
}

/// What a wait reports on its way: the set, then each task as it
/// changes or is removed.
pub enum Progress {
  Set(Vec<RpcWaitTask>),
  Changed(Box<RpcWaitTask>),
}

/// Sends `request` (an `RpcRequest::Wait`) and follows it to its
/// verdict. `start_runner` starts a runner that isn't running, as the
/// verb the wait carries out would.
pub async fn wait(
  runner: &RunnerSpec,
  request: RpcRequest,
  start_runner: bool,
  mut progress: impl FnMut(Progress),
) -> anyhow::Result<WaitDoneEvent> {
  let mut conn = RpcConn::connect(runner, start_runner).await?;
  if !conn.hello().has(FEATURE_WAIT) {
    bail!(
      "the runner ({}) can't wait; restart it to run this dekit: `dekit down && dekit up`",
      conn.hello().app
    );
  }
  // The ok comes before any event; an error ends it here, with its code.
  conn.request(request).await?;
  loop {
    match conn.recv_ctl().await {
      Ok(CtlMsg::Event(event)) => match event.name.as_str() {
        EVENT_TASKS => {
          let set: WaitTasksEvent = serde_json::from_value(event.params)?;
          progress(Progress::Set(set.tasks));
        }
        EVENT_TASK_CHANGED | EVENT_TASK_REMOVED => {
          let task = serde_json::from_value(event.params)?;
          progress(Progress::Changed(Box::new(task)));
        }
        EVENT_WAIT_DONE => return Ok(serde_json::from_value(event.params)?),
        _ => (),
      },
      Ok(CtlMsg::Bye(bye)) => {
        bail!("the runner ended the wait without a verdict: {}", bye.code)
      }
      Ok(_) => (),
      Err(_) => bail!("the runner went away before the wait ended"),
    }
  }
}

/// Runs a wait for the CLI: progress on stderr as it goes, the verdict
/// and, for what failed, why; with `json`, only the result on stdout.
/// Returns the exit status.
pub async fn cli_wait(
  runner: &RunnerSpec,
  request: RpcRequest,
  start_runner: bool,
  json: bool,
) -> anyhow::Result<i32> {
  let started = Instant::now();
  let live = !json && std::io::stderr().is_terminal();
  let view = RefCell::new(View::new(live, started));
  let result = {
    let waiting = wait(runner, request, start_runner, |progress| {
      if !json {
        view.borrow_mut().apply(progress);
      }
    });
    tokio::pin!(waiting);
    let mut tick = tokio::time::interval(Duration::from_secs(1));
    loop {
      tokio::select! {
        result = &mut waiting => break result?,
        _ = tick.tick(), if live => view.borrow_mut().redraw(),
      }
    }
  };
  if json {
    println!("{}", serde_json::to_string(&result)?);
    return Ok(exit_code(result.wait));
  }
  view.borrow_mut().finish(&result);
  match result.wait {
    WaitOutcome::Met => {}
    WaitOutcome::Timeout => {
      explain_timeout(runner, &result, started.elapsed()).await
    }
    WaitOutcome::Failed => explain_failure(runner, &result).await,
  }
  Ok(exit_code(result.wait))
}

/// One line per task: what it is at, and for how long it has been
/// waited on. On a terminal the lines are redrawn in place; otherwise a
/// line goes out as each task is met or fails.
struct View {
  live: bool,
  started: Instant,
  tasks: Vec<RpcWaitTask>,
  /// When each task stopped pending, for its line's time.
  settled: HashMap<TaskId, Duration>,
  /// Lines on the terminal from the last draw.
  drawn: usize,
}

impl View {
  fn new(live: bool, started: Instant) -> Self {
    View {
      live,
      started,
      tasks: Vec::new(),
      settled: HashMap::new(),
      drawn: 0,
    }
  }

  fn apply(&mut self, progress: Progress) {
    match progress {
      // The first set: what already stood there has no time.
      Progress::Set(tasks) if self.tasks.is_empty() => {
        for task in &tasks {
          if task.wait != WaitMark::Pending {
            self.settled.insert(task.info.id, Duration::ZERO);
            self.report(task);
          }
        }
        self.tasks = tasks;
      }
      // The set again, from a runner that was upgraded meanwhile.
      Progress::Set(tasks) => {
        for task in tasks {
          self.update(task);
        }
      }
      Progress::Changed(task) => self.update(*task),
    }
    self.redraw();
  }

  /// Takes in how a task stands now; its time is when it last stopped
  /// pending.
  fn update(&mut self, task: RpcWaitTask) {
    let id = task.info.id;
    if task.wait == WaitMark::Pending {
      self.settled.remove(&id);
    } else if !self.settled.contains_key(&id) {
      self.settled.insert(id, self.started.elapsed());
      self.report(&task);
    }
    match self.tasks.iter_mut().find(|t| t.info.id == id) {
      Some(slot) => *slot = task,
      None => self.tasks.push(task),
    }
  }

  /// Off a terminal, a task's line as it is met; what fails is told at
  /// the end, with why.
  fn report(&self, task: &RpcWaitTask) {
    if !self.live && task.wait == WaitMark::Met {
      eprintln!("{}: {}", task.info.path, state_text(task));
    }
  }

  fn redraw(&mut self) {
    if !self.live {
      return;
    }
    let pad = self
      .tasks
      .iter()
      .map(|t| t.info.path.width())
      .max()
      .unwrap_or(0);
    let now = self.started.elapsed();
    // Each line stays on one row, so moving up `drawn` rows finds them.
    let width = terminal_width().unwrap_or(usize::MAX).saturating_sub(1);
    let mut out = String::new();
    if self.drawn > 0 {
      out.push_str(&format!("\x1b[{}F", self.drawn));
    }
    for task in &self.tasks {
      let (mark, color) = match task.wait {
        WaitMark::Met => ("✓", "\x1b[32m"),
        WaitMark::Failed => ("✗", "\x1b[31m"),
        WaitMark::Pending => ("…", "\x1b[2m"),
      };
      let time = match self.settled.get(&task.info.id) {
        Some(at) if at.is_zero() => String::new(),
        Some(at) => duration_text(*at),
        None => duration_text(now),
      };
      // Padded to the widest path by columns, not characters.
      let path = &task.info.path;
      let fill = " ".repeat(pad.saturating_sub(path.width()));
      let text = format!("{path}{fill}  {}  {time}", state_text(task));
      let text = clip(text.trim_end(), width.saturating_sub(2));
      out.push_str(&format!("\x1b[2K{color}{mark}\x1b[0m {text}\n"));
    }
    self.drawn = self.tasks.len();
    let mut stderr = std::io::stderr();
    let _ = stderr.write_all(out.as_bytes());
    let _ = stderr.flush();
  }

  fn finish(&mut self, result: &WaitDoneEvent) {
    for task in &result.tasks {
      if let Some(slot) =
        self.tasks.iter_mut().find(|t| t.info.id == task.info.id)
      {
        *slot = task.clone();
      }
    }
    self.redraw();
  }
}

/// A task's state as `ls` shows it, and removed or blocked as the wait
/// saw it.
fn state_text(task: &RpcWaitTask) -> String {
  if task.removed {
    return "removed".to_string();
  }
  let state = crate::dekit::main::human_state(&task.info.state);
  match &task.blocked_by {
    Some(root) => format!("{state}, blocked by {root}"),
    None => state,
  }
}

/// The width of the terminal on stderr.
fn terminal_width() -> Option<usize> {
  #[cfg(unix)]
  {
    let size = rustix::termios::tcgetwinsize(std::io::stderr()).ok()?;
    (size.ws_col > 0).then_some(size.ws_col as usize)
  }
  #[cfg(not(unix))]
  {
    None
  }
}

/// `text` cut to `width` columns, an ellipsis marking the cut.
fn clip(text: &str, width: usize) -> String {
  use unicode_width::UnicodeWidthChar;
  if text.width() <= width {
    return text.to_string();
  }
  let mut out = String::new();
  let mut used = 0;
  for c in text.chars() {
    let w = c.width().unwrap_or(0);
    if used + w + 1 > width {
      break;
    }
    out.push(c);
    used += w;
  }
  out.push('…');
  out
}

fn duration_text(duration: Duration) -> String {
  let secs = duration.as_secs();
  if secs < 10 {
    format!("{:.1}s", duration.as_secs_f64())
  } else if secs < 60 {
    format!("{secs}s")
  } else {
    format!("{}m{:02}s", secs / 60, secs % 60)
  }
}

/// Lines of a task's screen shown under a failure.
const TAIL_LINES: usize = 20;

/// The runner asked `why` and for screens as a wait is explained: one
/// connection, made at the first question, and none when it can't be.
struct Asker<'a> {
  runner: &'a RunnerSpec,
  conn: Option<RpcConn>,
  tried: bool,
}

impl<'a> Asker<'a> {
  fn new(runner: &'a RunnerSpec) -> Self {
    Asker {
      runner,
      conn: None,
      tried: false,
    }
  }

  async fn ask(&mut self, request: RpcRequest) -> Option<serde_json::Value> {
    if !self.tried {
      self.tried = true;
      self.conn = RpcConn::connect(self.runner, false).await.ok();
    }
    self.conn.as_mut()?.request(request).await.ok()
  }
}

/// Says what failed and why, then the end of the screen of each task at
/// the root of a failure, once.
async fn explain_failure(runner: &RunnerSpec, result: &WaitDoneEvent) {
  let mut asker = Asker::new(runner);
  let mut roots: Vec<String> = Vec::new();
  for task in result.tasks.iter().filter(|t| t.wait == WaitMark::Failed) {
    let path = &task.info.path;
    let text = match &task.blocked_by {
      Some(root) => {
        let state = why(&mut asker, root)
          .await
          .map(|why| crate::dekit::main::human_state(&why.state));
        match state {
          Some(state) => format!("can't start because of {root}: {state}"),
          None => format!("can't start: {root} won't get ready"),
        }
      }
      None => why_text(task, result.quitting),
    };
    eprintln!("{path}: {text}");
    if !task.removed {
      let root = task.blocked_by.clone().unwrap_or_else(|| path.clone());
      if !roots.contains(&root) {
        roots.push(root);
      }
    }
  }
  for root in roots {
    if let Some(tail) = screen_tail(&mut asker, &root).await
      && !tail.is_empty()
    {
      eprintln!("--- {root}, last lines ---");
      for line in tail {
        eprintln!("{line}");
      }
    }
  }
}

/// Why a failed task that no dep holds back failed, from its state and
/// the `why` it came with; `quitting`, that the runner is stopping.
fn why_text(task: &RpcWaitTask, quitting: bool) -> String {
  let path = &task.info.path;
  if task.removed {
    return "removed while waited on".to_string();
  }
  let state = crate::dekit::main::human_state(&task.info.state);
  match task.info.state.reason.as_deref() {
    Some("kill_timeout") => {
      return format!("{state}: its process outlived a kill and may still run");
    }
    // Not ready in time, whether it is still stopping for it or not.
    Some(_) => return state,
    None => (),
  }
  // A run that ended says it all.
  if matches!(
    task.info.state.state.as_str(),
    "exited" | "done" | "backoff"
  ) {
    return state;
  }
  let Some(why) = &task.why else {
    return state;
  };
  if why.vetoed {
    return format!("vetoed; `dekit start {path}` lifts it");
  }
  if !why.wanted && quitting {
    return format!("{state}: the runner is stopping");
  }
  if !why.wanted {
    return format!("not started; `dekit start {path} --wait` starts it");
  }
  // Wanted and not ended: a wait for it to be down, on a task something
  // still needs.
  let who = match why.required_by.as_slice() {
    [] => "a started task still needs".to_string(),
    [one] => format!("{one} still needs"),
    [rest @ .., last] => format!("{} and {last} still need", rest.join(", ")),
  };
  format!(
    "{state}: {who} it, so it keeps running; `dekit veto {path}` keeps it down"
  )
}

/// What is still pending when a wait times out, and what each waits on.
async fn explain_timeout(
  runner: &RunnerSpec,
  result: &WaitDoneEvent,
  waited: Duration,
) {
  eprintln!("Timed out after {}.", duration_text(waited));
  let mut asker = Asker::new(runner);
  for task in result.tasks.iter().filter(|t| t.wait == WaitMark::Pending) {
    let path = &task.info.path;
    let mut line = format!("{path}: {}", state_text(task));
    if let Some(why) = why(&mut asker, path).await {
      if let Some(dep) = why.deps.iter().find(|dep| !dep.satisfied) {
        line.push_str(&format!(
          ", waiting for {} ({})",
          dep.path,
          crate::dekit::main::human_state(&dep.state)
        ));
      }
      if why.attempts > 0 {
        line.push_str(&format!(", {} failed starts", why.attempts));
      }
      eprintln!("{line}");
      // A task retrying after failures shows why it fails.
      if why.attempts > 0
        && let Some(tail) = screen_tail(&mut asker, path).await
        && !tail.is_empty()
      {
        eprintln!("--- {path}, last lines ---");
        for line in tail {
          eprintln!("{line}");
        }
      }
    } else {
      eprintln!("{line}");
    }
  }
}

async fn why(asker: &mut Asker<'_>, path: &str) -> Option<RpcWhy> {
  let target: Target = path.parse().ok()?;
  let reply = asker.ask(RpcRequest::Why { target }).await?;
  serde_json::from_value(reply).ok()
}

/// The last lines of a task's screen with something on them, as plain
/// text.
async fn screen_tail(asker: &mut Asker<'_>, path: &str) -> Option<Vec<String>> {
  let target: Target = path.parse().ok()?;
  let reply = asker.ask(RpcRequest::Screen { target }).await?;
  let screen: ScreenResult = serde_json::from_value(reply).ok()?;
  let text = plain_text(&screen.screen);
  let lines: Vec<&str> = text
    .lines()
    .map(str::trim_end)
    .filter(|line| !line.is_empty())
    .collect();
  let start = lines.len().saturating_sub(TAIL_LINES);
  Some(lines[start..].iter().map(|line| line.to_string()).collect())
}

/// The text of ANSI output, without its escape sequences.
fn plain_text(ansi: &str) -> String {
  let mut text = String::new();
  let mut scanner = Scanner::new(ScanMode::Output);
  let mut take = |seq: Seq<'_>| match seq {
    Seq::Text(s) => text.push_str(s),
    Seq::Ctl(b'\n') => text.push('\n'),
    _ => (),
  };
  scanner.feed(ansi.as_bytes(), &mut take);
  scanner.flush(&mut take);
  text
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn plain_text_drops_escapes() {
    assert_eq!(
      plain_text("\x1b[31merror\x1b[0m: bad\r\n\x1b]0;title\x07next"),
      "error: bad\nnext"
    );
  }

  #[test]
  fn timeouts_take_durations_and_any_zero() {
    assert_eq!(timeout_ms(None).unwrap(), None);
    for zero in ["0", "0s", "0ms", "00m"] {
      assert_eq!(timeout_ms(Some(zero)).unwrap(), Some(0), "{zero}");
    }
    assert_eq!(timeout_ms(Some("90s")).unwrap(), Some(90_000));
    assert!(timeout_ms(Some("0x")).is_err());
    assert!(timeout_ms(Some("25h")).is_err());
  }

  #[test]
  fn lines_are_clipped_to_the_terminal() {
    assert_eq!(clip("api  ready", 20), "api  ready");
    assert_eq!(clip("api  ready  1.2s", 8), "api  re…");
    assert_eq!(clip("日本語のタスク", 7), "日本語…");
  }

  /// A task nothing wants is "not started", unless the runner is
  /// stopping; a run that failed says how, either way.
  #[test]
  fn failures_say_whether_the_runner_is_stopping() {
    use crate::protocol::{RpcState, RpcTaskInfo};
    let task = |state: &str, code: Option<i32>| {
      let rpc_state = RpcState {
        state: state.to_string(),
        exit_code: code,
        signal: None,
        reason: None,
      };
      RpcWaitTask {
        info: RpcTaskInfo {
          id: TaskId(1),
          path: "api".to_string(),
          label: None,
          kind: Some("service".to_string()),
          state: rpc_state.clone(),
        },
        wait: WaitMark::Failed,
        blocked_by: None,
        removed: false,
        why: Some(RpcWhy {
          id: TaskId(1),
          path: "api".to_string(),
          state: rpc_state,
          wanted: false,
          supported: false,
          vetoed: false,
          pinned: false,
          saved_pin: false,
          required_by: Vec::new(),
          deps: Vec::new(),
          attempts: 0,
        }),
      }
    };
    assert!(why_text(&task("idle", None), false).starts_with("not started"));
    assert_eq!(
      why_text(&task("stopping", None), true),
      "stopping: the runner is stopping"
    );
    assert_eq!(why_text(&task("exited", Some(1)), true), "exited (code 1)");
  }

  /// A task removed while waited on settles as the kernel marked it,
  /// and its line says so.
  #[test]
  fn a_removed_task_settles_in_the_view() {
    use crate::protocol::{RpcState, RpcTaskInfo};
    let task = |wait, removed| RpcWaitTask {
      info: RpcTaskInfo {
        id: TaskId(1),
        path: "db".to_string(),
        label: None,
        kind: None,
        state: RpcState {
          state: "ready".to_string(),
          exit_code: None,
          signal: None,
          reason: None,
        },
      },
      wait,
      blocked_by: None,
      removed,
      why: None,
    };
    let mut view = View::new(false, Instant::now());
    view.apply(Progress::Set(vec![task(WaitMark::Pending, false)]));
    assert!(view.settled.is_empty());
    view.apply(Progress::Changed(Box::new(task(WaitMark::Met, true))));
    assert!(view.settled.contains_key(&TaskId(1)));
    assert_eq!(view.tasks[0].wait, WaitMark::Met);
    assert_eq!(state_text(&view.tasks[0]), "removed");
  }

  #[test]
  fn durations_read_short() {
    assert_eq!(duration_text(Duration::from_millis(1234)), "1.2s");
    assert_eq!(duration_text(Duration::from_secs(14)), "14s");
    assert_eq!(duration_text(Duration::from_secs(130)), "2m10s");
  }
}
