//! The `watch` session: `ls` kept current. Like a wait, it is the last
//! request on its connection: the ok response, the selected tasks as a
//! `tasks` event, then an event for each change to them, and
//! `config-changed` whenever what the config is read from changes, until
//! the client hangs up or the runner quits.

use std::collections::HashSet;
use std::sync::Arc;

use tokio::sync::mpsc::UnboundedReceiver;

use crate::{
  dekit::server::{
    ConnCtl, ServerCtx, SessionConn, carried, event, finish, rpc_task_described,
  },
  kernel::{
    kernel_message::{Heard, SpaceSelector, TaskDescription, TaskSelector},
    sub_trie::SubMode,
    task::{TaskId, TaskNotify},
    task_key::{TaskKey, TaskSpaceId},
    task_path::TaskPath,
  },
  protocol::{
    Bye, CtlMsg, RpcError, codes, ok_result,
    rpc::{
      EVENT_CONFIG_CHANGED, EVENT_FOLDER, EVENT_TASK, EVENT_TASK_REMOVED,
      EVENT_TASKS, RpcFolder, RpcListed, TaskRemovedEvent, WatchTasksEvent,
    },
  },
  target::Target,
  upgrade::snapshot as snap,
};

/// How far a client may fall behind, in bytes of events not written yet,
/// before what is queued for it is dropped and it is sent `tasks` again.
const BEHIND: usize = 4 << 20;

pub async fn watch_session(
  ctx: &Arc<ServerCtx>,
  mut conn: SessionConn,
  request_id: u64,
  target: Option<Target>,
) {
  let target = target.unwrap_or_else(|| Target::glob("**"));
  match Watch::new(&target) {
    Ok(watch) => {
      if conn
        .sender
        .queue_ctl(CtlMsg::ok(request_id, ok_result()))
        .is_err()
      {
        return;
      }
      run_watch(ctx, conn, watch).await;
    }
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
    }
  }
}

/// What a watch selects.
struct Watch {
  selector: TaskSelector,
  space: TaskSpaceId,
  /// The selected tasks and the folders among them.
  known: HashSet<TaskId>,
}

impl Watch {
  fn new(target: &Target) -> Result<Watch, RpcError> {
    let bad = |message: String| RpcError::new(codes::BAD_TARGET, message);
    let selector = target.selector().map_err(|err| bad(err.to_string()))?;
    let space = match &selector {
      TaskSelector::Id(_) => TaskSpaceId::default_space(),
      TaskSelector::Glob(space, _) | TaskSelector::Tag(space, _) => match space
      {
        SpaceSelector::One(space) => space.clone(),
        SpaceSelector::Any => {
          return Err(bad("a watch is on the tasks of one space".into()));
        }
      },
    };
    Ok(Watch {
      selector,
      space,
      known: HashSet::new(),
    })
  }

  /// Whether a target selects every task of its space, folders with them:
  /// no task needs looking up.
  fn all(&self) -> bool {
    matches!(&self.selector, TaskSelector::Glob(_, glob) if glob == "**")
  }

  fn listen(&self, ctx: &ServerCtx) -> UnboundedReceiver<Heard> {
    let key = TaskKey::new(self.space.clone(), TaskPath::root());
    ctx.pc.listen(key, SubMode::Subtree)
  }

  /// The ids the target selects among `ids`: settled once, as each task
  /// comes.
  async fn selected(&self, ctx: &ServerCtx, ids: &[TaskId]) -> HashSet<TaskId> {
    if self.all() || ids.is_empty() {
      return ids.iter().copied().collect();
    }
    ctx
      .pc
      .list_tasks(self.selector.clone())
      .await
      .into_iter()
      .map(|task| task.id)
      .filter(|id| ids.contains(id))
      .collect()
  }
}

/// The tasks and folders a listen first heard, before `Caught`, as a
/// `tasks` event; `None` once the kernel is gone.
async fn caught(
  ctx: &ServerCtx,
  watch: &mut Watch,
  heard: &mut UnboundedReceiver<Heard>,
) -> Option<WatchTasksEvent> {
  let mut tasks = Vec::new();
  let mut folders = Vec::new();
  loop {
    match heard.recv().await? {
      Heard::Caught => break,
      Heard::Notify {
        from,
        notify: TaskNotify::Added { after, .. },
        now: Some(now),
        ..
      } => tasks.push((from, after, now)),
      Heard::Notify {
        from,
        notify:
          TaskNotify::FolderAdded {
            path,
            after,
            collapsed,
          },
        ..
      } => folders.push(RpcFolder {
        id: from,
        path: TaskKey::new(watch.space.clone(), path).to_string(),
        after,
        collapsed,
      }),
      Heard::Notify { .. } => (),
    }
  }
  let ids: Vec<TaskId> = tasks.iter().map(|(id, ..)| *id).collect();
  let selected = watch.selected(ctx, &ids).await;
  let tasks: Vec<RpcListed> = tasks
    .into_iter()
    .filter(|(id, ..)| selected.contains(id))
    .map(|(_, after, now)| RpcListed {
      info: rpc_task_described(&now),
      after,
    })
    .collect();
  // Folders are part of the list of every task, and of no selection.
  let folders = match watch.all() {
    true => folders,
    false => Vec::new(),
  };
  watch.known = (tasks.iter().map(|task| task.info.id))
    .chain(folders.iter().map(|folder| folder.id))
    .collect();
  Some(WatchTasksEvent { tasks, folders })
}

