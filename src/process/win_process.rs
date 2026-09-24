use std::{
  env,
  io::{self},
  iter::once,
  mem::{size_of, zeroed},
  os::windows::io::{AsRawHandle, FromRawHandle, IntoRawHandle, OwnedHandle},
  ptr::null,
};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use windows::{
  Win32::{
    Foundation::{CloseHandle, HANDLE},
    System::{
      Console::{
        COORD, ClosePseudoConsole, CreatePseudoConsole, HPCON,
        ResizePseudoConsole,
      },
      Pipes::CreatePipe,
      Threading::{
        CREATE_UNICODE_ENVIRONMENT, CreateProcessW,
        DeleteProcThreadAttributeList, EXTENDED_STARTUPINFO_PRESENT,
        GetExitCodeProcess, InitializeProcThreadAttributeList,
        LPPROC_THREAD_ATTRIBUTE_LIST, PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE,
        PROCESS_INFORMATION, RegisterWaitForSingleObject, STARTF_USESTDHANDLES,
        STARTUPINFOEXW, TerminateProcess, UnregisterWait,
        UpdateProcThreadAttribute, WT_EXECUTEONLYONCE,
      },
    },
  },
  core::{PCWSTR, PWSTR},
};

use crate::{
  error::ResultLogger, kernel::task::TaskId, process::process::Process,
  term::Winsize,
};

use super::process_spec::ProcessSpec;

const SIGKILL: i32 = 9;

pub struct WinProcess {
  pub pid: i32,
  reader: tokio::fs::File,
  writer: tokio::fs::File,
  conpty: HPCON,
  process_handle: OwnedHandle,
  wait_handle: HANDLE,
}
unsafe impl Send for WinProcess {}

type OnWaitReturned = Box<dyn Fn(Option<i32>) + Send + Sync>;

