//! The `wait` session. Like an attach, it is the last request on its
//! connection: the ok response, the set as a `tasks` event, a
//! `task-changed` or `task-removed` event for each change, and the
//! verdict as `wait-done`; then the server closes.

use std::{sync::Arc, time::Duration};

use tokio::{sync::mpsc::UnboundedReceiver, time::Instant};

use crate::{
  command::Command,
  dekit::server::{
    ConnCtl, ConnReg, ServerCtx, carried, finish, rpc_task_info, rpc_why,
  },
  kernel::{
    kernel_message::{
      Verb, WaitCond, WaitEvent, WaitOn, WaitTask, WaitVerdict,
    },
    task::TaskId,
  },
  protocol::{
    ActResult, Bye, ConnReceiver, ConnSender, CtlMsg, Event, RpcError,
    RpcWaitTask, WaitDoneEvent, WaitFor, WaitOutcome, WaitTasksEvent, codes,
    ctl::Hello,
    ok_result,
    rpc::{
      EVENT_TASK_CHANGED, EVENT_TASK_REMOVED, EVENT_TASKS, EVENT_WAIT_DONE,
    },
  },
  target::Target,
  upgrade::snapshot as snap,
};

/// What a `wait` request asked for.
pub struct WaitParams {
  pub target: Option<Target>,
  pub command: Option<Command>,
  pub wait_for: Option<WaitFor>,
  pub timeout_ms: Option<u64>,
}

/// The connection a wait session is on.
pub struct WaitConn {
  pub reg: ConnReg,
  pub fd: Option<i32>,
  pub hello: Hello,
  pub sender: ConnSender,
  pub receiver: ConnReceiver,
}

/// A wait carried across an upgrade: all it needs, since it reads the
/// state the tasks are in.
pub struct CarriedWait {
  pub ids: Vec<TaskId>,
  pub cond: WaitCond,
  pub timeout: Option<Duration>,
}

/// How a request turns into a kernel wait: what it is on, what for, the
/// verb it carries out, what it named (for a message), and how long it
/// may take.
struct Plan {
  on: WaitOn,
  cond: WaitCond,
  verb: Option<Verb>,
  named: String,
  timeout: Option<Duration>,
}

fn plan(params: &WaitParams) -> Result<Plan, RpcError> {
  let invalid = |message: &str| RpcError::new(codes::INVALID_PARAMS, message);
  // As long as a config may write, so a deadline built from it can't
  // overflow.
  let timeout = params.timeout_ms.map(Duration::from_millis);
  if timeout.is_some_and(|timeout| timeout > crate::config::duration::MAX) {
    return Err(invalid("`timeout_ms` is at most 24h"));
  }
  let bad_target = |err: crate::target::InvalidTarget| {
    RpcError::new(codes::BAD_TARGET, err.to_string())
  };
  match (&params.target, &params.command) {
    (Some(target), None) => {
      let cond = match params.wait_for.unwrap_or(WaitFor::Ready) {
        WaitFor::Ready => WaitCond::Ready,
        WaitFor::Exit => WaitCond::Exit,
      };
      Ok(Plan {
        on: WaitOn::Matches(target.selector().map_err(bad_target)?),
        cond,
        verb: None,
        named: target.to_string(),
        timeout,
      })
    }
    (None, Some(command)) => {
      if params.wait_for.is_some() {
        return Err(invalid(
          "`for` goes with `target`: a command's verb says what it waits for",
        ));
      }
      let Some((verb, target)) = command.verb() else {
        return Err(invalid(
          "only up, start, restart, force-restart, stop, kill, and veto can be waited on",
        ));
      };
      Ok(Plan {
        on: WaitOn::Act(verb, target.selector().map_err(bad_target)?),
        cond: verb.wait_cond(),
        verb: Some(verb),
        named: target.to_string(),
        timeout,
      })
    }
    _ => Err(invalid("a wait takes a `target` or a `command`")),
  }
}

