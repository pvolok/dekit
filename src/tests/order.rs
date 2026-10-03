//! Tasks are listed as the config has them, a group where its first task
//! is, and a task that is not in the config where it was added.

#![cfg(unix)]

mod common;
use common::{TestRunner, stderr_of as stderr};

const SLEEP: &str = "{cmd: [sleep, '60']}";

fn listed(runner: &TestRunner) -> Vec<String> {
  runner
    .ok(&["ls"])
    .lines()
    .filter_map(|line| line.split_whitespace().next())
    .map(str::to_string)
    .collect()
}

#[test]
fn ls_follows_the_config() {
  let runner = TestRunner::new("or");
  // `web/api` is registered after `cache`, which it depends on.
  runner.yaml(&format!(
    "tasks:\n  db: {SLEEP}\n  web/api: {{cmd: [sleep, '60'], deps: [cache]}}\n  \
     cache: {SLEEP}\n  web/ui: {SLEEP}\n"
  ));
  runner.start_runner();
  assert_eq!(listed(&runner), ["db", "web/api", "web/ui", "cache"]);

  runner.ok(&["spawn", "extra", "--", "sleep", "60"]);
  runner.ok(&["spawn", "web/more", "--", "sleep", "60"]);
  assert_eq!(
    listed(&runner),
    ["db", "web/api", "web/ui", "web/more", "cache", "extra"]
  );

  // An edited config reorders the list at a restart. A task it lacks
  // stays right after the one it was listed after, or first.
  runner.yaml(&format!(
    "tasks:\n  web/ui: {SLEEP}\n  first: {SLEEP}\n  cache: {SLEEP}\n  \
     web/api: {{cmd: [sleep, '60'], deps: [cache]}}\n"
  ));
  let edited = [
    "db", "web/ui", "web/more", "web/api", "first", "cache", "extra",
  ];
  let out = runner.run(&["runner", "restart"]);
  assert!(out.status.success(), "restart: {}", stderr(&out));
  assert_eq!(listed(&runner), edited);

  runner.ok(&["down"]);
  runner.ok(&["up"]);
  assert_eq!(listed(&runner), edited);

  runner.stop();
}
