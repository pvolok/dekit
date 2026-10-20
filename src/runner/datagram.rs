//! A Unix datagram socket the runner owns on a task's behalf: bound once,
//! kept while the task's instances come and go, inherited across a live
//! upgrade like the runner's listener, and unlinked when dropped.

use std::io;
use std::os::fd::{AsRawFd, FromRawFd};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use tokio::net::UnixDatagram;

pub struct DatagramSocket {
  socket: Arc<UnixDatagram>,
  path: PathBuf,
}

impl DatagramSocket {
  /// Binds at `path`, over any stale file left there.
  pub fn bind(path: PathBuf) -> io::Result<Self> {
    if let Err(err) = std::fs::remove_file(&path)
      && err.kind() != io::ErrorKind::NotFound
    {
      return Err(err);
    }
    Ok(Self {
      socket: Arc::new(UnixDatagram::bind(&path)?),
      path,
    })
  }

  /// One inherited across an exec, still bound at `path`.
  pub fn adopt(fd: i32, path: PathBuf) -> io::Result<Self> {
    let socket = unsafe { std::os::unix::net::UnixDatagram::from_raw_fd(fd) };
    socket.set_nonblocking(true)?;
    Ok(Self {
      socket: Arc::new(UnixDatagram::from_std(socket)?),
      path,
    })
  }

  pub fn path(&self) -> &Path {
    &self.path
  }

  pub fn raw_fd(&self) -> i32 {
    self.socket.as_raw_fd()
  }

  /// Discards every datagram received so far.
  pub fn drain(&self) {
    // Straight off the descriptor: the reactor may not have seen them yet.
    let mut buf = [0u8; 4096];
    loop {
      let n = unsafe {
        libc::recv(
          self.raw_fd(),
          buf.as_mut_ptr().cast(),
          buf.len(),
          libc::MSG_DONTWAIT,
        )
      };
      if n < 0 {
        break;
      }
    }
  }

  /// The socket to read from, held as long as the reader.
  pub fn reader(&self) -> Arc<UnixDatagram> {
    self.socket.clone()
  }
}

impl Drop for DatagramSocket {
  fn drop(&mut self) {
    let _ = std::fs::remove_file(&self.path);
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  fn socket_path(name: &str) -> PathBuf {
    let dir =
      std::env::temp_dir().join(format!("dekit_dgram_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir.join(format!("{name}.sock"))
  }

  fn send(path: &Path, bytes: &[u8]) {
    let client = std::os::unix::net::UnixDatagram::unbound().unwrap();
    client.send_to(bytes, path).unwrap();
  }

  #[tokio::test]
  async fn receives_after_a_drain_and_unlinks_on_drop() {
    let path = socket_path("a");
    // A stale file is replaced.
    std::fs::write(&path, "").unwrap();
    let socket = DatagramSocket::bind(path.clone()).unwrap();
    send(&path, b"one");
    send(&path, b"two");
    socket.drain();
    send(&path, b"three");
    let mut buf = [0u8; 16];
    let n = socket.reader().recv(&mut buf).await.unwrap();
    assert_eq!(&buf[..n], b"three");
    drop(socket);
    assert!(!path.exists());
  }

  #[tokio::test]
  async fn an_adopted_socket_keeps_what_was_sent_before() {
    let path = socket_path("b");
    let bound = DatagramSocket::bind(path.clone()).unwrap();
    send(&path, b"READY=1");
    let fd = unsafe { libc::dup(bound.raw_fd()) };
    // The image that bound it execs; it never drops its sockets.
    std::mem::forget(bound);
    let adopted = DatagramSocket::adopt(fd, path.clone()).unwrap();
    let mut buf = [0u8; 16];
    let n = adopted.reader().recv(&mut buf).await.unwrap();
    assert_eq!(&buf[..n], b"READY=1");
    assert_eq!(adopted.path(), path);
  }
}