pub async fn wait_session(
  ctx: &Arc<ServerCtx>,
  mut conn: WaitConn,
  request_id: u64,
  params: WaitParams,
) {
  let started = Instant::now();
  let begun = match plan(&params) {
    Ok(Plan {
      on,
      cond,
      verb,
      named,
      timeout,
    }) => {
      let mut events = ctx.pc.wait(on, cond);
      // The kernel is running: connections freeze before it does.
      match events.recv().await {
        Some(WaitEvent::Set(tasks)) => {
          if tasks.is_empty() && verb != Some(Verb::Up) {
            Err(RpcError::new(
              codes::NO_MATCH,
              format!("no task matches '{named}'"),
            ))
          } else {
            let deadline = timeout.map(|timeout| started + timeout);
            Ok((cond, verb, tasks, events, deadline))
          }
        }
        Some(_) | None => Err(RpcError::new(
          codes::INTERNAL,
          "the runner is shutting down",
        )),
      }
    }
    Err(error) => Err(error),
  };
  let (cond, verb, tasks, events, deadline) = match begun {
    Ok(begun) => begun,
    Err(error) => {
      if conn
        .sender
        .queue_ctl(CtlMsg::err(request_id, error))
        .is_ok()
      {
        finish(
          &mut conn.sender,
          &mut conn.reg.ctl,
          conn.fd,
          Some(&conn.hello),
          conn.receiver.buffered(),
        )
        .await;
      }
      return;
    }
  };
  let result = match verb {
    Some(_) => serde_json::to_value(ActResult {
      matched: tasks.len(),
    })
    .expect("a count serializes"),
    None => ok_result(),
  };
  if conn
    .sender
    .queue_ctl(CtlMsg::ok(request_id, result))
    .is_err()
  {
    return;
  }
  run_wait(conn, cond, tasks, events, deadline).await;
}

/// A wait session inherited across an upgrade: the wait begins again on
/// the same tasks, and the client hears the set as it is now.
pub async fn resume_wait_session(
  ctx: Arc<ServerCtx>,
  mut conn: WaitConn,
  wait: CarriedWait,
) {
  let deadline = wait.timeout.map(|left| Instant::now() + left);
  let mut events = ctx.pc.wait(WaitOn::Ids(wait.ids), wait.cond);
  let Some(WaitEvent::Set(tasks)) = events.recv().await else {
    // The kernel stopped first: what the old image owed still goes out.
    let bye = CtlMsg::Bye(Bye::new(codes::QUIT, "the runner is stopping"));
    if conn.sender.queue_ctl(bye).is_ok() {
      finish(
        &mut conn.sender,
        &mut conn.reg.ctl,
        conn.fd,
        Some(&conn.hello),
        conn.receiver.buffered(),
      )
      .await;
    }
    return;
  };
  run_wait(conn, wait.cond, tasks, events, deadline).await;
}

