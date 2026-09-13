use std::path::{Path, PathBuf};

use anyhow::anyhow;
use clap::{Arg, Command as ClapCommand};
use rquickjs::CatchResultExt;

use crate::{
  attach_client::{AttachEnd, client_main},
  command::Command,
  config::task::{AUTOSTART_TAG, CmdConfig, is_script},
  dekit::{rpc_client::rpc_request, server::run_server},
  js::js_vm::JsVm,
  protocol::{
    ActResult, RpcRequest, RpcState, RpcWhy, ScreenResult, TaskListResult,
  },
  runner::{
    RunnerKind, RunnerSpec, clear_default_binary, lockfile,
    read_default_binary, resolve_kernel_binary, set_default_binary,
    socket::connect_client_socket,
  },
  target::{Runner, Target, parse_runner},
};

/// Render a wire state (token + optional exit detail) for humans.
fn human_state(s: &RpcState) -> String {
  match (s.exit_code, s.signal) {
    (Some(code), _) => format!("{} (code {})", s.state, code),
    (_, Some(signal)) => format!("{} (signal {})", s.state, signal),
    (None, None) => s.state.clone(),
  }
}

/// Report a selector verb result.
fn print_acted(
  result: serde_json::Value,
  json: bool,
  verb: &str,
  zero: &str,
) -> anyhow::Result<()> {
  if json {
    println!("{}", serde_json::to_string(&result)?);
    return Ok(());
  }
  let acted: ActResult = serde_json::from_value(result)?;
  match acted.matched {
    0 => println!("{}", zero),
    1 => println!("{} 1 task.", verb),
    n => println!("{} {} tasks.", verb, n),
  }
  Ok(())
}

fn print_task_list(
  result: serde_json::Value,
  json: bool,
) -> anyhow::Result<()> {
  let list: TaskListResult = serde_json::from_value(result)?;
  if json {
    println!("{}", serde_json::to_string(&list)?);
  } else if list.tasks.is_empty() {
    println!("No tasks.");
  } else {
    for t in &list.tasks {
      println!("{}\t{}", t.path, human_state(&t.state));
    }
  }
  Ok(())
}

fn print_why(result: serde_json::Value, json: bool) -> anyhow::Result<()> {
  let why: RpcWhy = serde_json::from_value(result)?;
  if json {
    println!("{}", serde_json::to_string(&why)?);
    return Ok(());
  }
  println!("{}: {}", why.path, human_state(&why.state));
  println!("  wanted: {}", why.wanted);
  if why.wanted && !why.supported {
    println!("  blocked: a dependency is not ready");
  }
  if why.vetoed {
    println!("  vetoed: yes (start it to clear)");
  }
  println!("  pinned: {}", why.pinned);
  if !why.required_by.is_empty() {
    println!("  required by: {}", why.required_by.join(", "));
  }
  if why.attempts > 0 {
    println!("  restart attempts: {}", why.attempts);
  }
  if !why.deps.is_empty() {
    println!("  deps:");
    for dep in &why.deps {
      let mut notes = Vec::new();
      if !dep.wanted {
        notes.push("not wanted");
      }
      if !dep.satisfied {
        notes.push("not satisfied");
      }
      let notes = if notes.is_empty() {
        String::new()
      } else {
        format!(" ({})", notes.join(", "))
      };
      println!("    {}\t{}{}", dep.path, human_state(&dep.state), notes);
    }
  }
  Ok(())
}

/// `--chdir` explicitly selects a project root. Processes spawned by a
/// runner (script tasks) inherit their runner's identity through
/// `DEKIT_RUNNER_ROOT`/`DEKIT_RUNNER_KIND`. Otherwise use the nearest
/// project; outside a project the host runner must be named explicitly.
fn resolve_runner(matches: &clap::ArgMatches) -> anyhow::Result<RunnerSpec> {
  if let Some(dir) = matches.get_one::<String>("chdir") {
    return RunnerSpec::project(Path::new(dir));
  }
  if let Some(root) = std::env::var_os(crate::runner::ENV_RUNNER_ROOT) {
    let kind = match std::env::var(crate::runner::ENV_RUNNER_KIND) {
      Ok(kind) => RunnerKind::from_name(&kind).ok_or_else(|| {
        anyhow!("bad {} '{kind}'", crate::runner::ENV_RUNNER_KIND)
      })?,
      Err(_) => RunnerKind::Project,
    };
    return RunnerSpec::exact(kind, Path::new(&root));
  }
  RunnerSpec::discover(&std::env::current_dir()?)
}