impl WinProcess {
  pub fn spawn(
    _id: TaskId,
    spec: &ProcessSpec,
    size: Winsize,
    on_wait_returned: OnWaitReturned,
  ) -> io::Result<Self> {
    unsafe {
      // ConPTY consumes synchronous pipe handles. Tokio's file adapter keeps
      // the host-facing synchronous ends off the async runtime's worker.
      let mut conpty_input = HANDLE::default();
      let mut host_write = HANDLE::default();
      CreatePipe(&mut conpty_input, &mut host_write, None, 0)?;
      let conpty_input = OwnedHandle::from_raw_handle(conpty_input.0);
      let host_write = OwnedHandle::from_raw_handle(host_write.0);

      let mut host_read = HANDLE::default();
      let mut conpty_output = HANDLE::default();
      CreatePipe(&mut host_read, &mut conpty_output, None, 0)?;
      let host_read = OwnedHandle::from_raw_handle(host_read.0);
      let conpty_output = OwnedHandle::from_raw_handle(conpty_output.0);

      // Create pseudo console
      let coord = COORD {
        X: size.x as i16,
        Y: size.y as i16,
      };
      let conpty = CreatePseudoConsole(
        coord,
        HANDLE(conpty_input.as_raw_handle()),
        HANDLE(conpty_output.as_raw_handle()),
        0,
      )?;

      let mut startup_info_ex: STARTUPINFOEXW = zeroed();
      startup_info_ex.StartupInfo.cb = size_of::<STARTUPINFOEXW>() as u32;
      // Prevent CreateProcessW from retaining the parent's console handles.
      // ConPTY supplies the child's real standard handles during attachment.
      startup_info_ex.StartupInfo.dwFlags |= STARTF_USESTDHANDLES;

      let mut attr_list_size: usize = 0;
      // Note: This initial call will return an error by design. This is
      // expected behavior.
      // https://learn.microsoft.com/en-us/windows/win32/api/processthreadsapi/nf-processthreadsapi-initializeprocthreadattributelist#remarks
      let _: Result<(), windows::core::Error> =
        InitializeProcThreadAttributeList(None, 1, None, &mut attr_list_size);

      let mut attr_list: Vec<u8> = vec![0; attr_list_size];
      startup_info_ex.lpAttributeList =
        LPPROC_THREAD_ATTRIBUTE_LIST(attr_list.as_mut_ptr() as _);

      InitializeProcThreadAttributeList(
        Some(startup_info_ex.lpAttributeList),
        1,
        None,
        &mut attr_list_size,
      )?;

      UpdateProcThreadAttribute(
        startup_info_ex.lpAttributeList,
        0,
        PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE as usize,
        Some(conpty.0 as _),
        size_of::<HPCON>(),
        None,
        None,
      )?;

      // Build environment block
      let mut env_map: std::collections::HashMap<String, String> =
        env::vars().collect();
      let term_is_explicit =
        spec.env.keys().any(|key| key.eq_ignore_ascii_case("TERM"));
      for (key, value) in &spec.env {
        if let Some(val) = value {
          env_map.insert(key.clone(), val.clone());
        } else {
          env_map.remove(key);
        }
      }
      if !term_is_explicit {
        // The child talks to dekit's ConPTY/VT implementation, not directly
        // to the terminal that launched the runner. In particular, inheriting
        // TERM=dumb makes full-screen applications such as Neovim suppress
        // their UI even though this PTY supports xterm-style color and input.
        env_map.retain(|key, _| !key.eq_ignore_ascii_case("TERM"));
        env_map.insert("TERM".to_string(), "xterm-256color".to_string());
      }
      let mut env_block: Vec<u16> = Vec::new();
      let mut env_pairs: Vec<(&String, &String)> = env_map.iter().collect();
      env_pairs.sort_by_key(|pair| pair.0);
      for (key, value) in env_pairs {
        env_block.extend(key.encode_utf16());
        env_block.push('=' as u16);
        env_block.extend(value.encode_utf16());
        env_block.push(0);
      }
      env_block.push(0);
      let env_block_ptr = if env_block.is_empty() {
        None
      } else {
        Some(env_block.as_mut_ptr() as _)
      };

      // Build command line
      fn quote_arg(arg: &str) -> String {
        if !arg.chars().any(|c| c == ' ' || c == '\t' || c == '"') {
          arg.to_string()
        } else {
          let mut s = String::new();
          s.push('"');
          for c in arg.chars() {
            if c == '"' {
              s.push('\\');
            }
            s.push(c);
          }
          s.push('"');
          s
        }
      }
      let mut cmdline = quote_arg(&spec.prog);
      for arg in &spec.args {
        cmdline.push(' ');
        cmdline.push_str(&quote_arg(arg));
      }
      let cmdline_wide: Vec<u16> =
        cmdline.encode_utf16().chain(once(0)).collect();
      let cmdline_ptr = cmdline_wide.as_ptr() as *mut u16;

      // CWD
      let cwd = spec.get_cwd().as_ref();
      let cwd_wide =
        cwd.map(|s| s.encode_utf16().chain(once(0)).collect::<Vec<u16>>());
      let cwd_ptr = cwd_wide.as_ref().map_or(null(), |v| v.as_ptr());

      let mut process_info: PROCESS_INFORMATION = zeroed();
      CreateProcessW(
        None,
        Some(PWSTR::from_raw(cmdline_ptr)),
        None,
        None,
        false,
        EXTENDED_STARTUPINFO_PRESENT | CREATE_UNICODE_ENVIRONMENT,
        env_block_ptr,
        PCWSTR::from_raw(cwd_ptr),
        &startup_info_ex.StartupInfo,
        &mut process_info,
      )?;
      // Keep the ConPTY-facing pipe handles alive until the client has been
      // attached. Closing them earlier can tear down conhost before it has
      // processed PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE.
      drop(conpty_input);
      drop(conpty_output);
      DeleteProcThreadAttributeList(startup_info_ex.lpAttributeList);

      let process_handle =
        OwnedHandle::from_raw_handle(process_info.hProcess.0);
      let pid = process_info.dwProcessId as i32;
      CloseHandle(process_info.hThread)?;

      struct WaitContext {
        callback: OnWaitReturned,
        process_handle: HANDLE,
      }
      unsafe extern "system" fn wait_callback(
        context: *mut std::ffi::c_void,
        _: bool,
      ) {
        let context = unsafe { Box::from_raw(context.cast::<WaitContext>()) };
        let mut exit_code = 0;
        let exit_code = if unsafe {
          GetExitCodeProcess(context.process_handle, &mut exit_code)
        }
        .is_ok()
        {
          Some(exit_code as i32)
        } else {
          None
        };
        (context.callback)(exit_code);
      }

      let mut wait_handle = HANDLE::default();
      RegisterWaitForSingleObject(
        &mut wait_handle,
        HANDLE(process_handle.as_raw_handle()),
        Some(wait_callback),
        Some(Box::into_raw(Box::new(WaitContext {
          callback: on_wait_returned,
          process_handle: HANDLE(process_handle.as_raw_handle()),
        })) as _),
        u32::MAX,
        WT_EXECUTEONLYONCE,
      )?;

      let reader = tokio::fs::File::from_std(std::fs::File::from_raw_handle(
        host_read.into_raw_handle(),
      ));
      let writer = tokio::fs::File::from_std(std::fs::File::from_raw_handle(
        host_write.into_raw_handle(),
      ));

      Ok(WinProcess {
        pid,
        reader,
        writer,
        conpty,
        process_handle,
        wait_handle,
      })
    }
  }
}

