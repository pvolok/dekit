//! `dekit logs`: the lines tasks printed, merged, and followed with `-f`.

#![cfg(unix)]

use std::io::{BufRead, BufReader};
use std::sync::mpsc;
use std::time::Duration;

mod common;
use common::{TestRunner, stderr_of, wait_until};

const TASKS: &str = r#"tasks:
  web:
    cmd: [sh, -c, 'echo web one; echo web two; sleep 60']
    autostart: true
  job:
    cmd: [sh, -c, 'echo working; exit 3']
    type: job
  bar:
    cmd: [printf, "frame 1\\rframe 2\\r\\033[Kdone\\nhalf a line"]
    type: job
"#;

fn logs(runner: &TestRunner, args: &[&str]) -> String {
  runner.ok(&[&["logs"], args].concat())
}

/// Up, once web has printed its lines: ready only says it started.
fn up(runner: &TestRunner) {
  runner.ok(&["up", "--wait"]);
  wait_until("web's lines", || logs(runner, &["web"]).contains("web two"));
}

#[test]
fn logs_merge_the_tasks_lines_after_their_names() {
  let runner = TestRunner::new("lm");
  runner.yaml(TASKS);
  up(&runner);
  runner.ok(&["start", "job"]);
  runner.ok(&["start", "bar"]);
  wait_until("the job's exit", || {
    logs(&runner, &["job"]).contains("exited")
  });
  wait_until("bar's exit", || logs(&runner, &["bar"]).contains("exited"));

  let web = logs(&runner, &["web"]);
  assert_eq!(web, "web | --- started\nweb | web one\nweb | web two\n");

  // The real exit, after the last line; padded to the widest name.
  let job = logs(&runner, &["job", "web"]);
  assert!(
    job.contains("job | working\njob | --- exited (code 3)\n"),
    "{job}"
  );

  // A `\r` redraw is one line, as it ended; a last line with no newline
  // is logged at the exit.
  let bar = logs(&runner, &["bar", "--no-prefix"]);
  assert_eq!(bar, "--- started\ndone\nhalf a line\n--- exited (code 0)\n");

  let last = logs(&runner, &["-n", "2"]);
  assert_eq!(last.lines().count(), 2, "{last}");
  assert_eq!(logs(&runner, &["web", "--since", "1h"]), web);

  let out = runner.run(&["logs", "nope"]);
  assert_eq!(out.status.code(), Some(1));
  assert!(stderr_of(&out).contains("no task matches 'nope'"));
  runner.stop();
}

#[test]
fn json_is_one_record_per_line() {
  let runner = TestRunner::new("lj");
  runner.yaml(TASKS);
  up(&runner);
  let out = logs(&runner, &["--json", "web"]);
  let records: Vec<serde_json::Value> = out
    .lines()
    .map(|line| serde_json::from_str(line).unwrap())
    .collect();
  assert_eq!(records.len(), 3, "{out}");
  assert_eq!(records[0]["kind"], "started");
  assert_eq!(records[1]["kind"], "line");
  assert_eq!(records[1]["task"], "web");
  assert_eq!(records[1]["text"], "web one");
  assert!(records[1]["seq"].as_u64() > records[0]["seq"].as_u64());
  assert!(records[1]["time_ms"].is_u64());
  runner.stop();
}

/// `dekit logs` with `args`, and its stdout's lines as they come.
fn follow(
  runner: &TestRunner,
  args: &[&str],
) -> (std::process::Child, impl Fn() -> String) {
  let mut child = runner.spawn(&[&["logs", "-f"], args].concat());
  let stdout = child.stdout.take().unwrap();
  let (tx, rx) = mpsc::channel();
  std::thread::spawn(move || {
    for line in BufReader::new(stdout).lines() {
      if tx.send(line.unwrap()).is_err() {
        break;
      }
    }
  });
  let next = move || rx.recv_timeout(Duration::from_secs(10)).unwrap();
  (child, next)
}

#[test]
fn follow_goes_on_across_a_restart_until_ctrl_c() {
  let runner = TestRunner::new("lf");
  runner.yaml(TASKS);
  up(&runner);
  let (mut child, next) = follow(&runner, &["web", "-n", "1"]);
  assert_eq!(next(), "web | web two");

  runner.ok(&["restart", "web", "--wait"]);
  assert!(next().starts_with("web | --- exited"));
  assert_eq!(next(), "web | --- started");
  assert_eq!(next(), "web | web one");
  assert_eq!(next(), "web | web two");

  unsafe { libc::kill(child.id() as i32, libc::SIGINT) };
  let status = child.wait().unwrap();
  assert!(status.success(), "{status}");
  runner.stop();
}

#[test]
fn follow_ends_with_an_error_when_the_runner_stops() {
  let runner = TestRunner::new("ls");
  runner.yaml(TASKS);
  up(&runner);
  let (child, next) = follow(&runner, &["web", "-n", "1"]);
  // The backlog is in: it follows.
  assert_eq!(next(), "web | web two");
  runner.stop();
  let out = child.wait_with_output().unwrap();
  assert_eq!(out.status.code(), Some(1));
  assert!(stderr_of(&out).contains("(quit)"), "{}", stderr_of(&out));
}
