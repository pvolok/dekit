use std::path::{Path, PathBuf};

use anyhow::anyhow;
use clap::{Arg, Command};
use rquickjs::CatchResultExt;

use crate::{
  attach_client::client_main,
  daemon::{
    lockfile, socket::connect_client_socket, spawn::spawn_server_daemon,
  },
  dekit::{rpc_client::rpc_request, server::run_server},
  js::js_vm::JsVm,
  protocol::{RpcRequest, RpcWhy, ScreenResult, TaskListResult},
};

/// Report a mutation result: the raw JSON under `--json`, a short
/// confirmation otherwise.
fn print_done(
  result: serde_json::Value,
  json: bool,
  msg: &str,
) -> anyhow::Result<()> {
  if json {
    println!("{}", serde_json::to_string(&result)?);
  } else {
    println!("{}", msg);
  }
  Ok(())
}

fn print_task_list(
  result: serde_json::Value,
  json: bool,
) -> anyhow::Result<()> {
  let list: TaskListResult = serde_json::from_value(result)?;
  if json {
    println!("{}", serde_json::to_string(&list.tasks)?);
  } else if list.tasks.is_empty() {
    println!("No tasks.");
  } else {
    for t in &list.tasks {
      println!("{}\t{}", t.path, t.state);
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
  println!("{}: {}", why.path, why.state);
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
      println!("    {}\t{}{}", dep.path, dep.state, notes);
    }
  }
  Ok(())
}

fn resolve_working_dir(matches: &clap::ArgMatches) -> anyhow::Result<PathBuf> {
  match matches.get_one::<String>("chdir") {
    Some(dir) => std::fs::canonicalize(dir)
      .map_err(|e| anyhow!("invalid --chdir `{}`: {}", dir, e)),
    None => Ok(std::env::current_dir()?),
  }
}

async fn shutdown_daemon(working_dir: &Path) -> anyhow::Result<()> {
  match lockfile::get_daemon_status(working_dir)? {
    None => anyhow::bail!("No daemon found for this directory"),
    Some(info) if !info.is_running => {
      lockfile::cleanup_stale(working_dir)?;
      anyhow::bail!("Daemon is not running (stale lock file cleaned up)");
    }
    Some(_) => {}
  }

  let _ = rpc_request(working_dir, RpcRequest::Shutdown {}, false).await;

  for _ in 0..50 {
    match lockfile::get_daemon_status(working_dir)? {
      None => return Ok(()),
      Some(info) if !info.is_running => {
        lockfile::cleanup_stale(working_dir)?;
        return Ok(());
      }
      Some(_) => {
        tokio::time::sleep(std::time::Duration::from_millis(100)).await
      }
    }
  }

  lockfile::stop_daemon(working_dir)?;
  Ok(())
}

fn daemon_json(info: Option<&lockfile::DaemonInfo>) -> serde_json::Value {
  match info {
    Some(info) => serde_json::json!({
      "running": info.is_running,
      "pid": info.contents.pid,
      "dir": info.contents.working_dir,
      "socket": info.contents.socket,
      "version": info.contents.version,
    }),
    None => serde_json::Value::Null,
  }
}

async fn wait_for_daemon(working_dir: &Path) -> anyhow::Result<()> {
  for _ in 0..50 {
    if let Some(info) = lockfile::get_daemon_status(working_dir)? {
      if info.is_running {
        return Ok(());
      }
    }
    tokio::time::sleep(std::time::Duration::from_millis(40)).await;
  }
  anyhow::bail!("daemon did not come up within 2s");
}