async fn run_wait(
  conn: WaitConn,
  cond: WaitCond,
  mut tasks: Vec<WaitTask>,
  mut events: UnboundedReceiver<WaitEvent>,
  deadline: Option<Instant>,
) {
  let WaitConn {
    mut reg,
    fd,
    hello,
    mut sender,
    mut receiver,
  } = conn;
  let set = WaitTasksEvent {
    tasks: tasks.iter().map(rpc_wait_task).collect(),
  };
  if sender.queue_ctl(event(EVENT_TASKS, &set)).is_err() {
    return;
  }
  let mut frozen = false;
  loop {
    let timed_out = async {
      match deadline {
        Some(deadline) => tokio::time::sleep_until(deadline).await,
        None => std::future::pending().await,
      }
    };
    // Biased: a verdict already in goes out before a timeout or a close
    // that comes with it.
    tokio::select! {
      biased;
      next = events.recv(), if !frozen => {
        let msg = match next {
          Some(WaitEvent::Changed(task)) => {
            let name = if task.removed {
              EVENT_TASK_REMOVED
            } else {
              EVENT_TASK_CHANGED
            };
            let msg = event(name, &rpc_wait_task(&task));
            if let Some(slot) =
              tasks.iter_mut().find(|t| t.info.id == task.info.id)
            {
              *slot = *task;
            }
            msg
          }
          Some(WaitEvent::Done {
            verdict,
            tasks,
            quitting,
          }) => {
            let done = done_event(outcome(verdict), &tasks, quitting);
            if sender.queue_ctl(done).is_ok() {
              break;
            }
            return;
          }
          // A second `Set` never comes.
          Some(WaitEvent::Set(_)) => continue,
          // The kernel has stopped without a verdict.
          None => break,
        };
        if sender.queue_ctl(msg).is_err() {
          return;
        }
      }
      ctl = reg.ctl.recv() => match ctl {
        Some(ConnCtl::Freeze(reply)) => {
          frozen = true;
          let kind = snap::ConnectionKind::Wait {
            // A removed task's standing is final: it isn't waited on
            // again.
            tasks: tasks
              .iter()
              .filter(|task| !task.removed)
              .map(|task| task.info.id.0)
              .collect(),
            cond: cond.into(),
            timeout_ms: deadline.map(|deadline| {
              deadline.saturating_duration_since(Instant::now()).as_millis()
                as u64
            }),
          };
          let _ = reply.send(carried(
            fd,
            Some(&hello),
            receiver.buffered(),
            &sender,
            kind,
          ));
        }
        Some(ConnCtl::Thaw) => frozen = false,
        Some(ConnCtl::Close) | None => {
          // The runner is exiting: a verdict it sent last still goes out.
          while let Ok(next) = events.try_recv() {
            if let WaitEvent::Done {
              verdict,
              tasks,
              quitting,
            } = next
            {
              let done = done_event(outcome(verdict), &tasks, quitting);
              let _ = sender.queue_ctl(done);
            }
          }
          break;
        }
      },
      () = timed_out, if !frozen => {
        // Dropping the receiver ends the kernel's wait.
        let done = done_event(WaitOutcome::Timeout, &tasks, false);
        if sender.queue_ctl(done).is_ok() {
          break;
        }
        return;
      }
      written = sender.flush(), if !frozen && !sender.pending().is_empty() => {
        if written.is_err() {
          return;
        }
      }
      // Read only to see the client go: nothing it sends changes a wait.
      msg = receiver.recv_ctl(), if !frozen => {
        if msg.is_err() {
          return;
        }
      }
    }
  }
  drop(events);
  finish(
    &mut sender,
    &mut reg.ctl,
    fd,
    Some(&hello),
    receiver.buffered(),
  )
  .await;
}

fn event<T: serde::Serialize>(name: &str, params: &T) -> CtlMsg {
  CtlMsg::Event(Event {
    name: name.to_string(),
    params: serde_json::to_value(params).expect("event params serialize"),
  })
}

fn outcome(verdict: WaitVerdict) -> WaitOutcome {
  match verdict {
    WaitVerdict::Met => WaitOutcome::Met,
    WaitVerdict::Failed => WaitOutcome::Failed,
  }
}

/// The session's last event.
fn done_event(wait: WaitOutcome, tasks: &[WaitTask], quitting: bool) -> CtlMsg {
  event(
    EVENT_WAIT_DONE,
    &WaitDoneEvent {
      wait,
      tasks: tasks.iter().map(rpc_wait_task).collect(),
      quitting,
    },
  )
}

pub(crate) fn rpc_wait_task(task: &WaitTask) -> RpcWaitTask {
  RpcWaitTask {
    info: rpc_task_info(&task.info),
    wait: task.mark.into(),
    blocked_by: task.blocked_by.clone(),
    removed: task.removed,
    why: task.why.clone().map(rpc_why),
  }
}
