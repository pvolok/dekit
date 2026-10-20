//! Ready checks: what makes a starting task count as ready.

use std::path::{Path, PathBuf};
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use crate::process::process_spec::ProcessSpec;

pub const DEFAULT_INTERVAL: Duration = Duration::from_secs(1);

#[derive(Clone, Debug)]
pub struct ReadyConfig {
  pub check: ReadyCheck,
  /// Not ready this long after starting fails the start (kernel side).
  pub timeout: Option<Duration>,
}

#[derive(Clone, Debug)]
pub enum ReadyCheck {
  /// A line of output contains this text.
  Log(String),
  /// Tried every `interval` until it succeeds.
  Probe { probe: Probe, interval: Duration },
  /// The program sends `READY=1` to the socket `NOTIFY_SOCKET` names, as
  /// it would to systemd.
  SdNotify,
}

#[derive(Clone, Debug)]
pub enum Probe {
  Tcp { host: Option<String>, port: u16 },
  Http(HttpUrl),
  Cmd { argv: Vec<String> },
  File { path: PathBuf },
}

/// `5432`, `host:5432`, or `[::1]:5432`.
pub fn parse_tcp(text: &str) -> Result<(Option<String>, u16), String> {
  let (host, port) = match text.parse::<u16>() {
    Ok(port) => (None, Ok(port)),
    Err(_) => {
      let (host, port) = split_host_port(text)
        .ok_or_else(|| format!("expected a port or host:port, got '{text}'"))?;
      (Some(host.to_string()), port.parse::<u16>())
    }
  };
  match port {
    Ok(port) if port != 0 => Ok((host, port)),
    _ => Err(format!("bad port in '{text}'")),
  }
}

/// `host:port` with an optional `[...]` around an IPv6 host.
fn split_host_port(text: &str) -> Option<(&str, &str)> {
  let (host, port) = match text.strip_prefix('[') {
    Some(rest) => {
      let (host, rest) = rest.split_once(']')?;
      (host, rest.strip_prefix(':')?)
    }
    None => text.rsplit_once(':')?,
  };
  if host.is_empty() || host.contains(':') && !text.starts_with('[') {
    return None;
  }
  Some((host, port))
}

#[derive(Clone, Debug)]
pub struct HttpUrl {
  /// The whole url as written.
  pub url: String,
  /// As written, brackets included; goes in the Host header.
  pub authority: String,
  pub host: String,
  pub port: u16,
  pub path: String,
}

pub fn parse_http_url(url: &str) -> Result<HttpUrl, String> {
  if url.starts_with("https://") {
    return Err("https is not supported; use an http:// url".to_string());
  }
  let Some(rest) = url.strip_prefix("http://") else {
    return Err(format!("expected an http:// url, got '{url}'"));
  };
  let rest = rest.split('#').next().unwrap_or_default();
  let split = rest.find(['/', '?']).unwrap_or(rest.len());
  let (authority, path) = rest.split_at(split);
  let path = match path.strip_prefix('?') {
    Some(_) => format!("/{path}"),
    None if path.is_empty() => "/".to_string(),
    None => path.to_string(),
  };
  if authority.contains('@') {
    return Err(format!("credentials are not supported in '{url}'"));
  }
  let (host, port) = match split_host_port(authority) {
    Some((host, port)) => match port.parse::<u16>() {
      Ok(port) if port != 0 => (host.to_string(), port),
      _ => return Err(format!("bad port in '{url}'")),
    },
    // No port: a plain host or a bracketed IPv6 one.
    None => {
      let host = match authority.strip_prefix('[') {
        Some(rest) => rest
          .strip_suffix(']')
          .filter(|host| !host.contains(['[', ']'])),
        None => Some(authority).filter(|host| !host.contains([':', ']'])),
      };
      match host {
        Some(host) => (host.to_string(), 80),
        None => return Err(format!("bad host or port in '{url}'")),
      }
    }
  };
  if host.is_empty() {
    return Err(format!("no host in '{url}'"));
  }
  Ok(HttpUrl {
    url: url.to_string(),
    authority: authority.to_string(),
    host,
    port,
    path,
  })
}

