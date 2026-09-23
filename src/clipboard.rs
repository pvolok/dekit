use std::process::Stdio;

use anyhow::Result;
use which::which;

/// The system clipboard of the machine the runner is on. Attachments
/// also receive copied text as OSC 52, so a terminal that supports it
/// needs none of this.
#[allow(dead_code)]
enum Provider {
  Exec(&'static str, Vec<&'static str>),
  #[cfg(windows)]
  Win,
  NoOp,
}

#[cfg(windows)]
fn detect_copy_provider() -> Provider {
  Provider::Win
}

#[cfg(target_os = "macos")]
fn detect_copy_provider() -> Provider {
  if let Some(provider) = check_prog("pbcopy", &[]) {
    return provider;
  }
  Provider::NoOp
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn detect_copy_provider() -> Provider {
  // Wayland
  if std::env::var("WAYLAND_DISPLAY").is_ok() {
    if let Some(provider) = check_prog("wl-copy", &["--type", "text/plain"]) {
      return provider;
    }
  }
  // X11
  if std::env::var("DISPLAY").is_ok() {
    if let Some(provider) =
      check_prog("xclip", &["-i", "-selection", "clipboard"])
    {
      return provider;
    }
    if let Some(provider) = check_prog("xsel", &["-i", "-b"]) {
      return provider;
    }
  }
  // Termux
  if let Some(provider) = check_prog("termux-clipboard-set", &[]) {
    return provider;
  }
  // Tmux
  if std::env::var("TMUX").is_ok() {
    if let Some(provider) = check_prog("tmux", &["load-buffer", "-"]) {
      return provider;
    }
  }

  Provider::NoOp
}

#[allow(dead_code)]
fn check_prog(cmd: &'static str, args: &[&'static str]) -> Option<Provider> {
  if which(cmd).is_ok() {
    Some(Provider::Exec(cmd, args.to_vec()))
  } else {
    None
  }
}

fn copy_impl(s: &str, provider: &Provider) -> Result<()> {
  match provider {
    Provider::Exec(prog, args) => {
      let mut child = std::process::Command::new(prog)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
      if let Err(e) = std::io::Write::write_all(
        &mut child.stdin.take().unwrap(),
        s.as_bytes(),
      ) {
        log::warn!("Failed to write into copy process: {:?}", e);
      }
      #[cfg(unix)]
      crate::process::unix_processes_waiter::UnixProcessesWaiter::wait_for_child(
        child,
        Box::new(|_| ()),
      );
      #[cfg(not(unix))]
      child.wait()?;
    }

    #[cfg(windows)]
    Provider::Win => clipboard_win::set_clipboard_string(s)
      .map_err(|e| anyhow::Error::msg(e.to_string()))?,

    Provider::NoOp => (),
  };

  Ok(())
}

lazy_static::lazy_static! {
  static ref PROVIDER: Provider = detect_copy_provider();
}

pub fn copy(s: &str) {
  match copy_impl(s, &PROVIDER) {
    Ok(()) => (),
    Err(err) => log::warn!("Copying error: {}", err),
  }
}
