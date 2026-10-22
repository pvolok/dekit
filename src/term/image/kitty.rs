//! The kitty graphics protocol: `ESC _ G <control> ; <payload> ESC \`.
//!
//! <https://sw.kovidgoyal.net/kitty/graphics-protocol/>

use base64::Engine as _;

#[cfg(unix)]
use super::store::MAX_IMAGE_BYTES;

/// One command's control data.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Command {
  /// `a`: t transmit, T transmit and display, p put, d delete, f frame,
  /// a animate, c compose, q query.
  pub action: u8,
  pub quiet: u8,
  pub format: u32,
  /// `t`: d direct, f file, t temporary file, s shared memory.
  pub medium: u8,
  pub width: u32,
  pub height: u32,
  pub size: u32,
  pub offset: u32,
  pub zlib: bool,
  pub more: bool,
  pub id: u32,
  pub number: u32,
  pub placement: u32,
  pub x: u32,
  pub y: u32,
  pub w: u32,
  pub h: u32,
  pub off_x: u32,
  pub off_y: u32,
  pub cols: u32,
  pub rows: u32,
  pub z: i32,
  pub cursor_stay: bool,
  pub unicode: bool,
  pub delete: u8,
  /// Keys of a frame, animation or compose command other than the
  /// data's, kept as text to replay.
  pub extra: String,
}

impl Command {
  /// Parses the control data (the part before `;`).
  pub fn parse(control: &[u8]) -> Result<Command, &'static str> {
    let mut cmd = Command {
      action: b't',
      format: 32,
      medium: b'd',
      ..Command::default()
    };
    let mut extra = Vec::new();
    for pair in control.split(|b| *b == b',') {
      if pair.is_empty() {
        continue;
      }
      let Some(eq) = pair.iter().position(|b| *b == b'=') else {
        return Err("EINVAL:malformed control data");
      };
      let (key, value) = (&pair[..eq], &pair[eq + 1..]);
      if key.len() != 1 || value.is_empty() {
        return Err("EINVAL:malformed control data");
      }
      let text = std::str::from_utf8(value).map_err(|_| "EINVAL:bad value")?;
      let int = || text.parse::<i64>().map_err(|_| "EINVAL:bad number");
      let uint = || -> Result<u32, &'static str> {
        let n = int()?;
        if !(0..=u32::MAX as i64).contains(&n) {
          return Err("EINVAL:bad number");
        }
        Ok(n as u32)
      };
      match key[0] {
        b'a' => cmd.action = value[0],
        b'q' => cmd.quiet = uint()?.min(2) as u8,
        b'f' => cmd.format = uint()?,
        b't' => cmd.medium = value[0],
        b's' => cmd.width = uint()?,
        b'v' => cmd.height = uint()?,
        b'S' => cmd.size = uint()?,
        b'O' => cmd.offset = uint()?,
        b'o' => cmd.zlib = value[0] == b'z',
        b'm' => cmd.more = uint()? != 0,
        b'i' => cmd.id = uint()?,
        b'I' => cmd.number = uint()?,
        b'p' => cmd.placement = uint()?,
        b'x' => cmd.x = uint()?,
        b'y' => cmd.y = uint()?,
        b'w' => cmd.w = uint()?,
        b'h' => cmd.h = uint()?,
        b'X' => cmd.off_x = uint()?,
        b'Y' => cmd.off_y = uint()?,
        b'c' => cmd.cols = uint()?,
        b'r' => cmd.rows = uint()?,
        b'z' => cmd.z = int()?.clamp(i32::MIN as i64, i32::MAX as i64) as i32,
        b'C' => cmd.cursor_stay = uint()? != 0,
        b'U' => cmd.unicode = uint()? != 0,
        b'd' => cmd.delete = value[0],
        _ => (),
      }
      // Frame, animation and compose commands reuse letters with other
      // meanings; keep them verbatim for replay.
      if !matches!(
        key[0],
        b'a' | b'q' | b'i' | b'I' | b'm' | b't' | b'S' | b'O'
      ) {
        extra.push(String::from_utf8_lossy(pair).into_owned());
      }
    }
    cmd.extra = extra.join(",");
    Ok(cmd)
  }
}

/// A transmission being received in chunks.
#[derive(Clone, Debug)]
pub struct Pending {
  pub command: Command,
  pub data: Vec<u8>,
}

/// Appends the decoded base64 `payload` to `out`; false (and `out` as it
/// was) when it is not base64.
pub(crate) fn decode_payload(payload: &[u8], out: &mut Vec<u8>) -> bool {
  if payload.is_empty() {
    return true;
  }
  let len = out.len();
  for engine in [
    &base64::engine::general_purpose::STANDARD,
    &base64::engine::general_purpose::STANDARD_NO_PAD,
  ] {
    if engine.decode_vec(payload, out).is_ok() {
      return true;
    }
    out.truncate(len);
  }
  false
}