/// Whether a finished line, or the line so far, says the task is ready.
pub fn line_ready(needle: &str, line: &str) -> bool {
  !needle.is_empty() && line.contains(needle)
}

/// Whether an sd_notify datagram says the program is ready: one of its
/// newline-separated `KEY=VALUE` lines is `READY=1`.
pub fn sd_notify_ready(datagram: &[u8]) -> bool {
  datagram
    .split(|b| *b == b'\n')
    .any(|line| line == b"READY=1")
}

/// Reads sd_notify datagrams until one says the program is ready. A socket
/// that cannot be read leaves the task starting, for its timeout to end.
#[cfg(unix)]
pub async fn wait_sd_notify(socket: std::sync::Arc<tokio::net::UnixDatagram>) {
  // What systemd takes at most.
  let mut buf = [0u8; 4096];
  loop {
    match socket.recv(&mut buf).await {
      Ok(n) if sd_notify_ready(&buf[..n]) => return,
      Ok(_) => (),
      Err(err) => {
        log::warn!("Cannot read the sd_notify socket: {err}");
        std::future::pending::<()>().await;
      }
    }
  }
}

/// Tries `probe` until it succeeds: the first attempt at once, then
/// `interval` after each failed one. An attempt may take
/// `max(interval, 1s)`.
pub async fn wait_ready(probe: Probe, interval: Duration, spec: ProcessSpec) {
  let limit = interval.max(Duration::from_secs(1));
  let mut warned = false;
  loop {
    let ok = match &probe {
      Probe::Tcp { host, port } => {
        tokio::time::timeout(limit, connect(host.as_deref(), *port))
          .await
          .is_ok_and(|conn| conn.is_ok())
      }
      Probe::Http(url) => tokio::time::timeout(limit, http_ok(url))
        .await
        .unwrap_or(false),
      Probe::Cmd { argv } => match run_cmd(argv, &spec, limit).await {
        Ok(ok) => ok,
        Err(err) => {
          if !warned {
            log::warn!("Ready command {argv:?} cannot run: {err}");
            warned = true;
          }
          false
        }
      },
      Probe::File { path } => {
        let path = match &spec.cwd {
          Some(cwd) if path.is_relative() => Path::new(cwd).join(path),
          _ => path.clone(),
        };
        tokio::fs::try_exists(&path).await.unwrap_or(false)
      }
    };
    if ok {
      return;
    }
    tokio::time::sleep(interval).await;
  }
}

/// No host or `localhost` tries both loopbacks at once and takes the
/// first that connects: a refused connect can take seconds (about 2s on
/// Windows), which must not use up the attempt before the other is tried.
async fn connect(host: Option<&str>, port: u16) -> std::io::Result<TcpStream> {
  match host {
    None | Some("localhost") => {
      let v4 = TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, port));
      let v6 = TcpStream::connect((std::net::Ipv6Addr::LOCALHOST, port));
      tokio::pin!(v4, v6);
      tokio::select! {
        conn = &mut v4 => match conn {
          Ok(stream) => Ok(stream),
          Err(_) => v6.await,
        },
        conn = &mut v6 => match conn {
          Ok(stream) => Ok(stream),
          Err(_) => v4.await,
        },
      }
    }
    Some(host) => TcpStream::connect((host, port)).await,
  }
}

/// A `GET` answered with 200-399.
async fn http_ok(url: &HttpUrl) -> bool {
  let Ok(mut stream) = connect(Some(&url.host), url.port).await else {
    return false;
  };
  let request = format!(
    "GET {} HTTP/1.1\r\nHost: {}\r\nUser-Agent: dekit\r\nConnection: close\r\n\r\n",
    url.path, url.authority
  );
  if stream.write_all(request.as_bytes()).await.is_err() {
    return false;
  }
  let mut buf = [0u8; 256];
  let mut len = 0;
  while len < buf.len() {
    match stream.read(&mut buf[len..]).await {
      Ok(0) | Err(_) => break,
      Ok(n) => len += n,
    }
    if buf[..len].contains(&b'\n') {
      break;
    }
  }
  // "HTTP/1.1 200 OK"
  let line = String::from_utf8_lossy(&buf[..len]);
  let mut parts = line.split_ascii_whitespace();
  match (parts.next(), parts.next().map(str::parse::<u16>)) {
    (Some(version), Some(Ok(status))) if version.starts_with("HTTP/") => {
      (200..400).contains(&status)
    }
    _ => false,
  }
}

