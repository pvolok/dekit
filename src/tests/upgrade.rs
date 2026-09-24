//! Live upgrade of a running runner (release gates): the
//! child, its PTY, an attached client, and the runner identity all
//! survive an exec into the same binary. A target that cannot resume the
//! runner is refused before the switch; a resume that fails after it
//! kills the tasks and says why.

#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::Output;
use std::time::{Duration, Instant};

use lib::protocol::{
  ConnReceiver, ConnSender, CtlMsg, Event, Msg, Request, RpcRequest,
  client_handshake, ctl::EVENT_INPUT,
};
use lib::runner::socket::connect_socket;
use lib::term::{TermEvent, key::Key};

mod common;
use common::{DEKIT, TestRunner, stderr_of as stderr, task_line, wait_until};

impl TestRunner {
  /// The published record, as `runner status --json` reports it.
  fn record(&self) -> serde_json::Value {
    let out = self.ok(&["--json", "runner", "status"]);
    let value: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(value["status"], "running", "{value}");
    value
  }

  fn upgrade(&self) -> Output {
    self.upgrade_to(Path::new(DEKIT))
  }

  fn upgrade_to(&self, binary: &Path) -> Output {
    self.run(&["runner", "upgrade", "--binary", binary.to_str().unwrap()])
  }

  /// An executable shell script in the work dir.
  fn script(&self, name: &str, body: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let path = self.work.path.join(name);
    std::fs::write(&path, format!("#!/bin/sh\n{body}")).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
      .unwrap();
    path
  }

  fn snapshot_files(&self) -> Vec<String> {
    std::fs::read_dir(self.runtime.path.join("dekit"))
      .into_iter()
      .flatten()
      .filter_map(|entry| entry.ok())
      .map(|entry| entry.file_name().to_string_lossy().into_owned())
      .filter(|name| name.ends_with(".snapshot"))
      .collect()
  }
}

/// The ticker prints its own pid first; `screen` shows the task's terminal.
fn child_pid(runner: &TestRunner) -> u32 {
  pid_of(runner, "ticker")
}

fn pid_of(runner: &TestRunner, task: &str) -> u32 {
  let screen = runner.ok(&["screen", task]);
  screen
    .lines()
    .find_map(|line| line.trim().strip_prefix("pid ")?.trim().parse().ok())
    .unwrap_or_else(|| panic!("{task} printed no pid: {screen}"))
}

fn last_tick(runner: &TestRunner) -> Option<u64> {
  let screen = runner.ok(&["screen", "ticker"]);
  screen
    .lines()
    .filter_map(|line| line.trim().strip_prefix("tick ")?.trim().parse().ok())
    .last()
}

#[tokio::test]
async fn upgrade_keeps_child_client_and_identity() {
  let runner = TestRunner::start("up");
  runner.ok(&[
    "spawn",
    "ticker",
    "--",
    "sh",
    "-c",
    "echo pid $$; i=0; while true; do echo tick $i; i=$((i+1)); sleep 0.1; done",
  ]);
  wait_until("first ticks", || last_tick(&runner).is_some_and(|t| t >= 2));
  let pid_before = child_pid(&runner);
  let record_before = runner.record();

  // An attached client, driven over the wire like the terminal client.
  let (mut sender, mut receiver) = attach(&runner, "ticker").await;
  assert!(saw(&mut receiver, b"tick").await, "attach paints before");

  let tick_before = last_tick(&runner).unwrap();
  let out = runner.upgrade();
  assert!(out.status.success(), "upgrade: {}", stderr(&out));
  let stdout = String::from_utf8_lossy(&out.stdout);
  assert!(stdout.contains("upgraded live"), "{stdout}");

  // Identity: same runner pid, same start, same child, binary published.
  let record_after = runner.record();
  assert_eq!(record_after["pid"], record_before["pid"]);
  assert_eq!(record_after["started_at"], record_before["started_at"]);
  assert_eq!(record_after["binary"], DEKIT);
  assert_eq!(child_pid(&runner), pid_before);
  assert!(
    runner.snapshot_files().is_empty(),
    "snapshot file left behind"
  );

  // Output kept flowing into the same screen, and the graph is intact.
  wait_until("ticks after upgrade", || {
    last_tick(&runner).is_some_and(|t| t > tick_before + 2)
  });
  let ls = runner.ok(&["ls"]);
  assert!(ls.contains("ticker") && ls.contains("ready"), "{ls}");

  // The attached client is still attached: it gets the repaint, and its
  // input still reaches the child (Ctrl-C ends the loop).
  assert!(saw(&mut receiver, b"tick").await, "attach paints after");
  send_key(&mut sender, "<C-c>").await;
  wait_until("child exit after Ctrl-C", || {
    runner.ok(&["ls"]).contains("exited")
  });
  drop(sender);
  drop(receiver);

  runner.stop();
}

