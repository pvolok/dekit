use crate::term::Winsize;

pub trait Process {
  fn on_exited(&mut self);

  fn pid(&self) -> u32;

  async fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize>;
  #[cfg_attr(windows, allow(dead_code))]
  async fn write(&mut self, buf: &[u8]) -> std::io::Result<usize>;
  async fn write_all(&mut self, buf: &[u8]) -> std::io::Result<()>;

  fn send_signal(&mut self, sig: i32, group: bool) -> std::io::Result<()>;

  async fn kill(&mut self, group: bool) -> std::io::Result<()>;

  fn resize(&mut self, size: Winsize) -> std::io::Result<()>;
}
