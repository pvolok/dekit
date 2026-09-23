use anyhow::bail;
use bytes::{Bytes, BytesMut};
use futures::SinkExt;
use serde_json::Value;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite};
use tokio_util::codec::{Decoder, Encoder, FramedWrite};

use crate::protocol::ctl::{
  Bye, CtlMsg, Hello, PROTOCOL_VERSION, codes, local_hello,
};
use crate::protocol::wire::{FrameCodec, KIND_CTL, KIND_OUT, RawFrame};

type DynWrite = dyn AsyncWrite + Unpin + Send + 'static;
type DynRead = dyn AsyncRead + Unpin + Send + 'static;

#[derive(Debug)]
pub enum Msg {
  Ctl(CtlMsg),
  Out(Bytes),
}

pub struct ConnSender {
  writer: FramedWrite<Box<DynWrite>, FrameCodec>,
}

impl ConnSender {
  pub fn new<W: AsyncWrite + Unpin + Send + 'static>(write: W) -> Self {
    Self::with_pending(write, &[])
  }

  /// Continues a connection inherited across an upgrade: the bytes the
  /// previous image had not written yet go out first.
  pub fn with_pending<W: AsyncWrite + Unpin + Send + 'static>(
    write: W,
    pending: &[u8],
  ) -> Self {
    let write: Box<DynWrite> = Box::new(write);
    let mut writer = FramedWrite::new(write, FrameCodec);
    writer.write_buffer_mut().extend_from_slice(pending);
    ConnSender { writer }
  }

  /// Queued but not written yet.
  pub fn pending(&self) -> &[u8] {
    self.writer.write_buffer()
  }

  /// Adds a message to the queue; `flush` writes it.
  pub fn queue_ctl(&mut self, msg: CtlMsg) -> anyhow::Result<()> {
    let payload = serde_json::to_vec(&msg)?;
    self.queue(RawFrame {
      kind: KIND_CTL,
      payload: Bytes::from(payload),
    })
  }

  pub fn queue_out(&mut self, bytes: Bytes) -> anyhow::Result<()> {
    self.queue(RawFrame {
      kind: KIND_OUT,
      payload: bytes,
    })
  }

  fn queue(&mut self, frame: RawFrame) -> anyhow::Result<()> {
    FrameCodec.encode(frame, self.writer.write_buffer_mut())?;
    Ok(())
  }

  /// Writes the queue. Cancel-safe: whatever was written has left the
  /// queue and the rest stays, so a caller can stop waiting at any time.
  pub async fn flush(&mut self) -> anyhow::Result<()> {
    SinkExt::<RawFrame>::flush(&mut self.writer).await?;
    Ok(())
  }

  pub async fn send_ctl(&mut self, msg: CtlMsg) -> anyhow::Result<()> {
    self.queue_ctl(msg)?;
    self.flush().await
  }

  pub async fn send_out(&mut self, bytes: Bytes) -> anyhow::Result<()> {
    self.queue_out(bytes)?;
    self.flush().await
  }
}

const READ_CAPACITY: usize = 8 * 1024;

pub struct ConnReceiver {
  read: Box<DynRead>,
  /// Read but not yet decoded; starts at a frame boundary.
  buf: BytesMut,
}