/// A restart is an upgrade into the same binary that re-reads
/// `dekit.yaml`: a running task keeps its child and takes the new command
/// at its next start, and a task added to the config appears.
#[tokio::test]
async fn restart_reloads_config() {
  let runner = TestRunner::new("rs");
  runner.yaml(
    "tasks:\n  alpha:\n    cmd: [sh, -c, 'echo pid $$; echo one; sleep 60']\n    autostart: true\n",
  );
  runner.start_runner();
  wait_until("alpha ready", || {
    task_line(&runner, "alpha").contains("ready")
  });
  let pid = pid_of(&runner, "alpha");

  runner.yaml(
    "tasks:\n  alpha:\n    cmd: [sh, -c, 'echo pid $$; echo two; sleep 60']\n    autostart: true\n  beta:\n    cmd: [sleep, '60']\n    autostart: true\n    deps: [alpha]\n",
  );
  let out = runner.run(&["runner", "restart"]);
  assert!(out.status.success(), "restart: {}", stderr(&out));
  assert!(
    String::from_utf8_lossy(&out.stdout).contains("restarted live"),
    "{out:?}"
  );

  // The running child is untouched; the added task starts.
  assert_eq!(pid_of(&runner, "alpha"), pid);
  let screen = runner.ok(&["screen", "alpha"]);
  assert!(
    screen.contains("one") && !screen.contains("two"),
    "{screen}"
  );
  wait_until("beta ready", || {
    task_line(&runner, "beta").contains("ready")
  });

  // The new command applies at the next start.
  runner.ok(&["restart", "alpha"]);
  wait_until("alpha runs the new command", || {
    runner.ok(&["screen", "alpha"]).contains("two")
  });
  assert_ne!(pid_of(&runner, "alpha"), pid);

  runner.stop();
}

#[tokio::test]
async fn upgrade_twice_in_a_row() {
  let runner = TestRunner::start("up2");
  runner.ok(&["spawn", "sleeper", "--", "sleep", "60"]);
  wait_until("ready", || runner.ok(&["ls"]).contains("ready"));
  let pid = runner.record()["pid"].clone();
  for _ in 0..2 {
    let out = runner.upgrade();
    assert!(out.status.success(), "upgrade: {}", stderr(&out));
    assert_eq!(runner.record()["pid"], pid);
    assert!(runner.ok(&["ls"]).contains("ready"));
  }
  runner.stop();
}

#[tokio::test]
async fn rejected_targets_leave_the_runner_running() {
  let runner = TestRunner::start("upbad");
  runner.ok(&["spawn", "sleeper", "--", "sleep", "60"]);
  wait_until("ready", || runner.ok(&["ls"]).contains("ready"));

  let rejected = [
    // Fails the check.
    (runner.script("bad", "echo nope >&2\nexit 3\n"), "nope"),
    // Not dekit: exits 0 without answering the check.
    (runner.script("silent", "exit 0\n"), "did not confirm"),
    // Runs dekit as its child, as npm's `dekit` script does: exec'd, it
    // would resume in a process that holds none of the runner's fds.
    (
      runner.script("wrapper", &format!("\"{DEKIT}\" \"$@\"\nexit $?\n")),
      "not run by the runner itself",
    ),
    // Replaced while it was being checked.
    (
      runner.script(
        "changing",
        &format!("echo '#' >> \"$0\"\nexec \"{DEKIT}\" \"$@\"\n"),
      ),
      "changed during the check",
    ),
  ];
  for (binary, error) in rejected {
    let out = runner.upgrade_to(&binary);
    assert!(!out.status.success(), "{} passed", binary.display());
    assert!(stderr(&out).contains(error), "{}", stderr(&out));
  }

  // Everything thawed: the task still runs, commands work, nothing left.
  assert!(runner.ok(&["ls"]).contains("ready"));
  assert_eq!(runner.record()["binary"], DEKIT);
  assert!(
    runner.snapshot_files().is_empty(),
    "snapshot file left behind"
  );
  runner.ok(&["spawn", "after", "--", "sleep", "60"]);
  wait_until("spawn after failed upgrade", || {
    runner.ok(&["ls"]).contains("after")
  });
  runner.stop();
}

