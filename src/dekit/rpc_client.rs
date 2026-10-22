use anyhow::bail;
use serde_json::Value;

use crate::protocol::{
  ConnReceiver, ConnSender, CtlMsg, Request, RpcRequest, client_handshake,
  ctl::Hello,
};
use crate::runner::{RunnerSpec, socket::connect_client_socket};

/// A connection to a runner that takes requests one after another.
pub struct RpcConn {
  sender: ConnSender,
  receiver: ConnReceiver,
  /// What the runner said of itself.
  hello: Hello,
  /// The id of the next request.
  next: u64,
}

impl RpcConn {
  /// Connects and says hello. `start_runner` starts a runner that isn't
  /// running.
  pub async fn connect(
    runner: &RunnerSpec,
    start_runner: bool,
  ) -> anyhow::Result<Self> {
    let (mut sender, mut receiver) =
      connect_client_socket(runner, start_runner).await?;
    let hello = client_handshake(&mut sender, &mut receiver).await?;
    Ok(RpcConn {
      sender,
      receiver,
      hello,
      next: 1,
    })
  }

  /// The runner's hello: its app, version, and features.
  pub fn hello(&self) -> &Hello {
    &self.hello
  }

  /// Sends a request and returns its result.
  pub async fn request(&mut self, req: RpcRequest) -> anyhow::Result<Value> {
    let id = self.next;
    self.next += 1;
    let (method, params) = req.to_wire();
    self
      .sender
      .send_ctl(CtlMsg::Request(Request { id, method, params }))
      .await?;
    loop {
      match self.receiver.recv_ctl().await? {
        CtlMsg::Response(response) => {
          if response.id != id {
            continue;
          }
          // The error keeps its code: callers can downcast to it.
          match response.error {
            Some(error) => return Err(error.into()),
            None => return Ok(response.result.unwrap_or(Value::Null)),
          }
        }
        CtlMsg::Bye(bye) => {
          bail!("runner closed the connection: {}", bye.code)
        }
        msg @ (CtlMsg::Hello(_) | CtlMsg::Request(_) | CtlMsg::Event(_)) => {
          log::debug!("ignoring runner message {msg:?}");
        }
      }
    }
  }

  /// The next message from the runner, for the session a request began
  /// (a `wait`): its events, and the bye that ends it.
  pub async fn recv_ctl(&mut self) -> anyhow::Result<CtlMsg> {
    self.receiver.recv_ctl().await
  }
}

/// One request on a connection of its own.
pub async fn rpc_request(
  runner: &RunnerSpec,
  req: RpcRequest,
  start_runner: bool,
) -> anyhow::Result<Value> {
  RpcConn::connect(runner, start_runner)
    .await?
    .request(req)
    .await
}