impl ConnReceiver {
  pub fn new<R: AsyncRead + Unpin + Send + 'static>(read: R) -> Self {
    Self::with_buffered(read, &[])
  }

  /// Bytes read from the transport but not yet decoded into a frame.
  pub fn buffered(&self) -> &[u8] {
    &self.buf
  }

  /// Continues a connection whose transport was inherited with bytes
  /// already read by the previous image.
  pub fn with_buffered<R: AsyncRead + Unpin + Send + 'static>(
    read: R,
    buffered: &[u8],
  ) -> Self {
    let mut buf = BytesMut::with_capacity(READ_CAPACITY);
    buf.extend_from_slice(buffered);
    ConnReceiver {
      read: Box::new(read),
      buf,
    }
  }

  /// Decodes what is buffered before reading more. Cancel-safe: a read
  /// either lands in the buffer or does not happen.
  async fn next_frame(&mut self) -> Option<std::io::Result<RawFrame>> {
    loop {
      match FrameCodec.decode(&mut self.buf) {
        Ok(Some(frame)) => return Some(Ok(frame)),
        Ok(None) => (),
        Err(err) => return Some(Err(err)),
      }
      // Room to read into: a read of zero bytes would look like EOF.
      self.buf.reserve(1);
      match self.read.read_buf(&mut self.buf).await {
        Ok(0) if self.buf.is_empty() => return None,
        Ok(0) => {
          return Some(Err(std::io::Error::new(
            std::io::ErrorKind::UnexpectedEof,
            "connection closed in the middle of a frame",
          )));
        }
        Ok(_) => (),
        Err(err) => return Some(Err(err)),
      }
    }
  }

  /// Next message. `None` means the connection closed; `Some(Err(_))`
  /// means a protocol error and the connection must be dropped.
  /// Unknown frame kinds and unknown control message types are skipped.
  pub async fn recv(&mut self) -> Option<anyhow::Result<Msg>> {
    loop {
      let frame = match self.next_frame().await? {
        Ok(frame) => frame,
        Err(err) => return Some(Err(err.into())),
      };
      match frame.kind {
        KIND_CTL => {
          let value: Value = match serde_json::from_slice(&frame.payload) {
            Ok(value) => value,
            Err(err) => {
              return Some(Err(anyhow::anyhow!(
                "invalid control frame: {err}"
              )));
            }
          };
          match serde_json::from_value::<CtlMsg>(value.clone()) {
            Ok(msg) => return Some(Ok(Msg::Ctl(msg))),
            Err(err) => {
              // Unknown message types are future protocol additions;
              // a malformed known type is a peer bug.
              let unknown_type =
                value.get("type").and_then(|t| t.as_str()).is_some_and(|t| {
                  let known = ["hello", "request", "response", "event", "bye"];
                  !known.contains(&t)
                });
              if unknown_type {
                log::debug!("skipping unknown control message: {value}");
                continue;
              }
              return Some(Err(anyhow::anyhow!(
                "malformed control message: {err}"
              )));
            }
          }
        }
        KIND_OUT => return Some(Ok(Msg::Out(frame.payload))),
        kind => {
          log::debug!("skipping unknown frame kind {kind}");
          continue;
        }
      }
    }
  }

  pub async fn recv_ctl(&mut self) -> anyhow::Result<CtlMsg> {
    loop {
      match self.recv().await {
        Some(Ok(Msg::Ctl(msg))) => return Ok(msg),
        Some(Ok(Msg::Out(_))) => {
          bail!("`recv_ctl` got OUT frame");
        }
        Some(Err(err)) => return Err(err),
        None => bail!("connection closed"),
      }
    }
  }
}

pub async fn client_handshake(
  sender: &mut ConnSender,
  receiver: &mut ConnReceiver,
) -> anyhow::Result<Hello> {
  sender.send_ctl(CtlMsg::Hello(local_hello())).await?;
  match receiver.recv_ctl().await? {
    CtlMsg::Hello(hello) => {
      if hello.protocol != PROTOCOL_VERSION {
        bail!(
          "runner ({}) speaks protocol {}, this binary speaks {}; \
           restart it with `dekit runner stop && dekit up`",
          hello.app,
          hello.protocol,
          PROTOCOL_VERSION,
        );
      }
      Ok(hello)
    }
    CtlMsg::Bye(bye) => bail!("runner refused connection: {}", bye_text(&bye)),
    msg => bail!("expected hello from runner, got {msg:?}"),
  }
}

/// The runner's answer to a client's hello: its own hello, or a bye when
/// they speak different protocols.
pub fn server_hello(client: &Hello) -> Result<Hello, Bye> {
  if client.protocol != PROTOCOL_VERSION {
    return Err(Bye {
      code: codes::UNSUPPORTED_PROTOCOL.to_string(),
      message: format!(
        "runner speaks protocol {}, client ({}) speaks {}",
        PROTOCOL_VERSION, client.app, client.protocol,
      ),
      state: None,
      screen: None,
    });
  }
  Ok(local_hello())
}

fn bye_text(bye: &Bye) -> String {
  if bye.message.is_empty() {
    bye.code.clone()
  } else {
    format!("{} ({})", bye.message, bye.code)
  }
}

#[cfg(test)]
mod tests {
  use bytes::BytesMut;
  use tokio::io::AsyncWriteExt;
  use tokio_util::codec::Encoder;

  use super::*;
  use crate::protocol::ctl::{Request, RpcError};
  use crate::protocol::rpc::RpcRequest;
  use crate::target::Target;

  fn pair() -> (ConnSender, ConnReceiver, ConnSender, ConnReceiver) {
    let (client, server) = tokio::io::duplex(64 * 1024);
    let (client_read, client_write) = tokio::io::split(client);
    let (server_read, server_write) = tokio::io::split(server);
    (
      ConnSender::new(client_write),
      ConnReceiver::new(client_read),
      ConnSender::new(server_write),
      ConnReceiver::new(server_read),
    )
  }

  async fn raw_pair(frames: Vec<RawFrame>) -> ConnReceiver {
    let (client, server) = tokio::io::duplex(64 * 1024);
    let (_client_read, mut client_write) = tokio::io::split(client);
    let (server_read, _server_write) = tokio::io::split(server);
    let mut buf = BytesMut::new();
    let mut codec = FrameCodec;
    for frame in frames {
      codec.encode(frame, &mut buf).unwrap();
    }
    client_write.write_all(&buf).await.unwrap();
    client_write.shutdown().await.unwrap();
    ConnReceiver::new(server_read)
  }

