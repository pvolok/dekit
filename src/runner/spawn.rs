use std::path::Path;

use crate::runner::RunnerSpec;

pub fn spawn_runner(
  runner: &RunnerSpec,
  executable: &Path,
) -> anyhow::Result<()> {
  let exe = dunce::canonicalize(executable)?;
  let dir_str = runner
    .root
    .to_str()
    .expect("validated runner root")
    .to_string();
  let kind = runner.kind.as_str();

  #[cfg(unix)]
  return self::unix::spawn_impl(exe, &dir_str, kind);
  #[cfg(windows)]
  return self::windows::spawn_impl(exe, &dir_str, kind);
}

#[cfg(unix)]
mod unix {
  use std::{ffi::CString, path::PathBuf};

  use anyhow::bail;

  pub fn spawn_impl(exe: PathBuf, dir: &str, kind: &str) -> anyhow::Result<()> {
    let process = daemonize::Daemonize::new().working_directory(dir);

    match process.execute() {
      daemonize::Outcome::Parent(_) => (),
      daemonize::Outcome::Child(_) => {
        // daemonize double-forks, so this process is in a session led
        // by its exited intermediate parent. Take a session of our own:
        // every task inherits it, which lets a force-stop reap the
        // whole tree by session membership. Failure just leaves the
        // reap guard (`sid == pid`) off.
        unsafe {
          libc::setsid();
        }
        exec(&[
          exe.to_str().ok_or_else(|| {
            anyhow::format_err!("Failed to convert exe path: {:?}", exe)
          })?,
          "runner",
          "run",
          "--dir",
          dir,
          "--kind",
          kind,
        ])?
      }
    }

    Ok(())
  }

  #[cfg(unix)]
  fn exec(argv: &[&str]) -> anyhow::Result<()> {
    // Add null terminations to our strings and our argument array,
    // converting them into a C-compatible format.
    let program_cstring = CString::new(
      argv
        .first()
        .ok_or_else(|| anyhow::format_err!("Empty argv"))?
        .as_bytes(),
    )?;
    let arg_cstrings = argv
      .iter()
      .map(|arg| CString::new(arg.as_bytes()))
      .collect::<Result<Vec<_>, _>>()?;
    let mut arg_charptrs: Vec<_> =
      arg_cstrings.iter().map(|arg| arg.as_ptr()).collect();
    arg_charptrs.push(std::ptr::null());

    // Use an `unsafe` block so that we can call directly into C.
    let res =
      unsafe { libc::execvp(program_cstring.as_ptr(), arg_charptrs.as_ptr()) };

    // Handle our error result.
    if res < 0 {
      bail!("Error calling execvp");
    } else {
      // Should never happen.
      panic!("execvp returned unexpectedly")
    }
  }
}

#[cfg(windows)]
mod windows {
  use std::path::PathBuf;

  use windows::Win32::{
    Foundation::{HANDLE_FLAG_INHERIT, HANDLE_FLAGS, SetHandleInformation},
    System::{
      Console::{
        GetStdHandle, STD_ERROR_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE,
      },
      Threading::{CREATE_NEW_PROCESS_GROUP, DETACHED_PROCESS},
    },
  };

  pub fn spawn_impl(
    path: PathBuf,
    dir: &str,
    kind: &str,
  ) -> anyhow::Result<()> {
    use std::{os::windows::process::CommandExt, process::Stdio};

    // std spawns with bInheritHandles, so the runner would hold our
    // caller's stdio pipes open forever.
    for std in [STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, STD_ERROR_HANDLE] {
      if let Ok(handle) = unsafe { GetStdHandle(std) }
        && !handle.is_invalid()
      {
        let _ = unsafe {
          SetHandleInformation(handle, HANDLE_FLAG_INHERIT.0, HANDLE_FLAGS(0))
        };
      }
    }

    std::process::Command::new(path)
      .args(["runner", "run", "--dir", dir, "--kind", kind])
      .current_dir(dir)
      .stdin(Stdio::null())
      .stdout(Stdio::null())
      .stderr(Stdio::null())
      .creation_flags(CREATE_NEW_PROCESS_GROUP.0 | DETACHED_PROCESS.0)
      .spawn()?;

    Ok(())
  }
}