async fn shutdown_runner(runner: &RunnerSpec) -> anyhow::Result<()> {
  let paths = lockfile::runner_paths(runner)?;
  match lockfile::runner_state(runner, &paths)? {
    lockfile::RunnerState::Ready(_) | lockfile::RunnerState::Starting => {}
    lockfile::RunnerState::Stale(_) | lockfile::RunnerState::Failed(_) => {
      lockfile::cleanup_paths(&paths)?;
      anyhow::bail!("Runner is not running (stale runtime state cleaned up)")
    }
    lockfile::RunnerState::Absent => anyhow::bail!("No runner found"),
  }
  // The loop's first `Ready` read captures the target and sends Quit.
  let mut target = None;
  let mut quit_error = None;

  // A graceful Quit gets 21s; then the runner is force-killed by pid
  // and gets a few more seconds to release its lock as it dies.
  let quit_deadline =
    tokio::time::Instant::now() + std::time::Duration::from_secs(21);
  let kill_deadline = quit_deadline + std::time::Duration::from_secs(5);
  let mut killed = None;
  loop {
    match lockfile::runner_state(runner, &paths)? {
      lockfile::RunnerState::Absent => return Ok(()),
      lockfile::RunnerState::Stale(_) | lockfile::RunnerState::Failed(_) => {
        lockfile::cleanup_paths(&paths)?;
        return Ok(());
      }
      lockfile::RunnerState::Ready(record) => {
        if target.as_ref().is_some_and(|owner| owner != &record.owner) {
          return Ok(());
        }
        if target.is_none() {
          target = Some(record.owner.clone());
          quit_error = request_runner_quit(runner).await;
        }
        if killed.is_none() && tokio::time::Instant::now() >= quit_deadline {
          force_kill_runner(&record.owner)?;
          killed = Some(record.owner.pid);
        }
      }
      // A runner still Starting past the grace is wedged in bootstrap
      // (a hung on_init hook, say): it never bound a socket, so Quit
      // has nowhere to go — kill it by the identity it wrote to its
      // lock the instant it started.
      lockfile::RunnerState::Starting => {
        if killed.is_none() && tokio::time::Instant::now() >= quit_deadline {
          match lockfile::runner_owner(runner) {
            Some(owner) => {
              force_kill_runner(&owner)?;
              killed = Some(owner.pid);
            }
            None => anyhow::bail!(
              "runner is wedged starting up but wrote no identity to kill"
            ),
          }
        }
      }
    }
    if tokio::time::Instant::now() >= kill_deadline {
      break;
    }
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
  }

  let message = match killed {
    Some(pid) => {
      format!("killed the runner (pid {pid}), but its lock is still held")
    }
    None => {
      "runner did not stop within 21s and published no pid to kill".to_string()
    }
  };
  match quit_error {
    Some(quit_error) => Err(quit_error.context(message)),
    None => Err(anyhow!(message)),
  }
}

/// Force-kill a runner by verified identity and reap its task session.
fn force_kill_runner(owner: &lockfile::OwnerInfo) -> anyhow::Result<()> {
  let lockfile::OwnerInfo {
    pid,
    start_time,
    sid,
  } = *owner;
  if crate::runner::kill::kill_verified(pid, start_time)? {
    eprintln!("Runner did not stop within 21s; killed pid {pid}.");
    // The spawned runner owns its session and every task inherits it;
    // reap them so the kill leaves no orphans. A foreground runner
    // (sid != pid) shares the user's session, never swept.
    #[cfg(unix)]
    if sid == pid {
      match crate::runner::kill::kill_session(sid) {
        Ok(0) => {}
        Ok(left) => eprintln!("{left} task process(es) survived the reap."),
        Err(err) => eprintln!("Failed to reap the runner's tasks: {err:#}"),
      }
    }
    #[cfg(not(unix))]
    let _ = sid;
  }
  Ok(())
}

async fn request_runner_quit(runner: &RunnerSpec) -> Option<anyhow::Error> {
  match tokio::time::timeout(
    std::time::Duration::from_secs(2),
    rpc_request(runner, RpcRequest::Command(Command::Quit), false),
  )
  .await
  {
    Ok(result) => result.err(),
    Err(_) => Some(anyhow!("Quit request timed out after 2s")),
  }
}

/// The record's own JSON, minus the file-format `schema` field: the one
/// base object every `runner list`/`runner status` rendering builds on.
fn record_json(
  record: &lockfile::RunnerRecord,
) -> serde_json::Map<String, serde_json::Value> {
  let value = serde_json::to_value(record).expect("record serializes");
  let serde_json::Value::Object(mut map) = value else {
    unreachable!("record serializes to an object")
  };
  map.remove("schema");
  map
}