impl Process for WinProcess {
  fn on_exited(&mut self) {
    unsafe {
      ClosePseudoConsole(self.conpty);
      self.conpty = HPCON::default();

      UnregisterWait(self.wait_handle).log_ignore();
      self.wait_handle = HANDLE::default();
    };
  }

  fn pid(&self) -> u32 {
    self.pid as u32
  }

  async fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
    let count = self.reader.read(buf).await?;
    Ok(count)
  }

  async fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
    self.writer.write(buf).await
  }

  async fn write_all(&mut self, buf: &[u8]) -> io::Result<()> {
    self.writer.write_all(buf).await
  }

  fn send_signal(&mut self, sig: i32, _group: bool) -> io::Result<()> {
    if sig == SIGKILL {
      unsafe {
        TerminateProcess(HANDLE(self.process_handle.as_raw_handle()), 1)?
      };
    } else {
      // Only SIGKILL is supported on Windows
    }
    Ok(())
  }

  async fn kill(&mut self, group: bool) -> io::Result<()> {
    self.send_signal(SIGKILL, group)
  }

  fn resize(&mut self, size: Winsize) -> io::Result<()> {
    unsafe {
      ResizePseudoConsole(
        self.conpty,
        COORD {
          X: size.x as i16,
          Y: size.y as i16,
        },
      )?
    };
    Ok(())
  }
}

impl Drop for WinProcess {
  fn drop(&mut self) {
    unsafe {
      if !self.conpty.is_invalid() {
        log::warn!("`self.conpty` is still open in `WinProcess::drop()`.");
        ClosePseudoConsole(self.conpty);
      }
      if !self.wait_handle.is_invalid() {
        log::warn!("`self.wait_handle` is still open in `WinProcess::drop()`.");
        UnregisterWait(self.wait_handle).log_ignore();
      }
    }
  }
}

#[cfg(test)]
mod tests {
  use std::sync::atomic::{AtomicUsize, Ordering};

  use tokio::sync::mpsc::unbounded_channel;

  use super::*;

  #[tokio::test]
  async fn reads_output_from_conpty() {
    static NEXT_ID: AtomicUsize = AtomicUsize::new(10_000);
    let id = TaskId(NEXT_ID.fetch_add(1, Ordering::Relaxed));
    let spec = ProcessSpec::from_argv(vec![
      "cmd.exe".to_string(),
      "/d".to_string(),
      "/c".to_string(),
      "echo DEKIT_CONPTY_PROBE".to_string(),
    ]);
    let (exit_tx, mut exit_rx) = unbounded_channel();
    let mut process = WinProcess::spawn(
      id,
      &spec,
      Winsize {
        x: 80,
        y: 24,
        x_px: 0,
        y_px: 0,
      },
      Box::new(move |code| {
        let _ = exit_tx.send(code);
      }),
    )
    .unwrap();

    let output =
      tokio::time::timeout(std::time::Duration::from_secs(5), async {
        let mut output = Vec::new();
        let mut buf = [0; 4096];
        loop {
          let n = process.read(&mut buf).await.unwrap();
          if n == 0 {
            break output;
          }
          output.extend_from_slice(&buf[..n]);
          if output
            .windows(b"DEKIT_CONPTY_PROBE".len())
            .any(|window| window == b"DEKIT_CONPTY_PROBE")
          {
            break output;
          }
        }
      })
      .await
      .expect("timed out waiting for ConPTY output");

    assert!(
      output
        .windows(b"DEKIT_CONPTY_PROBE".len())
        .any(|window| window == b"DEKIT_CONPTY_PROBE"),
      "ConPTY output was: {:?}",
      String::from_utf8_lossy(&output)
    );
    let code =
      tokio::time::timeout(std::time::Duration::from_secs(5), exit_rx.recv())
        .await
        .expect("timed out waiting for child exit");
    assert_eq!(code, Some(Some(0)));
    process.on_exited();
  }
}
