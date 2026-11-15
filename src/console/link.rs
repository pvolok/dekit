//! The console's line to a runner: requests on one connection, answered
//! as they finish, and a connection of its own for each session (the
//! task list it watches, the screen it shows). A runner too old to watch
//! has its list read again every so often instead.

use std::collections::HashMap;
use std::sync::Arc;

use futures::future::BoxFuture;
use serde_json::Value;
use tokio::sync::{mpsc, oneshot};

use crate::kernel::task::TaskId;
use crate::protocol::{
  ConnReceiver, ConnSender, CtlMsg, Request, RpcError, RpcRequest,
  client_handshake_with, codes,
  ctl::Hello,
  rpc::{
    EVENT_CONFIG_CHANGED, EVENT_FOLDER, EVENT_TASK, EVENT_TASK_REMOVED,
    EVENT_TASKS, FEATURE_WATCH, RpcFolder, RpcListed, TaskRemovedEvent,
    WatchTasksEvent,
  },
};
use crate::runner::RunnerSpec;

/// Opens a connection to the runner, not said hello on yet.
pub type Connector = Arc<
  dyn Fn() -> BoxFuture<'static, anyhow::Result<(ConnSender, ConnReceiver)>>
    + Send
    + Sync,
>;

/// A connector to a runner's socket, which it does not start.
pub fn socket_connector(runner: RunnerSpec) -> Connector {
  Arc::new(move || {
    let runner = runner.clone();
    Box::pin(async move {
      crate::runner::socket::connect_client_socket(&runner, false).await
    })
  })
}

type Reply = oneshot::Sender<anyhow::Result<Value>>;

#[derive(Clone)]
pub struct Link {
  inner: Arc<Inner>,
}

struct Inner {
  /// None for the runner `dekit mprocs` runs in itself.
  runner: Option<RunnerSpec>,
  connect: Connector,
  hello: Hello,
  requests: mpsc::UnboundedSender<(RpcRequest, Reply)>,
}

impl Link {
  /// Connects, and serves requests until the link is dropped.
  pub async fn open(
    runner: Option<RunnerSpec>,
    connect: Connector,
  ) -> anyhow::Result<Link> {
    let conn = connect().await?;
    Link::open_with(runner, connect, conn).await
  }

  /// `open`, with the first connection made already.
  pub async fn open_with(
    runner: Option<RunnerSpec>,
    connect: Connector,
    (mut sender, mut receiver): (ConnSender, ConnReceiver),
  ) -> anyhow::Result<Link> {
    let hello =
      client_handshake_with(&mut sender, &mut receiver, Vec::new()).await?;
    if !hello.has(FEATURE_WATCH) {
      anyhow::bail!(
        "the runner is dekit {}, older than this console: upgrade it with \
         `dekit runner upgrade`",
        hello.version
      );
    }
    let (requests, rx) = mpsc::unbounded_channel();
    tokio::spawn(serve_requests(sender, receiver, rx));
    Ok(Link {
      inner: Arc::new(Inner {
        runner,
        connect,
        hello,
        requests,
      }),
    })
  }

  pub fn runner(&self) -> Option<&RunnerSpec> {
    self.inner.runner.as_ref()
  }

  /// What the runner said of itself.
  pub fn hello(&self) -> &Hello {
    &self.inner.hello
  }

  /// The result of a request; the error keeps the runner's code.
  pub fn request(
    &self,
    request: RpcRequest,
  ) -> impl Future<Output = anyhow::Result<Value>> + Send + 'static {
    let (tx, rx) = oneshot::channel();
    let sent = self.inner.requests.send((request, tx)).is_ok();
    async move {
      match sent {
        true => rx.await.unwrap_or_else(|_| Err(lost())),
        false => Err(lost()),
      }
    }
  }

  /// A connection of its own for a session, said hello on with what the
  /// client does.
  pub fn session(
    &self,
    features: Vec<String>,
  ) -> impl Future<Output = anyhow::Result<(ConnSender, ConnReceiver, Hello)>>
  + Send
  + 'static {
    let connect = self.inner.connect.clone();
    async move { handshake(&connect, features).await }
  }

  /// Follows the runner's tasks, telling `events` of each change, until
  /// `events` is gone or the runner is.
  pub fn watch(&self, events: impl Fn(RunnerEvent) + Send + Sync + 'static) {
    let link = self.clone();
    tokio::spawn(async move {
      // A runner being replaced ends the watch: the next image is
      // watched in its place.
      let why = loop {
        match watch(&link, &events).await {
          Ok(true) => continue,
          ended => break ended.map(drop),
        }
      };
      events(RunnerEvent::Lost(why.err().map(|err| format!("{err:#}"))));
    });
  }
}