fn runner_json(info: &lockfile::RunnerInfo) -> serde_json::Value {
  let mut map = record_json(&info.contents);
  map.insert("running".to_string(), info.is_running.into());
  serde_json::Value::Object(map)
}

async fn start_runner(runner: &RunnerSpec) -> anyhow::Result<()> {
  match lockfile::get_runner_state(runner)? {
    lockfile::RunnerState::Ready(record) => {
      println!("Runner already running (pid={}).", record.owner.pid);
      return Ok(());
    }
    lockfile::RunnerState::Absent
    | lockfile::RunnerState::Starting
    | lockfile::RunnerState::Stale(_)
    | lockfile::RunnerState::Failed(_) => {}
  };
  let connection = connect_client_socket(runner, true).await?;
  drop(connection);
  println!("Runner started for {}.", runner.root.display());
  match lockfile::get_runner_state(runner)? {
    lockfile::RunnerState::Ready(record) => print_warnings(&record.warnings),
    lockfile::RunnerState::Absent
    | lockfile::RunnerState::Starting
    | lockfile::RunnerState::Stale(_)
    | lockfile::RunnerState::Failed(_) => {}
  }
  Ok(())
}

pub fn print_warnings(warnings: &[String]) {
  for warning in warnings {
    eprintln!("Warning: {warning}");
  }
}

/// A `Command::Add` from `spawn`/`run` arguments.
fn add_command(
  sub_m: &clap::ArgMatches,
  target: Target,
) -> anyhow::Result<Command> {
  let cwd = match sub_m.get_one::<String>("cwd") {
    Some(cwd) => cwd.clone(),
    None => std::env::current_dir()?.to_string_lossy().into_owned(),
  };
  let cmd: Vec<String> =
    sub_m.get_many::<String>("cmd").unwrap().cloned().collect();
  let deps = sub_m
    .get_many::<String>("dep")
    .into_iter()
    .flatten()
    .map(|dep| {
      let dep = dep.parse::<Target>()?;
      if dep.runner().is_some() {
        anyhow::bail!("dep '{}': deps live in the task's own runner", dep);
      }
      Ok(dep)
    })
    .collect::<anyhow::Result<Vec<_>>>()?;
  let tags: Vec<String> = sub_m
    .get_many::<String>("tag")
    .map(|v| v.cloned().collect())
    .unwrap_or_default();
  let mut env = std::collections::BTreeMap::new();
  if let Some(vals) = sub_m.get_many::<String>("env") {
    for val in vals {
      let (k, v) = val
        .split_once('=')
        .ok_or_else(|| anyhow!("--env expects KEY=VALUE, got `{}`", val))?;
      env.insert(k.to_string(), Some(v.to_string()));
    }
  }
  Ok(Command::Add {
    target,
    label: None,
    cmd: CmdConfig::Cmd { cmd },
    cwd: Some(cwd),
    env: if env.is_empty() { None } else { Some(env) },
    deps,
    tags,
  })
}

/// The exit code a foreground `run` should end with, from the final
/// state the runner reported in the `task_exited` bye.
fn run_exit_code(state: Option<&RpcState>) -> i32 {
  let Some(state) = state else {
    return 1;
  };
  if let Some(code) = state.exit_code {
    return code;
  }
  if let Some(signal) = state.signal {
    return 128 + signal;
  }
  if state.state == "done" { 0 } else { 1 }
}

fn arg_target(sub_m: &clap::ArgMatches) -> anyhow::Result<Option<Target>> {
  sub_m
    .get_one::<String>("target")
    .map(|target| target.parse::<Target>().map_err(Into::into))
    .transpose()
}

/// Resolves a target's runner to the working dir to talk to, and returns
/// the target as the runner sees it.
fn resolve_target(
  matches: &clap::ArgMatches,
  target: Target,
) -> anyhow::Result<(RunnerSpec, Target)> {
  let chdir = matches.get_one::<String>("chdir");
  let runner = match (target.runner(), chdir) {
    (Some(_), Some(_)) => {
      anyhow::bail!("use either --chdir or a runner qualifier, not both")
    }
    (None, _) => resolve_runner(matches)?,
    (Some(runner), None) => runner_from_ref(runner)?,
  };
  Ok((runner, target.without_runner()))
}