#[tokio::test]
async fn failed_resume_kills_the_tasks_and_says_why() {
  let runner = TestRunner::start("upfail");
  // Survives the hangup of its terminal: only a kill ends it.
  let child = "trap '' HUP; echo pid $$; sleep 600";
  runner.ok(&["spawn", "ticker", "--", "sh", "-c", child]);
  wait_until("the child's pid", || {
    runner.ok(&["screen", "ticker"]).contains("pid ")
  });
  let pid = child_pid(&runner);

  // Passes the check, then resumes from a snapshot naming a listener fd
  // that was never inherited: a failure only the new image can hit.
  let breaking = runner.script(
    "breaking",
    &format!(
      r#"for arg; do
  case $arg in --check) exec "{DEKIT}" "$@";; esac
done
for arg; do
  [ "$prev" = --snapshot ] && snapshot=$arg
  prev=$arg
done
sed 's/"listener_fd":[0-9]*/"listener_fd":999/' "$snapshot" > "$snapshot.new"
mv "$snapshot.new" "$snapshot"
exec "{DEKIT}" "$@"
"#
    ),
  );
  let out = runner.upgrade_to(&breaking);
  assert!(!out.status.success());
  let error = stderr(&out);
  assert!(
    error.contains("could not resume the runner") && error.contains("fd 999"),
    "{error}"
  );

  // Nothing is left running without its runner, and status says why.
  wait_until("the child killed", || unsafe {
    libc::kill(pid as i32, 0) != 0
  });
  let status = runner.ok(&["--json", "runner", "status"]);
  let status: serde_json::Value = serde_json::from_str(&status).unwrap();
  assert_eq!(status["status"], "failed", "{status}");
}

async fn attach(
  runner: &TestRunner,
  target: &str,
) -> (ConnSender, ConnReceiver) {
  let socket = runner.record()["socket"]
    .as_str()
    .expect("socket path")
    .to_string();
  let (mut sender, mut receiver) = connect_socket(&socket).await.unwrap();
  client_handshake(&mut sender, &mut receiver).await.unwrap();
  let (method, params) = RpcRequest::Attach {
    target: target.parse().unwrap(),
    width: 80,
    height: 24,
    until_exit: false,
  }
  .to_wire();
  sender
    .send_ctl(CtlMsg::Request(Request {
      id: 1,
      method,
      params,
    }))
    .await
    .unwrap();
  match receiver.recv_ctl().await.unwrap() {
    CtlMsg::Response(response) => assert!(response.error.is_none()),
    msg => panic!("expected attach response, got {msg:?}"),
  }
  (sender, receiver)
}

/// Reads `Out` frames until one contains `needle`.
async fn saw(receiver: &mut ConnReceiver, needle: &[u8]) -> bool {
  tokio::time::timeout(Duration::from_secs(5), async {
    let mut out = Vec::new();
    loop {
      match receiver.recv().await {
        Some(Ok(Msg::Out(bytes))) => {
          out.extend_from_slice(&bytes);
          if out.windows(needle.len()).any(|w| w == needle) {
            return true;
          }
        }
        Some(Ok(Msg::Ctl(_))) => (),
        Some(Err(_)) | None => return false,
      }
    }
  })
  .await
  .unwrap_or(false)
}

async fn send_key(sender: &mut ConnSender, spec: &str) {
  let key = Key::parse(spec).unwrap();
  sender
    .send_ctl(CtlMsg::Event(Event {
      name: EVENT_INPUT.to_string(),
      params: serde_json::to_value(TermEvent::Key(key)).unwrap(),
    }))
    .await
    .unwrap();
}