/// Reads an image from a file, a temporary file or shared memory named
/// by the (decoded) payload, with kitty's restrictions.
#[cfg(unix)]
pub(crate) fn read_medium(
  command: &Command,
  name: &[u8],
) -> Result<Vec<u8>, &'static str> {
  use std::io::{Read, Seek, SeekFrom};
  let path = std::str::from_utf8(name).map_err(|_| "EBADF:bad path")?;
  let limit = if command.size > 0 {
    command.size as usize
  } else {
    MAX_IMAGE_BYTES
  };
  match command.medium {
    b'f' | b't' => {
      use std::path::Path;
      let path = Path::new(path);
      if command.medium == b't' {
        // As kitty: named for the protocol, in a known temporary
        // directory, judged on the path as given (the real one may be
        // elsewhere, as /tmp is on macOS).
        let name_ok = path.file_name().is_some_and(|n| {
          n.to_string_lossy().contains("tty-graphics-protocol")
        });
        let dir_ok = path.starts_with("/tmp")
          || path.starts_with("/dev/shm")
          || path.starts_with(std::env::temp_dir());
        if !name_ok || !dir_ok {
          return Err("EBADF:temporary file not in a temporary directory");
        }
      }
      let canonical =
        std::fs::canonicalize(path).map_err(|_| "EBADF:cannot read file")?;
      if canonical.starts_with("/proc")
        || canonical.starts_with("/sys")
        || (canonical.starts_with("/dev") && !canonical.starts_with("/dev/shm"))
      {
        return Err("EBADF:refusing to read that path");
      }
      let mut file = std::fs::File::open(&canonical)
        .map_err(|_| "EBADF:cannot read file")?;
      if command.offset > 0 {
        file
          .seek(SeekFrom::Start(u64::from(command.offset)))
          .map_err(|_| "EBADF:cannot read file")?;
      }
      let mut data = Vec::new();
      file
        .take(limit as u64)
        .read_to_end(&mut data)
        .map_err(|_| "EBADF:cannot read file")?;
      if command.medium == b't' {
        let _ = std::fs::remove_file(&canonical);
      }
      Ok(data)
    }
    b's' => {
      let fd = rustix::shm::open(
        path,
        rustix::shm::OFlags::RDONLY,
        rustix::fs::Mode::empty(),
      )
      .map_err(|_| "EBADF:cannot open shared memory")?;
      let mut file = std::fs::File::from(fd);
      if command.offset > 0 {
        file
          .seek(SeekFrom::Start(u64::from(command.offset)))
          .map_err(|_| "EBADF:cannot read shared memory")?;
      }
      let mut data = Vec::new();
      let result = file.take(limit as u64).read_to_end(&mut data);
      let _ = rustix::shm::unlink(path);
      result.map_err(|_| "EBADF:cannot read shared memory")?;
      Ok(data)
    }
    _ => Err("EINVAL:unknown transmission medium"),
  }
}

#[cfg(not(unix))]
pub(crate) fn read_medium(
  _command: &Command,
  _name: &[u8],
) -> Result<Vec<u8>, &'static str> {
  Err("EINVAL:unsupported transmission medium")
}

#[cfg(test)]
mod tests {
  use super::*;

  #[cfg(unix)]
  #[test]
  fn temporary_files_are_read_from_the_temp_dir_and_removed() {
    let dir = std::env::temp_dir();
    let path = dir.join(format!(
      "tty-graphics-protocol-dekit-test-{}",
      std::process::id()
    ));
    std::fs::write(&path, [1u8, 2, 3]).unwrap();
    let command = Command::parse(b"a=T,t=t,f=24,s=1,v=1").unwrap();
    let data = read_medium(&command, path.to_string_lossy().as_bytes());
    assert_eq!(data, Ok(vec![1, 2, 3]));
    assert!(!path.exists(), "a temporary file is consumed");
    // Elsewhere, or named otherwise, it is refused and left alone.
    let other = dir.join(format!("dekit-test-{}", std::process::id()));
    std::fs::write(&other, [1u8]).unwrap();
    assert!(read_medium(&command, other.to_string_lossy().as_bytes()).is_err());
    assert!(other.exists());
    std::fs::remove_file(&other).unwrap();
  }

  #[test]
  fn parses_control_data() {
    let cmd = Command::parse(b"a=T,f=24,s=2,v=1,i=7,p=3,z=-1,C=1,q=2").unwrap();
    assert_eq!(cmd.action, b'T');
    assert_eq!(cmd.format, 24);
    assert_eq!((cmd.width, cmd.height), (2, 1));
    assert_eq!((cmd.id, cmd.placement), (7, 3));
    assert_eq!(cmd.z, -1);
    assert!(cmd.cursor_stay);
    assert_eq!(cmd.quiet, 2);
    assert_eq!(cmd.extra, "f=24,s=2,v=1,p=3,z=-1,C=1");
    assert!(Command::parse(b"a").is_err());
    assert!(Command::parse(b"i=x").is_err());
    let defaults = Command::parse(b"").unwrap();
    assert_eq!(defaults.action, b't');
    assert_eq!(defaults.format, 32);
    assert_eq!(defaults.medium, b'd');
  }
}