fn runner_from_ref(runner: &Runner) -> anyhow::Result<RunnerSpec> {
  match runner {
    Runner::Name(name) => match RunnerKind::from_name(name) {
      Some(RunnerKind::Project) => {
        match crate::runner::find_project_root(&std::env::current_dir()?) {
          Some(dir) => RunnerSpec::project(&dir),
          None => anyhow::bail!(
            "no project found above the current dir (looked for dekit.yaml, a git repo, or package.json)"
          ),
        }
      }
      Some(RunnerKind::Host) => RunnerSpec::host(),
      None => anyhow::bail!("unknown runner '{}'", name),
    },
    Runner::Path(path) => {
      let path = match path.strip_prefix("~/") {
        Some(rest) => match std::env::var_os("HOME") {
          Some(home) => PathBuf::from(home).join(rest),
          None => anyhow::bail!("HOME is not set"),
        },
        None => PathBuf::from(path),
      };
      RunnerSpec::project(&path)
    }
    Runner::Url(url) => {
      anyhow::bail!("remote runners are not available yet: {}", url)
    }
  }
}

/// Runner selection for `dekit runner` subcommands: an explicit runner
/// reference (`home`, `project`, a path), else the discovered runner.
fn arg_runner(
  matches: &clap::ArgMatches,
  sub_m: &clap::ArgMatches,
) -> anyhow::Result<RunnerSpec> {
  match sub_m.get_one::<String>("runner") {
    Some(reference) => {
      if matches.get_one::<String>("chdir").is_some() {
        anyhow::bail!("use either --chdir or a runner reference, not both")
      }
      runner_from_ref(&parse_runner(reference)?)
    }
    None => resolve_runner(matches),
  }
}