pub async fn dekit_main() -> anyhow::Result<()> {
  let cmd = clap::command!()
    .subcommands([
      Command::new("tui")
        .about("Open the TUI, starting the server if needed"),
      Command::new("attach").about("Attach the TUI to the running daemon"),
      Command::new("up")
        .about("Start the daemon if needed and start autostart tasks"),
      Command::new("down")
        .about("Stop tasks; without a pattern, stop all tasks")
        .arg(Arg::new("pattern").help("Task path or glob")),
      Command::new("spawn")
        .about("Add a task at a path and start it")
        .arg(
          Arg::new("path")
            .long("path")
            .required(true)
            .help("Task path (e.g. /services/web)"),
        )
        .arg(
          Arg::new("cwd")
            .long("cwd")
            .help("Working directory for the process"),
        )
        .arg(
          Arg::new("cmd")
            .required(true)
            .num_args(1..)
            .last(true)
            .help("Command to run"),
        ),
      Command::new("ls")
        .about("List tasks")
        .arg(Arg::new("glob").help("Optional glob pattern")),
      Command::new("start")
        .about("Start tasks matching a path or glob")
        .arg(Arg::new("pattern").required(true).help("Task path or glob")),
      Command::new("stop")
        .about("Stop tasks; each comes back if something still wants it")
        .arg(Arg::new("pattern").required(true).help("Task path or glob")),
      Command::new("kill")
        .about("Like stop, but with an immediate hard kill")
        .arg(Arg::new("pattern").required(true).help("Task path or glob")),
      Command::new("veto")
        .about("Veto tasks: they stay down until started again")
        .arg(Arg::new("pattern").required(true).help("Task path or glob")),
      Command::new("restart")
        .about("Restart tasks matching a path or glob")
        .arg(Arg::new("pattern").required(true).help("Task path or glob")),
      Command::new("why")
        .about("Explain why a task is (not) running")
        .arg(Arg::new("path").required(true).help("Task path")),
      Command::new("screen")
        .about("Print the current screen of a task")
        .arg(Arg::new("path").required(true).help("Task path")),
      Command::new("server")
        .about("Manage the background server")
        .subcommands([
        Command::new("run")
          .about("Run the daemon in the foreground")
          .arg(
            Arg::new("dir")
              .long("dir")
              .required(true)
              .help("Working directory this server manages"),
          )
          .arg(
            Arg::new("log-level")
              .long("log-level")
              .help("Diagnostic log level (off|error|warn|info|debug|trace, or env_logger spec). Falls back to $DEKIT_LOG, $RUST_LOG, then 'error' (release) or 'trace' (debug)."),
          ),
        Command::new("start")
          .about("Start the server for the current directory"),
        Command::new("stop").about("Stop the server for the current directory"),
        Command::new("status")
          .about("Show server status for the current directory"),
        Command::new("list").about("List all server on this machine"),
        Command::new("clean").about("Remove stale lock files"),
      ]),
      Command::new("mprocs")
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
        .help("Directory whose server to talk to (default: current dir)"),
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
        .help("A .js script to run; with no command, launch the TUI"),
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

  match matches.subcommand() {
    Some(("tui", _sub_m)) => {
      let working_dir = resolve_working_dir(&matches)?;
      let (sender, receiver) = connect_client_socket(&working_dir, true).await?;
      client_main(sender, receiver).await?;
    }
    Some(("attach", _sub_m)) => {
      let working_dir = resolve_working_dir(&matches)?;
      let (sender, receiver) =
        connect_client_socket(&working_dir, false).await?;
      client_main(sender, receiver).await?;
    }
    Some(("spawn", sub_m)) => {
      let working_dir = resolve_working_dir(&matches)?;
      let path = sub_m.get_one::<String>("path").unwrap().clone();
      let cwd = sub_m.get_one::<String>("cwd").cloned();
      let cmd: Vec<String> =
        sub_m.get_many::<String>("cmd").unwrap().cloned().collect();
      let result =
        rpc_request(&working_dir, RpcRequest::Spawn { path, cmd, cwd }, true)
          .await?;
      print_done(result, json, "Spawned.")?;
    }
    Some(("ls", sub_m)) => {
      let working_dir = resolve_working_dir(&matches)?;
      let glob = sub_m.get_one::<String>("glob").cloned();
      let result =
        rpc_request(&working_dir, RpcRequest::Ls { glob }, false).await?;
      print_task_list(result, json)?;
    }
    Some(("start", sub_m)) => {
      let working_dir = resolve_working_dir(&matches)?;
      let pattern = sub_m.get_one::<String>("pattern").unwrap().clone();
      let result =
        rpc_request(&working_dir, RpcRequest::Start { pattern }, true).await?;
      print_done(result, json, "Started.")?;
    }
    Some(("stop", sub_m)) => {
      let working_dir = resolve_working_dir(&matches)?;
      let pattern = sub_m.get_one::<String>("pattern").unwrap().clone();
      let result =
        rpc_request(&working_dir, RpcRequest::Stop { pattern }, false).await?;
      print_done(result, json, "Stopped.")?;
    }
    Some(("kill", sub_m)) => {
      let working_dir = resolve_working_dir(&matches)?;
      let pattern = sub_m.get_one::<String>("pattern").unwrap().clone();
      let result =
        rpc_request(&working_dir, RpcRequest::Kill { pattern }, false).await?;
      print_done(result, json, "Killed.")?;
    }
    Some(("veto", sub_m)) => {
      let working_dir = resolve_working_dir(&matches)?;
      let pattern = sub_m.get_one::<String>("pattern").unwrap().clone();
      let result =
        rpc_request(&working_dir, RpcRequest::Veto { pattern }, false).await?;
      print_done(result, json, "Vetoed.")?;
    }
    Some(("restart", sub_m)) => {
      let working_dir = resolve_working_dir(&matches)?;
      let pattern = sub_m.get_one::<String>("pattern").unwrap().clone();
      let result =
        rpc_request(&working_dir, RpcRequest::Restart { pattern }, true)
          .await?;
      print_done(result, json, "Restarted.")?;
    }
    Some(("why", sub_m)) => {
      let working_dir = resolve_working_dir(&matches)?;
      let path = sub_m.get_one::<String>("path").unwrap().clone();
      let result =
        rpc_request(&working_dir, RpcRequest::Why { path }, false).await?;
      print_why(result, json)?;
    }
    Some(("screen", sub_m)) => {
      let working_dir = resolve_working_dir(&matches)?;
      let path = sub_m.get_one::<String>("path").unwrap().clone();
      let result =
        rpc_request(&working_dir, RpcRequest::Screen { path }, false).await?;
      let screen: ScreenResult = serde_json::from_value(result)?;
      if json {
        println!("{}", serde_json::to_string(&screen)?);
      } else {
        match screen.screen {
          Some(content) => {
            print!("{}", content);
            // Reset terminal attributes after printing
            print!("\x1b[0m\n");
          }
          None => anyhow::bail!("no screen content for this task"),
        }
      }
    }
    Some(("up", _sub_m)) => {
      let working_dir = resolve_working_dir(&matches)?;
      let result = rpc_request(&working_dir, RpcRequest::Up {}, true).await?;
      print_done(result, json, "Started autostart tasks.")?;
    }
    Some(("down", sub_m)) => {
      let working_dir = resolve_working_dir(&matches)?;
      let pattern = sub_m.get_one::<String>("pattern").cloned();
      let result =
        rpc_request(&working_dir, RpcRequest::Down { pattern }, false).await?;
      print_done(result, json, "Put down.")?;
    }
    Some(("server", sub_m)) => match sub_m.subcommand() {
      Some(("run", run_m)) => {
        let dir = run_m.get_one::<String>("dir").unwrap();
        let log_level =
          run_m.get_one::<String>("log-level").map(String::as_str);
        run_server(PathBuf::from(dir), log_level).await?;
      }
      Some(("start", _sub_m)) => {
        let working_dir = resolve_working_dir(&matches)?;
        // Check if already running.
        if let Some(info) = lockfile::get_daemon_status(&working_dir)? {
          if info.is_running {
            println!("Daemon already running (pid={}).", info.contents.pid);
            return Ok(());
          }
          // Stale -- clean up and start fresh.
          lockfile::cleanup_stale(&working_dir)?;
        }
        spawn_server_daemon(&working_dir)?;
        wait_for_daemon(&working_dir).await?;
        println!("Daemon started for {}.", working_dir.display());
      }
      Some(("stop", _sub_m)) => {
        let working_dir = resolve_working_dir(&matches)?;
        shutdown_daemon(&working_dir).await?;
        println!("Daemon stopped.");
      }
      Some(("status", _sub_m)) => {
        let working_dir = resolve_working_dir(&matches)?;
        let info = lockfile::get_daemon_status(&working_dir)?;
        if matches.get_flag("json") {
          println!("{}", serde_json::to_string(&daemon_json(info.as_ref()))?);
        } else {
          match info {
            Some(info) => {
              let status = if info.is_running { "running" } else { "stale" };
              println!(
                "[{}] pid={} socket={} version={}",
                status,
                info.contents.pid,
                info.contents.socket,
                info.contents.version,
              );
            }
            None => {
              println!("No daemon for this directory.");
            }
          }
        }
      }
      Some(("list", _sub_m)) => {
        let daemons = lockfile::list_daemons()?;
        if matches.get_flag("json") {
          let arr: Vec<_> =
            daemons.iter().map(|d| daemon_json(Some(d))).collect();
          println!("{}", serde_json::to_string(&arr)?);
        } else if daemons.is_empty() {
          println!("No daemons found.");
        } else {
          for d in &daemons {
            let status = if d.is_running { "running" } else { "stale" };
            println!(
              "[{}] pid={} dir={} socket={} version={}",
              status,
              d.contents.pid,
              d.contents.working_dir,
              d.contents.socket,
              d.contents.version,
            );
          }
        }
      }
      Some(("clean", _sub_m)) => {
        let count = lockfile::cleanup_all_stale()?;
        println!("Removed {} stale lock file(s).", count);
      }
      _ => {
        anyhow::bail!(
          "expected a subcommand after `dekit server` (run, start, stop, status, list, clean)"
        );
      }
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
        if first.ends_with(".js") {
          let src = std::fs::read_to_string(first)?;

          let vm = JsVm::new().await?;
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
        // No args: same as `tui`.
        let working_dir = resolve_working_dir(&matches)?;
        let (sender, receiver) =
          connect_client_socket(&working_dir, true).await?;
        client_main(sender, receiver).await?;
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