/// Runs `argv` in the task's cwd and env; true if it exits 0 within
/// `limit`, an error if it can't run (exit 127 included). One that takes
/// longer is killed with its process group and waited for, so attempts
/// never overlap.
#[cfg(unix)]
async fn run_cmd(
  argv: &[String],
  spec: &ProcessSpec,
  limit: Duration,
) -> std::io::Result<bool> {
  use crate::process::unix_process::spawn_command;
  use crate::process::unix_processes_waiter::UnixProcessesWaiter;

  let (sender, mut exits) = tokio::sync::mpsc::unbounded_channel();
  let pid = spawn_command(
    argv,
    spec,
    Box::new(move |info| {
      let _ = sender.send(info);
    }),
  )?;

  /// Kills the attempt's process group unless it has been reaped: a
  /// dropped attempt (the task stopped or froze) must not leave it
  /// running.
  struct Attempt(rustix::process::Pid);
  impl Drop for Attempt {
    fn drop(&mut self) {
      UnixProcessesWaiter::kill(self.0, libc::SIGKILL, true);
    }
  }

  let _attempt = Attempt(pid);
  let info = tokio::select! {
    info = exits.recv() => info,
    _ = tokio::time::sleep(limit) => {
      UnixProcessesWaiter::kill(pid, libc::SIGKILL, true);
      exits.recv().await
    }
  };
  match info {
    Some(info) if info.code == Some(127) => Err(std::io::Error::new(
      std::io::ErrorKind::NotFound,
      "exited with 127 (not found or not executable)",
    )),
    Some(info) => Ok(info.success()),
    None => Ok(false),
  }
}

#[cfg(windows)]
async fn run_cmd(
  argv: &[String],
  spec: &ProcessSpec,
  limit: Duration,
) -> std::io::Result<bool> {
  let Some(cmd) = command(argv, spec) else {
    return Ok(false);
  };
  let mut cmd = tokio::process::Command::from(cmd);
  cmd.kill_on_drop(true);
  match tokio::time::timeout(limit, cmd.status()).await {
    Ok(status) => Ok(status?.success()),
    Err(_) => Ok(false),
  }
}