async fn run_watch(ctx: &Arc<ServerCtx>, conn: SessionConn, mut watch: Watch) {
  let SessionConn {
    mut reg,
    fd,
    hello,
    mut sender,
    mut receiver,
  } = conn;
  let mut config = ctx.project.changes();
  config.mark_unchanged();
  let mut heard = watch.listen(ctx);
  let mut frozen = false;
  // Behind by more than `BEHIND`: the events since are dropped, and the
  // client is sent `tasks` again once it has read what it was sent.
  let mut behind = false;
  let mut resync = true;
  loop {
    if resync && !frozen && sender.pending().is_empty() {
      resync = false;
      let Some(tasks) = caught(ctx, &mut watch, &mut heard).await else {
        break;
      };
      if sender.queue_ctl(event(EVENT_TASKS, &tasks)).is_err() {
        return;
      }
    }
    tokio::select! {
      next = heard.recv(), if !frozen && !resync => {
        let Some(next) = next else { break };
        if behind {
          continue;
        }
        let Some(msg) = notify_event(ctx, &mut watch, next).await else {
          continue;
        };
        if sender.queue_ctl(msg).is_err() {
          return;
        }
        if sender.pending().len() > BEHIND {
          behind = true;
        }
      }
      changed = config.changed(), if !frozen && !behind => {
        if changed.is_err() {
          // The project is gone: the runner is.
          break;
        }
        let msg = event(EVENT_CONFIG_CHANGED, &serde_json::json!({}));
        if sender.queue_ctl(msg).is_err() {
          return;
        }
      }
      ctl = reg.ctl.recv() => match ctl {
        Some(ConnCtl::Freeze(reply)) => {
          // Not carried: the session ends with a bye, which the next image
          // writes, on a connection it takes as a plain one, and the
          // client watches again. Any image can take that.
          let bye = Bye::new(codes::UPGRADE, "the runner is being upgraded");
          if sender.queue_ctl(CtlMsg::Bye(bye)).is_err() {
            return;
          }
          frozen = true;
          let kind = snap::ConnectionKind::Rpc { pending_upgrade: None };
          let _ = reply.send(carried(
            fd,
            Some(&hello),
            receiver.buffered(),
            &sender,
            kind,
          ));
        }
        // The upgrade failed: the bye is on its way already.
        Some(ConnCtl::Thaw) => break,
        Some(ConnCtl::Close) | None => {
          let bye = CtlMsg::Bye(Bye::new(codes::QUIT, "the runner stopped"));
          let _ = sender.queue_ctl(bye);
          break;
        }
      },
      written = sender.flush(), if !frozen && !sender.pending().is_empty() => {
        if written.is_err() {
          return;
        }
        if behind && sender.pending().is_empty() {
          // What it missed is in a fresh listen's account of the tasks.
          behind = false;
          resync = true;
          heard = watch.listen(ctx);
        }
      }
      // Read only to see the client go: nothing it sends changes a watch.
      msg = receiver.recv_ctl(), if !frozen => {
        if msg.is_err() {
          return;
        }
      }
    }
  }
  drop(heard);
  finish(
    &mut sender,
    &mut reg.ctl,
    fd,
    Some(&hello),
    receiver.buffered(),
  )
  .await;
}

/// The event for what a listen heard, if it is of a selected task: the
/// task or folder as it is now and where it is listed, or that it is
/// gone.
async fn notify_event(
  ctx: &ServerCtx,
  watch: &mut Watch,
  heard: Heard,
) -> Option<CtlMsg> {
  let Heard::Notify {
    from,
    notify,
    now,
    after,
  } = heard
  else {
    return None;
  };
  let folder = |path: TaskPath, collapsed: bool| {
    let folder = RpcFolder {
      id: from,
      path: TaskKey::new(watch.space.clone(), path).to_string(),
      after,
      collapsed,
    };
    event(EVENT_FOLDER, &folder)
  };
  let task = |now: &TaskDescription| {
    let listed = RpcListed {
      info: rpc_task_described(now),
      after,
    };
    event(EVENT_TASK, &listed)
  };
  match notify {
    TaskNotify::Added { .. } => {
      let now = now?;
      if !watch.selected(ctx, &[from]).await.contains(&from) {
        return None;
      }
      watch.known.insert(from);
      Some(task(&now))
    }
    TaskNotify::FolderAdded {
      path, collapsed, ..
    } => {
      if !watch.all() {
        return None;
      }
      watch.known.insert(from);
      Some(folder(path, collapsed))
    }
    TaskNotify::StateChanged(_)
    | TaskNotify::Changed { .. }
    | TaskNotify::Moved { .. }
      if !watch.known.contains(&from) =>
    {
      None
    }
    TaskNotify::StateChanged(_) | TaskNotify::Changed { .. } => {
      let now = now?;
      Some(task(&now))
    }
    // A folder has no task to describe.
    TaskNotify::Moved { path, .. } => match now {
      Some(now) => Some(task(&now)),
      None => Some(folder(path?, false)),
    },
    TaskNotify::Removed => {
      if !watch.known.remove(&from) {
        return None;
      }
      Some(event(EVENT_TASK_REMOVED, &TaskRemovedEvent { id: from }))
    }
  }
}