pub async fn dekit_main() -> anyhow::Result<()> {
  let target_arg = |help: &'static str| Arg::new("target").help(help);
  let required_target = || {
    Arg::new("target")
      .required(true)
      .help("Task path, glob, or +tag")
  };
  let runner_ref_arg = || {
    Arg::new("runner")
      .help("Runner: host, project, or a path (default: the discovered runner)")
  };
  // Shared by `spawn` and `run`: a new task from a command line.
  let task_args = |command: ClapCommand| {
    command
      .arg(
        Arg::new("target")
          .required(true)
          .help("Task path (e.g. services/web)"),
      )
      .arg(
        Arg::new("cwd")
          .long("cwd")
          .help("Working directory for the task (default: current dir)"),
      )
      .arg(
        Arg::new("env")
          .long("env")
          .action(clap::ArgAction::Append)
          .help("Set an environment variable, KEY=VALUE (repeatable)"),
      )
      .arg(
        Arg::new("dep")
          .long("dep")
          .action(clap::ArgAction::Append)
          .help("Depend on existing tasks matching a target (repeatable)"),
      )
      .arg(
        Arg::new("tag")
          .long("tag")
          .action(clap::ArgAction::Append)
          .help("Tag the task (repeatable)"),
      )
      .arg(
        Arg::new("cmd")
          .required(true)
          .num_args(1..)
          .last(true)
          .help("Command to run"),
      )
  };
  let runner_command = ClapCommand::new("runner")
    .about("Manage independent runners")
    .subcommands([
      ClapCommand::new("run")
        .about("Run a runner in the foreground")
        .arg(
          Arg::new("dir")
            .long("dir")
            .required(true)
            .help("Canonical working root this runner manages"),
        )
        .arg(
          Arg::new("kind")
            .long("kind")
            .default_value("project")
            .value_parser(["project", "host"])
            .hide(true),
        )
        .arg(
          Arg::new("log-level")
            .long("log-level")
            .help("Diagnostic log level"),
        ),
      ClapCommand::new("start")
        .about("Start the selected runner")
        .arg(runner_ref_arg()),
      ClapCommand::new("stop")
        .about("Stop the selected runner")
        .arg(runner_ref_arg()),
      ClapCommand::new("status")
        .about("Show selected runner status")
        .arg(runner_ref_arg()),
      ClapCommand::new("list").about("List published runner records"),
      ClapCommand::new("clean").about("Remove stale runtime records"),
    ]);
  let cmd = clap::command!()
    .subcommands([
      ClapCommand::new("attach")
        .about("Attach the terminal to a task's screen (default: the console)")
        .arg(target_arg("Task screen to attach to"))
        .arg(
          Arg::new("no-start")
            .long("no-start")
            .action(clap::ArgAction::SetTrue)
            .help("Fail if the runner is not running instead of starting it"),
        ),
      ClapCommand::new("up")
        .about("Start autostart tasks, or tasks matching a target")
        .arg(target_arg("Task path, glob, or +tag")),
      ClapCommand::new("down")
        .about("Unpin tasks (bare: all); each stops unless something still needs it")
        .arg(target_arg("Task path, glob, or +tag")),
      task_args(
        ClapCommand::new("spawn").about("Add a task at a path and start it"),
      ),
      task_args(ClapCommand::new("run").about(
        "Run a command as a task in the foreground; removed when it exits",
      )),
      ClapCommand::new("ls")
        .about("List tasks")
        .arg(target_arg("Task path, glob, or +tag (default: everything)")),
      ClapCommand::new("start")
        .about("Start tasks matching a target")
        .arg(required_target()),
      ClapCommand::new("stop")
        .about("Unpin and stop tasks; each restarts if something still needs it")
        .arg(required_target()),
      ClapCommand::new("kill")
        .about("Like stop, but with an immediate hard kill")
        .arg(required_target()),
      ClapCommand::new("veto")
        .about("Force tasks down and keep them down until started again")
        .arg(required_target()),
      ClapCommand::new("restart")
        .about("Restart tasks matching a target")
        .arg(required_target()),
      ClapCommand::new("rm")
        .about("Remove tasks, killing running ones")
        .arg(required_target()),
      ClapCommand::new("why")
        .about("Explain why a task is (not) running")
        .arg(Arg::new("target").required(true).help("Task path")),
      ClapCommand::new("screen")
        .about("Print the current screen of a task")
        .arg(Arg::new("target").required(true).help("Task path")),
      runner_command,
      ClapCommand::new("kernel")
        .about("Inspect kernel selection and manage the user default")
        .subcommands([
          ClapCommand::new("status")
            .about("Show the selected and user-default kernels"),
          ClapCommand::new("set-default")
            .about("Register the default dekit binary")
            .arg(Arg::new("path").help("Binary path (default: this binary)")),
          ClapCommand::new("clear-default")
            .about("Clear the registered default binary"),
        ]),
      ClapCommand::new("mprocs")
        .about("Run the legacy mprocs CLI (mprocs.yaml, --ctl, etc.)")
        .disable_help_flag(true)
        .arg(
          Arg::new("args")
            .num_args(0..)
            .trailing_var_arg(true)
            .allow_hyphen_values(true),
        ),
    ])
    .arg(
      Arg::new("chdir")
        .long("chdir")
        .short('C')
        .global(true)
        .help(
          "Explicit project root (default: nearest dekit.yaml, git repo, or package.json)",
        ),
    )
    .arg(
      Arg::new("json")
        .long("json")
        .global(true)
        .action(clap::ArgAction::SetTrue)
        .help("Emit machine-readable JSON instead of text"),
    )
    .arg(
      Arg::new("files")
        .action(clap::ArgAction::Append)
        .trailing_var_arg(true)
        .help("A .js script to run; with no command, attach to the console"),
    )
    .after_help(
      "TARGETS\n  \
       A target is a task path (services/web), a glob (services/*, **), or\n  \
       a +tag (+backend), optionally in a space (@dekit/console, @*/web)\n  \
       and a runner (project::web, /abs/dir::+ci). The surgical verbs\n  \
       (start, stop, kill, veto, restart, rm) require a target; the workday\n  \
       verbs (up, down) default to the autostart set / everything. spawn\n  \
       adds a task from a command line; run does the same in the foreground\n  \
       and removes the task when it exits.\n\
       \n\
       BRINGING TASKS DOWN\n  \
       stop  unpins and stops now; a task restarts if a dependent still needs it.\n  \
       down  unpins only; a task keeps running while something still needs it.\n  \
       veto  forces a task down and holds it there until it is started again.\n  \
       kill  is stop with an immediate hard kill.",
    );
  let matches = cmd.get_matches();
  let json = matches.get_flag("json");

  if let Some(("mprocs", sub_m)) = matches.subcommand() {
    let args: Vec<String> = sub_m
      .get_many::<String>("args")
      .map(|vals| vals.cloned().collect())
      .unwrap_or_default();
    let mut argv = vec!["mprocs".to_string()];
    argv.extend(args);
    return crate::mprocs::mprocs::run_app(argv).await;
  }

  let console = || "@dekit/console".parse::<Target>().expect("valid target");

  match matches.subcommand() {
    Some(("attach", sub_m)) => {
      let target = arg_target(sub_m)?.unwrap_or_else(console);
      let (runner, target) = resolve_target(&matches, target)?;
      let start = !sub_m.get_flag("no-start");
      let (sender, receiver) = connect_client_socket(&runner, start).await?;
      client_main(target, false, sender, receiver).await?;
    }
    Some(("spawn", sub_m)) => {
      let target = arg_target(sub_m)?.expect("clap requires target");
      let (runner, target) = resolve_target(&matches, target)?;
      let name = target.to_string();
      let command = add_command(sub_m, target)?;
      let result =
        rpc_request(&runner, RpcRequest::Command(command), true).await?;
      if json {
        println!("{}", serde_json::to_string(&result)?);
      } else {
        println!("Spawned {}.", name);
      }
    }
    Some(("run", sub_m)) => {
      let target = arg_target(sub_m)?.expect("clap requires target");
      let (runner, target) = resolve_target(&matches, target)?;
      let command = add_command(sub_m, target.clone())?;
      rpc_request(&runner, RpcRequest::Command(command), true).await?;
      let (sender, receiver) = connect_client_socket(&runner, false).await?;
      // The runner reaps the task itself on exit (it is until_exit), so
      // a client that dies mid-run never leaks it.
      match client_main(target.clone(), true, sender, receiver).await? {
        AttachEnd::TaskExited(state) => {
          std::process::exit(run_exit_code(state.as_ref()));
        }
        AttachEnd::Detached => {
          eprintln!(
            "Detached; {target} keeps running (remove with `dekit rm {target}`)."
          );
        }
      }
    }
    Some(("ls", sub_m)) => {
      let target = arg_target(sub_m)?.unwrap_or_else(|| Target::glob("**"));
      let (runner, target) = resolve_target(&matches, target)?;
      let result = rpc_request(
        &runner,
        RpcRequest::Ls {
          target: Some(target),
        },
        false,
      )
      .await?;
      print_task_list(result, json)?;
    }
    Some((
      verb @ ("start" | "stop" | "kill" | "veto" | "restart" | "rm"),
      sub_m,
    )) => {
      let target = arg_target(sub_m)?.expect("clap requires target");
      let (runner, target) = resolve_target(&matches, target)?;
      let (command, spawn, done) = match verb {
        "start" => (Command::Start { target }, true, "Started"),
        "stop" => (Command::Stop { target }, false, "Stopped"),
        "kill" => (Command::Kill { target }, false, "Killed"),
        "veto" => (Command::Veto { target }, false, "Vetoed"),
        "restart" => (Command::Restart { target }, true, "Restarted"),
        _ => (Command::Remove { target }, false, "Removed"),
      };
      let result =
        rpc_request(&runner, RpcRequest::Command(command), spawn).await?;
      print_acted(result, json, done, "No tasks matched.")?;
    }
    Some(("why", sub_m)) => {
      let target = arg_target(sub_m)?.expect("clap requires target");
      let (runner, target) = resolve_target(&matches, target)?;
      let result =
        rpc_request(&runner, RpcRequest::Why { target }, false).await?;
      print_why(result, json)?;
    }
    Some(("screen", sub_m)) => {
      let target = arg_target(sub_m)?.expect("clap requires target");
      let (runner, target) = resolve_target(&matches, target)?;
      let result =
        rpc_request(&runner, RpcRequest::Screen { target }, false).await?;
      let screen: ScreenResult = serde_json::from_value(result)?;
      if json {
        println!("{}", serde_json::to_string(&screen)?);
      } else {
        print!("{}", screen.screen);
        // Reset terminal attributes after printing
        println!("{}", crate::term::vt::emit::SGR_RESET);
      }
    }
    Some(("up", sub_m)) => {
      let target =
        arg_target(sub_m)?.unwrap_or_else(|| Target::tag(AUTOSTART_TAG));
      let (runner, target) = resolve_target(&matches, target)?;
      let result = rpc_request(
        &runner,
        RpcRequest::Command(Command::Start { target }),
        true,
      )
      .await?;
      print_acted(result, json, "Started", "No tasks matched.")?;
    }
    Some(("down", sub_m)) => {
      let target = arg_target(sub_m)?.unwrap_or_else(|| Target::glob("**"));
      let (runner, target) = resolve_target(&matches, target)?;
      let result = rpc_request(
        &runner,
        RpcRequest::Command(Command::Down { target }),
        false,
      )
      .await?;
      print_acted(result, json, "Put down", "No tasks matched.")?;
    }
    Some(("runner", sub_m)) => match sub_m.subcommand() {
      Some(("run", run_m)) => {
        let dir = run_m.get_one::<String>("dir").unwrap();
        let root = dunce::canonicalize(dir)
          .map_err(|err| anyhow!("invalid runner root `{dir}`: {err}"))?;
        let kind = run_m
          .get_one::<String>("kind")
          .and_then(|kind| RunnerKind::from_name(kind))
          .unwrap_or(RunnerKind::Project);
        let runner = RunnerSpec::exact(kind, &root)?;
        let log_level =
          run_m.get_one::<String>("log-level").map(String::as_str);
        run_server(runner, log_level).await?;
      }
      Some(("start", sub_m)) => {
        let runner = arg_runner(&matches, sub_m)?;
        start_runner(&runner).await?;
      }
      Some(("stop", sub_m)) => {
        let runner = arg_runner(&matches, sub_m)?;
        shutdown_runner(&runner).await?;
        println!("Runner stopped.");
      }
      Some(("status", sub_m)) => {
        let runner = arg_runner(&matches, sub_m)?;
        let state = lockfile::get_runner_state(&runner)?;
        let selected = resolve_kernel_binary(&runner);
        if matches.get_flag("json") {
          let selected_path = selected.as_ref().ok();
          let selected_error =
            selected.as_ref().err().map(|err| format!("{err:#}"));
          let record_value = |record: &lockfile::RunnerRecord,
                              running: bool| {
            let restart_required = running
              && selected_path
                .is_some_and(|path| path != Path::new(&record.binary));
            let mut map = record_json(record);
            map.insert(
              "status".to_string(),
              if running { "running" } else { "stale" }.into(),
            );
            map.insert("restart_required".to_string(), restart_required.into());
            serde_json::Value::Object(map)
          };
          let mut value = match state {
            lockfile::RunnerState::Ready(record) => record_value(&record, true),
            lockfile::RunnerState::Stale(record) => {
              record_value(&record, false)
            }
            lockfile::RunnerState::Starting => {
              serde_json::json!({"status": "starting"})
            }
            lockfile::RunnerState::Failed(error) => {
              serde_json::json!({"status": "failed", "error": error})
            }
            lockfile::RunnerState::Absent => {
              serde_json::json!({"status": "absent"})
            }
          };
          let map = value.as_object_mut().expect("status is an object");
          map.insert(
            "selected_binary".to_string(),
            serde_json::to_value(selected_path)?,
          );
          map.insert(
            "selection_error".to_string(),
            serde_json::to_value(&selected_error)?,
          );
          println!("{}", serde_json::to_string(&value)?);
        } else {
          match state {
            lockfile::RunnerState::Ready(record) => {
              println!(
                "[running] pid={} socket={} version={} kernel={}",
                record.owner.pid, record.socket, record.version, record.binary,
              );
              match &selected {
                Ok(selected) if Path::new(&record.binary) != selected => {
                  println!(
                    "Restart required to use selected kernel {}.",
                    selected.display()
                  );
                }
                Err(error) => println!("Kernel selection error: {error:#}"),
                _ => {}
              }
              print_warnings(&record.warnings);
            }
            lockfile::RunnerState::Stale(record) => println!(
              "[stale] pid={} socket={} version={} kernel={}",
              record.owner.pid, record.socket, record.version, record.binary,
            ),
            lockfile::RunnerState::Starting => println!("Runner is starting."),
            lockfile::RunnerState::Failed(error) => {
              println!("Runner failed to start: {error}")
            }
            lockfile::RunnerState::Absent => println!("No runner."),
          }
        }
      }
      Some(("list", _sub_m)) => {
        let runners = lockfile::list_runners()?;
        if matches.get_flag("json") {
          let arr: Vec<_> = runners.iter().map(runner_json).collect();
          println!("{}", serde_json::to_string(&arr)?);
        } else if runners.is_empty() {
          println!("No runners found.");
        } else {
          for d in &runners {
            let status = if d.is_running { "running" } else { "stale" };
            println!(
              "[{}] {} pid={} root={} socket={} version={}",
              status,
              d.contents.kind.as_str(),
              d.contents.owner.pid,
              d.contents.root,
              d.contents.socket,
              d.contents.version,
            );
          }
        }
      }
      Some(("clean", _sub_m)) => {
        let count = lockfile::cleanup_all_stale()?;
        println!("Removed {} stale runtime record(s).", count);
      }
      _ => {
        anyhow::bail!(
          "expected a subcommand after `dekit runner` (run, start, stop, status, list, clean)"
        );
      }
    },
    Some(("kernel", sub_m)) => match sub_m.subcommand() {
      Some(("status", _)) => {
        let runner = resolve_runner(&matches)?;
        let selected = resolve_kernel_binary(&runner);
        let default = read_default_binary()?;
        let active = match lockfile::get_runner_state(&runner)? {
          lockfile::RunnerState::Ready(record) => {
            Some(PathBuf::from(record.binary))
          }
          _ => None,
        };
        let restart_required = selected.as_ref().ok().is_some_and(|selected| {
          active.as_deref().is_some_and(|path| path != selected)
        });
        if json {
          println!(
            "{}",
            serde_json::to_string(&serde_json::json!({
              "selected": selected.as_ref().ok(),
              "selection_error": selected.as_ref().err().map(|err| format!("{err:#}")),
              "default": default,
              "active": active,
              "restart_required": restart_required,
            }))?
          );
        } else {
          match &selected {
            Ok(path) => println!("Selected: {}", path.display()),
            Err(error) => println!("Selected: error ({error:#})"),
          }
          match active {
            Some(path) => println!("Active:   {}", path.display()),
            None => println!("Active:   not running"),
          }
          match default {
            Some(path) => println!("Default:  {}", path.display()),
            None => println!("Default:  not registered"),
          }
          if restart_required {
            println!("Restart required to use the selected kernel.");
          }
        }
      }
      Some(("set-default", set_m)) => {
        let path = set_m
          .get_one::<String>("path")
          .map(PathBuf::from)
          .unwrap_or(std::env::current_exe()?);
        let path = set_default_binary(&path)?;
        println!("Default dekit kernel: {}", path.display());
      }
      Some(("clear-default", _)) => {
        clear_default_binary()?;
        println!("Default dekit kernel cleared.");
      }
      _ => anyhow::bail!(
        "expected `status`, `set-default`, or `clear-default` after `dekit kernel`"
      ),
    },
    Some((arg, _sub_m)) => {
      anyhow::bail!("unknown command: {}", arg);
    }
    None => {
      let paths = matches
        .get_many::<String>("files")
        .map(|p| p.collect::<Vec<_>>())
        .unwrap_or_default();

      if let Some(first) = paths.first() {
        // .js
        if is_script(Path::new(first)) {
          let src = std::fs::read_to_string(first)?;

          // A standalone script runs anywhere; std.dekit calls fail
          // lazily if there is no runner to act on.
          let vm = JsVm::new(resolve_runner(&matches).ok()).await?;
          let root = vm
            .eval_file(Path::new(first.as_str()), src.as_bytes())
            .await?;

          rquickjs::async_with!(vm.context => |ctx| {
            run_module_main(&ctx, &root).await
          })
          .await?;
        } else {
          anyhow::bail!(
            "unknown command or unsupported file: `{}` (expected a subcommand or a .js script)",
            first
          );
        }
      } else {
        // No args: same as `attach`.
        let runner = resolve_runner(&matches)?;
        let (sender, receiver) = connect_client_socket(&runner, true).await?;
        client_main(console(), false, sender, receiver).await?;
      }
    }
  }

  Ok(())
}

