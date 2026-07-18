use serde_yaml::Value;
use tokio::io::AsyncReadExt;
use tokio::net::TcpListener;

use crate::kernel::kernel_message::TaskSender;
use crate::mprocs::config::{Config, ServerConfig};
use crate::mprocs::event::AppEvent;

pub async fn run_ctl(ctl: &str, config: &Config) -> anyhow::Result<()> {
  let event: AppEvent = match serde_yaml::from_str(ctl) {
    Ok(event) => event,
    Err(err) => {
      let val: Value = serde_yaml::from_str(ctl)?;
      println!(
        "Remote command parsed as:\n{}",
        serde_yaml::to_string(&val)?
      );
      return Err(err.into());
    }
  };

  let socket = match &config.server {
    Some(ServerConfig::Tcp(addr)) => std::net::TcpStream::connect(addr)?,
    None => anyhow::bail!("Server address is not defined."),
  };

  serde_yaml::to_writer(socket, &event).unwrap();

  Ok(())
}

/// Executes commands received from `run_ctl`: one yaml-encoded AppEvent per
/// connection.
pub async fn run_ctl_server(listener: TcpListener, app_sender: TaskSender) {
  loop {
    let (mut socket, _addr) = match listener.accept().await {
      Ok(conn) => conn,
      Err(err) => {
        log::warn!("Ctl server failed to accept a connection: {}", err);
        continue;
      }
    };
    let mut buf = Vec::new();
    if let Err(err) = socket.read_to_end(&mut buf).await {
      log::warn!("Ctl server failed to read a command: {}", err);
      continue;
    }
    match serde_yaml::from_slice::<AppEvent>(&buf) {
      Ok(event) => app_sender.send(event.to_action()),
      Err(err) => log::warn!("Ctl server received a bad command: {}", err),
    }
  }
}