  fn ctl_frame(json: &str) -> RawFrame {
    RawFrame {
      kind: KIND_CTL,
      payload: Bytes::copy_from_slice(json.as_bytes()),
    }
  }

  #[test]
  fn server_hello_answers_the_same_protocol() {
    let ours = server_hello(&local_hello()).unwrap();
    assert_eq!(ours.protocol, PROTOCOL_VERSION);
    assert!(ours.app.starts_with("dekit "));
  }

  #[test]
  fn server_hello_refuses_another_protocol_with_bye() {
    let bye = server_hello(&Hello {
      protocol: 999,
      version: "99.0.0".to_string(),
      app: "dekit future".to_string(),
      features: vec![],
    })
    .unwrap_err();
    assert_eq!(bye.code, codes::UNSUPPORTED_PROTOCOL);
  }

  #[tokio::test]
  async fn pending_bytes_go_out_before_new_frames() {
    let mut carried = BytesMut::new();
    FrameCodec
      .encode(ctl_frame(r#"{"type":"bye","code":"one"}"#), &mut carried)
      .unwrap();
    let (client, server) = tokio::io::duplex(64 * 1024);
    let (client_read, _client_write) = tokio::io::split(client);
    let (_server_read, server_write) = tokio::io::split(server);
    // Half a frame was written by the previous image; the rest is owed.
    let mut sender = ConnSender::with_pending(server_write, &carried[5..]);
    let mut receiver = ConnReceiver::with_buffered(client_read, &carried[..5]);
    sender.queue_out(Bytes::from_static(b"after")).unwrap();
    sender.flush().await.unwrap();
    assert!(sender.pending().is_empty());
    match receiver.recv().await.unwrap().unwrap() {
      Msg::Ctl(CtlMsg::Bye(bye)) => assert_eq!(bye.code, "one"),
      msg => panic!("expected the carried bye, got {msg:?}"),
    }
    match receiver.recv().await.unwrap().unwrap() {
      Msg::Out(bytes) => assert_eq!(bytes, Bytes::from_static(b"after")),
      msg => panic!("expected the new frame, got {msg:?}"),
    }
  }

  #[tokio::test]
  async fn client_rejects_mismatched_server() {
    let (mut cs, mut cr, mut ss, mut sr) = pair();
    let server = tokio::spawn(async move {
      match sr.recv_ctl().await.unwrap() {
        CtlMsg::Hello(_) => (),
        msg => panic!("expected hello, got {msg:?}"),
      }
      ss.send_ctl(CtlMsg::Hello(Hello {
        protocol: 999,
        version: "99.0.0".to_string(),
        app: "dekit future".to_string(),
        features: vec![],
      }))
      .await
      .unwrap();
    });
    let err = client_handshake(&mut cs, &mut cr).await.unwrap_err();
    assert!(err.to_string().contains("protocol 999"), "{err}");
    server.await.unwrap();
  }

  #[tokio::test]
  async fn rpc_round_trip() {
    let (mut cs, mut cr, mut ss, mut sr) = pair();
    let server = tokio::spawn(async move {
      let request = match sr.recv_ctl().await.unwrap() {
        CtlMsg::Request(request) => request,
        msg => panic!("expected request, got {msg:?}"),
      };
      let req = RpcRequest::from_wire(&request.method, request.params).unwrap();
      assert_eq!(
        req,
        RpcRequest::Why {
          target: Target::glob("web")
        }
      );
      ss.send_ctl(CtlMsg::ok(request.id, serde_json::json!({})))
        .await
        .unwrap();
    });
    let (method, params) = RpcRequest::Why {
      target: Target::glob("web"),
    }
    .to_wire();
    cs.send_ctl(CtlMsg::Request(Request {
      id: 7,
      method: method.to_string(),
      params,
    }))
    .await
    .unwrap();
    match cr.recv_ctl().await.unwrap() {
      CtlMsg::Response(response) => {
        assert_eq!(response.id, 7);
        assert_eq!(response.error, None);
      }
      msg => panic!("expected response, got {msg:?}"),
    }
    server.await.unwrap();
  }

  #[tokio::test]
  async fn error_response_round_trips() {
    let (mut cs, _cr, _ss, mut sr) = pair();
    cs.send_ctl(CtlMsg::err(3, RpcError::new(codes::NO_MATCH, "nope")))
      .await
      .unwrap();
    match sr.recv_ctl().await.unwrap() {
      CtlMsg::Response(response) => {
        let error = response.error.unwrap();
        assert_eq!(error.code, codes::NO_MATCH);
        assert_eq!(error.message, "nope");
      }
      msg => panic!("expected response, got {msg:?}"),
    }
  }

  #[tokio::test]
  async fn out_frames_pass_through_unescaped() {
    let (mut cs, _cr, _ss, mut sr) = pair();
    let ansi = Bytes::from_static(b"\x1b[2J\x1b[Hhello \xf0\x9f\x91\x8b");
    cs.send_out(ansi.clone()).await.unwrap();
    match sr.recv().await.unwrap().unwrap() {
      Msg::Out(bytes) => assert_eq!(bytes, ansi),
      msg => panic!("expected out frame, got {msg:?}"),
    }
  }

  #[tokio::test]
  async fn unknown_message_type_is_skipped() {
    let mut receiver = raw_pair(vec![
      ctl_frame(r#"{"type":"hologram","data":[1,2,3]}"#),
      ctl_frame(r#"{"type":"bye","code":"quit"}"#),
    ])
    .await;
    match receiver.recv().await.unwrap().unwrap() {
      Msg::Ctl(CtlMsg::Bye(bye)) => assert_eq!(bye.code, "quit"),
      msg => panic!("expected bye, got {msg:?}"),
    }
  }

  #[tokio::test]
  async fn unknown_frame_kind_is_skipped() {
    let mut receiver = raw_pair(vec![
      RawFrame {
        kind: 0x42,
        payload: Bytes::from_static(b"future data"),
      },
      ctl_frame(r#"{"type":"bye","code":"quit"}"#),
    ])
    .await;
    match receiver.recv().await.unwrap().unwrap() {
      Msg::Ctl(CtlMsg::Bye(bye)) => assert_eq!(bye.code, "quit"),
      msg => panic!("expected bye, got {msg:?}"),
    }
  }

  #[tokio::test]
  async fn malformed_known_message_is_fatal() {
    let mut receiver =
      raw_pair(vec![ctl_frame(r#"{"type":"request","id":"abc"}"#)]).await;
    assert!(receiver.recv().await.unwrap().is_err());
  }

  #[tokio::test]
  async fn invalid_json_is_fatal() {
    let mut receiver = raw_pair(vec![ctl_frame(r#"{"type": "#)]).await;
    assert!(receiver.recv().await.unwrap().is_err());
  }

  #[tokio::test]
  async fn close_yields_none() {
    let mut receiver = raw_pair(vec![]).await;
    assert!(receiver.recv().await.is_none());
  }

  fn encoded(json: &str) -> BytesMut {
    let mut buf = BytesMut::new();
    FrameCodec.encode(ctl_frame(json), &mut buf).unwrap();
    buf
  }

  fn bye_code(msg: Option<anyhow::Result<Msg>>) -> String {
    match msg {
      Some(Ok(Msg::Ctl(CtlMsg::Bye(bye)))) => bye.code,
      msg => panic!("expected bye, got {msg:?}"),
    }
  }

  #[tokio::test]
  async fn carried_frames_are_decoded_before_reading() {
    use futures::FutureExt;

    let third = encoded(r#"{"type":"bye","code":"three"}"#);
    let mut carried = encoded(r#"{"type":"bye","code":"one"}"#);
    carried.extend_from_slice(&encoded(r#"{"type":"bye","code":"two"}"#));
    carried.extend_from_slice(&third[..6]);
    let (client, server) = tokio::io::duplex(64 * 1024);
    let (_client_read, mut client_write) = tokio::io::split(client);
    let (server_read, _server_write) = tokio::io::split(server);
    let mut receiver = ConnReceiver::with_buffered(server_read, &carried);

    // Nothing new on the socket: both complete frames are ready at once.
    assert_eq!(bye_code(receiver.recv().now_or_never().unwrap()), "one");
    assert_eq!(bye_code(receiver.recv().now_or_never().unwrap()), "two");
    assert!(receiver.recv().now_or_never().is_none());
    assert_eq!(receiver.buffered(), &third[..6]);

    // The partial frame continues with the bytes the peer sends next.
    client_write.write_all(&third[6..]).await.unwrap();
    assert_eq!(bye_code(receiver.recv().await), "three");
  }

  #[tokio::test]
  async fn partial_frame_is_buffered_from_its_header() {
    use futures::FutureExt;

    let frame = encoded(r#"{"type":"bye","code":"quit"}"#);
    let (client, server) = tokio::io::duplex(64 * 1024);
    let (_client_read, mut client_write) = tokio::io::split(client);
    let (server_read, _server_write) = tokio::io::split(server);
    let mut receiver = ConnReceiver::new(server_read);
    client_write.write_all(&frame[..6]).await.unwrap();

    assert!(receiver.recv().now_or_never().is_none());
    // What another image would carry over: the frame from its start.
    assert_eq!(receiver.buffered(), &frame[..6]);
  }
}
