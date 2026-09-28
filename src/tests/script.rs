//! `dekit script.js`: the module runs to the end, `main` is optional, and
//! the `std` functions behave as the JavaScript docs say.

use std::process::{Command, Output};

mod common;
use common::{DEKIT, TmpDir, stderr_of};

fn run(dir: &TmpDir, src: &str) -> Output {
  let script = dir.path.join("script.js");
  std::fs::write(&script, src).unwrap();
  Command::new(DEKIT)
    .arg(&script)
    .current_dir(&dir.path)
    .env("XDG_RUNTIME_DIR", &dir.path)
    .env("XDG_CONFIG_HOME", &dir.path)
    .env("XDG_DATA_HOME", &dir.path)
    .output()
    .unwrap()
}

#[test]
fn a_script_without_main_exits_zero() {
  let dir = TmpDir::new("jsnomain");
  let out = run(&dir, "std.log('hi');\n");
  assert!(out.status.success(), "{}", stderr_of(&out));
  assert_eq!(stderr_of(&out), "hi\n");
  assert!(out.stdout.is_empty());
}

#[test]
fn top_level_await_runs_to_the_end_before_main() {
  let dir = TmpDir::new("jstla");
  let out = run(
    &dir,
    "await std.fs.exists('.');\n\
     std.log('top');\n\
     export async function main() {\n\
     await std.fs.exists('.');\n\
     std.log('main');\n\
     }\n",
  );
  assert!(out.status.success(), "{}", stderr_of(&out));
  assert_eq!(stderr_of(&out), "top\nmain\n");
}

#[test]
fn a_top_level_rejection_exits_one() {
  let dir = TmpDir::new("jsreject");
  let out = run(
    &dir,
    "await std.fs.exists('.');\n\
     throw new Error('boom');\n\
     export function main() { std.log('main ran'); }\n",
  );
  assert_eq!(out.status.code(), Some(1));
  let error = stderr_of(&out);
  assert!(error.contains("boom"), "{error}");
  assert!(!error.contains("main ran"), "{error}");
}

#[test]
fn a_main_that_is_not_a_function_fails() {
  let dir = TmpDir::new("jsbadmain");
  let out = run(&dir, "export const main = 1;\n");
  assert_eq!(out.status.code(), Some(1));
  let error = stderr_of(&out);
  assert!(error.contains("not a function"), "{error}");
}

#[test]
fn log_prints_any_value() {
  let dir = TmpDir::new("jslog");
  let out = run(
    &dir,
    "std.log(42, 1.5, true, null, undefined, 'text', {a: 1, b: ['x']}, [1, 2]);\n\
     const cycle = {};\n\
     cycle.self = cycle;\n\
     std.warn(cycle, 10n);\n\
     std.error(new TypeError('bad'));\n",
  );
  assert!(out.status.success(), "{}", stderr_of(&out));
  let stderr = stderr_of(&out);
  let lines: Vec<&str> = stderr.lines().collect();
  assert_eq!(
    lines[0],
    r#"42 1.5 true null undefined text {"a":1,"b":["x"]} [1,2]"#
  );
  assert_eq!(lines[1], "[object Object] 10");
  assert_eq!(lines[2], "TypeError: bad");
}

#[cfg(unix)]
#[test]
fn stat_follows_symlinks_and_reports_them() {
  let dir = TmpDir::new("jsstat");
  std::fs::write(dir.path.join("file.txt"), "hello").unwrap();
  std::os::unix::fs::symlink("file.txt", dir.path.join("link")).unwrap();
  let out = run(
    &dir,
    "for (const path of ['file.txt', 'link']) {\n\
     const stat = await std.fs.stat(path);\n\
     std.log(path, stat.size, stat.isFile, stat.isSymlink);\n\
     }\n",
  );
  assert!(out.status.success(), "{}", stderr_of(&out));
  assert_eq!(stderr_of(&out), "file.txt 5 true false\nlink 5 true true\n");
}

#[cfg(unix)]
#[test]
fn exists_is_false_only_for_missing_paths() {
  use std::os::unix::fs::PermissionsExt;

  let dir = TmpDir::new("jsexists");
  let locked = dir.path.join("locked");
  std::fs::create_dir(&locked).unwrap();
  std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0))
    .unwrap();
  // Root is not stopped by the permissions.
  let denied = match std::fs::metadata(locked.join("file")) {
    Ok(_) => false,
    Err(err) => err.kind() == std::io::ErrorKind::PermissionDenied,
  };
  let out = run(
    &dir,
    "std.log(\n\
     await std.fs.exists('script.js'),\n\
     await std.fs.exists('missing'),\n\
     await std.fs.exists('script.js/under-a-file'),\n\
     );\n\
     try {\n\
     std.log(await std.fs.exists('locked/file'));\n\
     } catch (err) {\n\
     std.log(err.message);\n\
     }\n",
  );
  std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755))
    .unwrap();
  assert!(out.status.success(), "{}", stderr_of(&out));
  let stderr = stderr_of(&out);
  let lines: Vec<&str> = stderr.lines().collect();
  assert_eq!(lines[0], "true false false");
  if denied {
    assert!(lines[1].contains("Permission denied"), "{stderr}");
  }
}