/// `argv` in the task's cwd and env, no stdio: for commands dekit runs on
/// the task's behalf. None for an empty argv.
#[cfg(windows)]
pub fn command(
  argv: &[String],
  spec: &ProcessSpec,
) -> Option<std::process::Command> {
  let (prog, args) = argv.split_first()?;
  let mut cmd = std::process::Command::new(prog);
  cmd.args(args);
  if let Some(cwd) = &spec.cwd {
    cmd.current_dir(cwd);
  }
  for (k, v) in &spec.env {
    match v {
      Some(v) => cmd.env(k, v),
      None => cmd.env_remove(k),
    };
  }
  cmd.stdin(std::process::Stdio::null());
  cmd.stdout(std::process::Stdio::null());
  cmd.stderr(std::process::Stdio::null());
  Some(cmd)
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn sd_notify_datagrams() {
    assert!(sd_notify_ready(b"READY=1"));
    assert!(sd_notify_ready(b"READY=1\n"));
    assert!(sd_notify_ready(b"STATUS=Listening\nREADY=1\nMAINPID=4"));
    assert!(!sd_notify_ready(b"READY=10"));
    assert!(!sd_notify_ready(b"STATUS=READY=1"));
    assert!(!sd_notify_ready(b"RELOADING=1"));
    assert!(!sd_notify_ready(b""));
  }

  #[test]
  fn log_lines_match_plain_text() {
    assert!(line_ready("listening on :3000", "listening on :3000"));
    assert!(line_ready("ready", "server ready now"));
    assert!(!line_ready("", "anything"));
    assert!(!line_ready("ready", "read"));
  }

  #[test]
  fn tcp_targets() {
    assert_eq!(parse_tcp("5432"), Ok((None, 5432)));
    assert_eq!(parse_tcp("db:5432"), Ok((Some("db".to_string()), 5432)));
    assert_eq!(parse_tcp("[::1]:80"), Ok((Some("::1".to_string()), 80)));
    assert!(parse_tcp("::1:80").is_err());
    assert!(parse_tcp("db").is_err());
    assert!(parse_tcp("db:0").is_err());
  }

  #[test]
  fn http_urls() {
    let url = parse_http_url("http://localhost:3000/health?x=1#top").unwrap();
    assert_eq!(
      (
        url.authority.as_str(),
        url.host.as_str(),
        url.port,
        url.path.as_str()
      ),
      ("localhost:3000", "localhost", 3000, "/health?x=1")
    );
    let url = parse_http_url("http://[::1]").unwrap();
    assert_eq!(
      (url.host.as_str(), url.port, url.path.as_str()),
      ("::1", 80, "/")
    );
    let url = parse_http_url("http://x?a").unwrap();
    assert_eq!(url.path, "/?a");
    assert!(
      parse_http_url("https://x")
        .err()
        .is_some_and(|err| err.contains("https"))
    );
    assert!(parse_http_url("ftp://x").is_err());
    assert!(parse_http_url("http://u:p@x").is_err());
    let url = parse_http_url("http://host").unwrap();
    assert_eq!((url.host.as_str(), url.port), ("host", 80));
    // Without a port only a plain or a bracketed host is taken.
    for bad in [
      "http://::1:8080/health",
      "http://:8080/x",
      "http://host:80:90/",
      "http://[::1/",
      "http://[::1]x/",
      "http://host]/",
      "http://[]/",
    ] {
      assert!(parse_http_url(bad).is_err(), "{bad}");
    }
  }

  #[tokio::test]
  async fn http_probe_reads_the_status() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = tokio::spawn(async move {
      for status in ["503 Unavailable", "204 No Content"] {
        let (mut conn, _) = listener.accept().await.unwrap();
        let mut buf = [0u8; 1024];
        let n = conn.read(&mut buf).await.unwrap();
        let request = String::from_utf8_lossy(&buf[..n]).to_string();
        assert!(request.starts_with("GET /h HTTP/1.1\r\n"), "{request}");
        assert!(request.contains(&format!("Host: localhost:{port}\r\n")));
        let reply = format!("HTTP/1.1 {status}\r\nContent-Length: 0\r\n\r\n");
        conn.write_all(reply.as_bytes()).await.unwrap();
      }
    });
    let url = parse_http_url(&format!("http://localhost:{port}/h")).unwrap();
    assert!(!http_ok(&url).await);
    assert!(http_ok(&url).await);
    server.await.unwrap();
  }

  #[tokio::test]
  async fn localhost_reaches_a_server_on_ipv6_only() {
    let Ok(listener) = tokio::net::TcpListener::bind("[::1]:0").await else {
      return;
    };
    let port = listener.local_addr().unwrap().port();
    for host in [None, Some("localhost")] {
      let stream = connect(host, port).await.unwrap();
      assert!(stream.peer_addr().unwrap().is_ipv6());
    }
  }

  #[tokio::test]
  async fn tcp_and_file_probes() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let spec = ProcessSpec::from_argv(vec!["true".to_string()]);
    tokio::time::timeout(
      Duration::from_secs(2),
      wait_ready(
        Probe::Tcp { host: None, port },
        Duration::from_millis(10),
        spec.clone(),
      ),
    )
    .await
    .unwrap();

    let dir =
      std::env::temp_dir().join(format!("dekit_ready_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let mut spec = spec;
    spec.cwd(dir.to_string_lossy());
    let file = dir.join("marker");
    let _ = std::fs::remove_file(&file);
    let wait = tokio::spawn(wait_ready(
      Probe::File {
        path: PathBuf::from("marker"),
      },
      Duration::from_millis(10),
      spec,
    ));
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(!wait.is_finished());
    std::fs::write(&file, "").unwrap();
    tokio::time::timeout(Duration::from_secs(2), wait)
      .await
      .unwrap()
      .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
  }
}