fn lost() -> anyhow::Error {
  anyhow::anyhow!("lost the connection to the runner")
}

async fn handshake(
  connect: &Connector,
  features: Vec<String>,
) -> anyhow::Result<(ConnSender, ConnReceiver, Hello)> {
  let (mut sender, mut receiver) = connect().await?;
  let hello =
    client_handshake_with(&mut sender, &mut receiver, features).await?;
  Ok((sender, receiver, hello))
}

async fn serve_requests(
  mut sender: ConnSender,
  mut receiver: ConnReceiver,
  mut requests: mpsc::UnboundedReceiver<(RpcRequest, Reply)>,
) {
  let mut pending: HashMap<u64, Reply> = HashMap::new();
  let mut next = 1;
  loop {
    tokio::select! {
      request = requests.recv() => {
        let Some((request, reply)) = request else { break };
        let id = next;
        next += 1;
        let (method, params) = request.to_wire();
        let queued =
          sender.queue_ctl(CtlMsg::Request(Request { id, method, params }));
        match queued {
          Ok(()) => {
            pending.insert(id, reply);
          }
          Err(err) => {
            let _ = reply.send(Err(err));
          }
        }
      }
      written = sender.flush(), if !sender.pending().is_empty() => {
        if written.is_err() {
          break;
        }
      }
      msg = receiver.recv_ctl() => match msg {
        Ok(CtlMsg::Response(response)) => {
          if let Some(reply) = pending.remove(&response.id) {
            let result = match response.error {
              Some(error) => Err(error.into()),
              None => Ok(response.result.unwrap_or(Value::Null)),
            };
            let _ = reply.send(result);
          }
        }
        Ok(CtlMsg::Bye(_)) | Err(_) => break,
        Ok(_) => (),
      },
    }
  }
}

/// What a runner's list does, as a watch reports it, or as reading it
/// again finds.
#[derive(Debug)]
pub enum RunnerEvent {
  /// The list as it is: in place of whatever was known of it.
  Tasks(WatchTasksEvent),
  /// A task, new or as it is now, and where it is listed.
  Task(RpcListed),
  /// A folder, new or listed elsewhere.
  Folder(RpcFolder),
  Removed(TaskId),
  /// What the runner's config is read from changed.
  ConfigChanged,
  /// The runner is gone, or can't be followed: why, unless it quit.
  Lost(Option<String>),
}

/// Follows a runner until it quits (false) or is being upgraded (true).
async fn watch(
  link: &Link,
  events: &impl Fn(RunnerEvent),
) -> anyhow::Result<bool> {
  let (mut sender, mut receiver, _) = link.session(Vec::new()).await?;
  let (method, params) = RpcRequest::Watch { target: None }.to_wire();
  sender
    .send_ctl(CtlMsg::Request(Request {
      id: 1,
      method,
      params,
    }))
    .await?;
  loop {
    let event = match receiver.recv_ctl().await? {
      CtlMsg::Response(response) => match response.error {
        Some(error) => return Err(error.into()),
        None => continue,
      },
      CtlMsg::Bye(bye) if bye.code == codes::QUIT => return Ok(false),
      CtlMsg::Bye(bye) if bye.code == codes::UPGRADE => return Ok(true),
      CtlMsg::Bye(bye) => {
        anyhow::bail!("the runner ended the watch: {}", bye.code)
      }
      CtlMsg::Event(event) => event,
      CtlMsg::Hello(_) | CtlMsg::Request(_) => continue,
    };
    let params = event.params;
    let parsed = match event.name.as_str() {
      EVENT_TASKS => serde_json::from_value(params).map(RunnerEvent::Tasks),
      EVENT_TASK => serde_json::from_value(params).map(RunnerEvent::Task),
      EVENT_FOLDER => serde_json::from_value(params).map(RunnerEvent::Folder),
      EVENT_TASK_REMOVED => serde_json::from_value(params)
        .map(|removed: TaskRemovedEvent| RunnerEvent::Removed(removed.id)),
      EVENT_CONFIG_CHANGED => Ok(RunnerEvent::ConfigChanged),
      _ => continue,
    };
    match parsed {
      Ok(event) => events(event),
      Err(err) => log::debug!("dropping a watch event: {err}"),
    }
  }
}

/// Whether a request failed with the runner's error `code`.
pub fn is_code(err: &anyhow::Error, code: &str) -> bool {
  err
    .downcast_ref::<RpcError>()
    .is_some_and(|error| error.code == code)
}