async fn run_module_main(
  ctx: &rquickjs::Ctx<'_>,
  root: &rquickjs::Persistent<rquickjs::Object<'static>>,
) -> anyhow::Result<()> {
  let m = map_js_error(
    ctx,
    root.clone().restore(ctx),
    "Failed to restore module namespace",
  )?;
  let main = map_js_error(
    ctx,
    m.get::<_, rquickjs::Value>("main"),
    "Failed to read exported `main`",
  )?;

  let val = match main.type_of() {
    rquickjs::Type::Constructor => map_js_error(
      ctx,
      main
        .into_constructor()
        .expect("Type checked as constructor")
        .call::<_, rquickjs::Value>(()),
      "Error while calling exported constructor `main`",
    )?,
    rquickjs::Type::Function => map_js_error(
      ctx,
      main
        .into_function()
        .expect("Type checked as function")
        .call(()),
      "Error while calling exported function `main`",
    )?,
    t => anyhow::bail!("Exported `main` is not a function ({}).", t.as_str()),
  };

  let val = if let Some(promise) = val.clone().into_promise() {
    map_js_error(
      ctx,
      promise.into_future::<rquickjs::Value<'_>>().await,
      "Unhandled rejection in exported `main`",
    )?
  } else {
    val
  };

  println!("-> {:?}", val);
  Ok(())
}

fn map_js_error<T>(
  ctx: &rquickjs::Ctx<'_>,
  result: rquickjs::Result<T>,
  scope: &str,
) -> anyhow::Result<T> {
  result.catch(ctx).map_err(|err| anyhow!("{scope}:\n{err}"))
}
