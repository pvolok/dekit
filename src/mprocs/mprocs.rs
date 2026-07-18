use std::{
  collections::HashSet,
  fs::File,
  io::BufRead,
  io::BufReader,
  path::{Path, PathBuf},
};

use crate::console::app::create_app_task;
use crate::console::keymap::Keymap;
#[cfg(unix)]
use crate::error::ResultLogger;
use crate::kernel::kernel::Kernel;
use crate::mprocs::config::{
  CmdConfig, Config, ConfigContext, ProcConfig, ServerConfig, default_stop,
};
use crate::mprocs::ctl::run_ctl;
use crate::mprocs::event::AppEvent;
use crate::mprocs::just::load_just_procs;
use crate::mprocs::package_json::load_npm_procs;
use crate::mprocs::proc_log_config::{LogConfig, LogMode};
use crate::mprocs::settings::Settings;
use crate::mprocs::yaml_val::Val;
use crate::{
  attach_client::client_main,
  console::{app_client::client_loop, server_message::ClientId},
  protocol::{ConnReceiver, ConnSender},
};
use anyhow::{Result, bail};
use clap::{ArgMatches, arg, command};
use serde_yaml::Value;

pub async fn run_app(args: Vec<String>) -> anyhow::Result<()> {
  let matches = command!()
    .arg(arg!(-c --config [PATH] "Config path [default: mprocs.yaml]"))
    .arg(arg!(-s --server [PATH] "Remote control server address. Example: 127.0.0.1:4050."))
    .arg(arg!(--ctl [YAML] "Send yaml/json encoded command to running mprocs"))
    .arg(arg!(--"proc-list-title" [TITLE] "Title for the processes pane"))
    .arg(arg!(--names [NAMES] "Names for processes provided by cli arguments. Separated by comma."))
    .arg(arg!(--npm "Run scripts from package.json. Scripts are not started by default."))
    .arg(
      arg!(--procfile [PATH] "Load processes from Procfile. Processes are not started by default.")
        .num_args(0..=1)
        .default_missing_value("Procfile"),
    )
    .arg(arg!(--just "Run recipes from justfile. Recipes are not started by default. Requires just to be installed."))
    .arg(arg!(--"on-init" [YAML] "Event to trigger when mprocs starts"))
    .arg(arg!(--"on-all-finished" [YAML] "Event to trigger when all processes are finished"))
    .arg(arg!(--"log-dir" [DIR] "Directory for process log files. Each process logs to <DIR>/<name>.log"))
    .arg(arg!(--"log-file" [PATH] "Process log file path or template. Supports {name}, {id}, {pid}, {ts}. Relative paths are resolved inside --log-dir when present."))
    .arg(
      arg!(--"log-mode" [MODE] "Process log file mode: append or truncate")
        .value_parser(["append", "truncate"]),
    )
    .arg(arg!(--"log-level" [SPEC] "Diagnostic log level (off|error|warn|info|debug|trace, or env_logger spec). Falls back to $MPROCS_LOG, $RUST_LOG, then 'error' (release) or 'trace' (debug)."))
    .arg(arg!([COMMANDS]... "Commands to run (if omitted, commands from config will be run)"))
    .get_matches_from(args);

  let config_value = load_config_value(&matches)
    .map_err(|e| anyhow::Error::msg(format!("[{}] {}", "config", e)))?;

  let mut settings = Settings::default();

  // merge ~/.config/mprocs/mprocs.yaml
  settings.merge_from_xdg().map_err(|e| {
    anyhow::Error::msg(format!("[{}] {}", "global settings", e))
  })?;
  // merge ./mprocs.yaml
  if let Some((value, _)) = &config_value {
    settings
      .merge_value(Val::new(value)?)
      .map_err(|e| anyhow::Error::msg(format!("[{}] {}", "local config", e)))?;
  }

  let mut keymap = Keymap::new();
  settings.add_to_keymap(&mut keymap)?;

  let config = {
    let mut config = if let Some((v, ctx)) = config_value {
      Config::from_value(&v, &ctx, &settings)?
    } else {
      Config::make_default(&settings)?
    };

    if let Some(server_addr) = matches.get_one::<String>("server") {
      config.server = Some(ServerConfig::from_str(server_addr)?);
    }

    if let Some(ctl_arg) = matches.get_one::<String>("ctl") {
      return run_ctl(ctl_arg, &config).await;
    }

    if let Some(title) = matches.get_one::<String>("proc-list-title") {
      config.proc_list_title = title.to_string();
    }

    if let Some(on_all_finished) = matches.get_one::<String>("on-all-finished")
    {
      let event: AppEvent = serde_yaml::from_str(on_all_finished)?;
      config.on_all_finished = Some(event.to_action());
    }

    if let Some(on_init) = matches.get_one::<String>("on-init") {
      let event: AppEvent = serde_yaml::from_str(on_init)?;
      config.on_init = Some(event.to_action());
    }

    if let Some(log_dir) = matches.get_one::<String>("log-dir") {
      let log = config.proc_log.get_or_insert(LogConfig {
        enabled: Some(true),
        dir: None,
        file: None,
        mode: None,
      });
      log.enabled = Some(true);
      log.dir = Some(PathBuf::from(log_dir));
    }

    if let Some(log_file) = matches.get_one::<String>("log-file") {
      let log = config.proc_log.get_or_insert(LogConfig {
        enabled: Some(true),
        dir: None,
        file: None,
        mode: None,
      });
      log.enabled = Some(true);
      log.file = Some(PathBuf::from(log_file));
    }

    if let Some(log_mode) = matches.get_one::<String>("log-mode") {
      let log = config.proc_log.get_or_insert(LogConfig {
        enabled: Some(true),
        dir: None,
        file: None,
        mode: None,
      });
      log.enabled = Some(true);
      log.mode = Some(match log_mode.as_str() {
        "append" => LogMode::Append,
        "truncate" => LogMode::Truncate,
        _ => unreachable!(),
      });
    }

    if let Some(cmds) = matches.get_many::<String>("COMMANDS") {
      let names = matches
        .get_one::<String>("names")
        .map_or(Vec::new(), |arg| arg.split(',').collect::<Vec<_>>());
      let mut procs = cmds
        .into_iter()
        .enumerate()
        .map(|(i, cmd)| ProcConfig {
          name: names
            .get(i)
            .map_or_else(|| cmd.to_string(), |s| s.to_string()),
          cmd: CmdConfig::Shell {
            shell: cmd.to_string(),
          },
          env: None,
          cwd: None,
          add_path: Vec::new(),
          autostart: true,
          autorestart: false,
          stop: default_stop(),
          deps: Vec::new(),
          mouse_scroll_speed: settings.mouse_scroll_speed,
          scrollback_len: settings.scrollback_len,
          log: config.proc_log.clone(),
        })
        .collect::<Vec<_>>();
      dedup_proc_names(&mut procs);

      config.procs = procs;
    } else if matches.get_flag("npm") {
      let procs = load_npm_procs(&settings)?;
      config.procs = procs;
    } else if let Some(path) = matches.get_one::<String>("procfile") {
      let procs = load_procfile_procs(path, &settings)?;
      config.procs = procs;
    } else if matches.get_flag("just") {
      let procs = load_just_procs(&settings)?;
      config.procs = procs;
    }

    // Propagate top-level log settings to all procs unless they override them.
    if let Some(global_log) = config.proc_log.clone() {
      for proc in &mut config.procs {
        proc.log = Some(match proc.log.take() {
          Some(proc_log) => global_log.merged(&proc_log),
          None => global_log.clone(),
        });
      }
    }

    config
  };

  match matches.subcommand() {
    Some((cmd, _args)) => {
      bail!("Unexpected command: {}", cmd);
    }
    None => {
      let logger = crate::logging::init(crate::logging::Config {
        binary: "mprocs",
        cli_level: matches.get_one::<String>("log-level").map(String::as_str),
        log_env: "MPROCS_LOG",
        file_env: "MPROCS_LOG_FILE",
        config_level: None,
        config_file: None,
        default_dir: None,
      })?;

      let (srv_to_clt_sender, srv_to_clt_receiver) = {
        let (reader, writer) = tokio::io::simplex(8 * 1024);
        (ConnSender::new(writer), ConnReceiver::new(reader))
      };
      let (clt_to_srv_sender, clt_to_srv_receiver) = {
        let (reader, writer) = tokio::io::simplex(8 * 1024);
        (ConnSender::new(writer), ConnReceiver::new(reader))
      };

      #[cfg(unix)]
      crate::process::unix_processes_waiter::UnixProcessesWaiter::init()?;
      let kernel = Kernel::new();
      let pc = kernel.context();
      let app_task_id = create_app_task(
        crate::config::config::Config::from(config),
        keymap,
        &pc,
      );

      let app_sender = pc.get_task_sender(app_task_id);
      tokio::spawn(async move {
        client_loop(
          ClientId(1),
          app_sender,
          (srv_to_clt_sender, clt_to_srv_receiver),
        )
        .await
      });

      tokio::spawn(async {
        kernel.run().await;
        #[cfg(unix)]
        crate::process::unix_processes_waiter::UnixProcessesWaiter::uninit()
          .log_ignore();
      });

      let ret = client_main(clt_to_srv_sender, srv_to_clt_receiver).await;
      drop(logger);
      ret
    }
  }
}

fn load_procfile_procs(
  path: &str,
  settings: &Settings,
) -> Result<Vec<ProcConfig>> {
  let file = File::open(path)?;
  let reader = BufReader::new(file);

  let mut procs = Vec::new();

  for line in reader.lines() {
    let line = line?;
    let line = line.trim();
    if line.is_empty() || line.starts_with('#') {
      continue;
    }

    if let Some((name, cmd)) = line.split_once(':') {
      let name = name.trim().to_string();
      let cmd = cmd.trim().to_string();

      procs.push(ProcConfig {
        name,
        cmd: CmdConfig::Shell { shell: cmd },
        cwd: None,
        env: None,
        add_path: Vec::new(),
        autostart: false,
        autorestart: false,
        stop: default_stop(),
        deps: Vec::new(),
        mouse_scroll_speed: settings.mouse_scroll_speed,
        scrollback_len: settings.scrollback_len,
        log: None,
      });
    }
  }
  dedup_proc_names(&mut procs);
  Ok(procs)
}

fn dedup_proc_names(procs: &mut [ProcConfig]) {
  let mut taken: HashSet<String> = HashSet::new();
  for p in procs.iter_mut() {
    if taken.insert(p.name.clone()) {
      continue;
    }
    let base = p.name.clone();
    let mut n: usize = 2;
    loop {
      let candidate = format!("{} ({})", base, n);
      if taken.insert(candidate.clone()) {
        p.name = candidate;
        break;
      }
      n += 1;
    }
  }
}

fn load_config_value(
  matches: &ArgMatches,
) -> Result<Option<(Value, ConfigContext)>> {
  if let Some(path) = matches.get_one::<String>("config") {
    return Ok(Some((
      read_value(path)?,
      ConfigContext { path: path.into() },
    )));
  }

  {
    let path = "mprocs.yaml";
    if Path::new(path).is_file() {
      return Ok(Some((
        read_value(path)?,
        ConfigContext { path: path.into() },
      )));
    }
  }

  {
    let path = "mprocs.json";
    if Path::new(path).is_file() {
      return Ok(Some((
        read_value(path)?,
        ConfigContext { path: path.into() },
      )));
    }
  }

  Ok(None)
}

fn read_value(path: &str) -> Result<Value> {
  // Open the file in read-only mode with buffer.
  let file = match std::fs::File::open(path) {
    Ok(file) => file,
    Err(err) => match err.kind() {
      std::io::ErrorKind::NotFound => {
        bail!("Config file '{}' not found.", path);
      }
      _kind => return Err(err.into()),
    },
  };
  let reader = std::io::BufReader::new(file);
  let ext = std::path::Path::new(path)
    .extension()
    .map_or_else(|| "".to_string(), |ext| ext.to_string_lossy().to_string());
  let mut value: Value = match ext.as_str() {
    "yaml" | "yml" | "json" => serde_yaml::from_reader(reader)?,
    _ => bail!("Supported config extensions: yaml, yml, json."),
  };
  value.apply_merge().unwrap();
  Ok(value)
}

#[cfg(test)]
mod tests {
  use super::*;
  use std::fs::File;
  use std::io::Write;
  use std::time::{SystemTime, UNIX_EPOCH};

  #[test]
  fn test_procfile_parsing() {
    let mut temp_dir = std::env::temp_dir();
    let nanos = SystemTime::now()
      .duration_since(UNIX_EPOCH)
      .unwrap()
      .subsec_nanos();
    temp_dir.push(format!("mprocs_test_{}", nanos));
    std::fs::create_dir_all(&temp_dir).unwrap();

    let procfile_path = temp_dir.join("Procfile");
    let mut file = File::create(&procfile_path).unwrap();
    writeln!(file, "web: ./start-web.sh").unwrap();
    writeln!(file, "worker: ./start-worker.sh").unwrap();
    // Add comment
    writeln!(file, "# comment: ignored").unwrap();

    let settings = Settings::default();

    let procs =
      load_procfile_procs(procfile_path.to_str().unwrap(), &settings).unwrap();

    assert_eq!(procs.len(), 2);
    assert!(procs.iter().any(|p| p.name == "web"));
    assert!(procs.iter().any(|p| p.name == "worker"));
    assert!(!procs[0].autostart);

    std::fs::remove_dir_all(&temp_dir).unwrap();
  }

  #[test]
  fn test_procfile_parsing_edge_cases() {
    let mut temp_dir = std::env::temp_dir();
    let nanos = SystemTime::now()
      .duration_since(UNIX_EPOCH)
      .unwrap()
      .subsec_nanos();
    temp_dir.push(format!("mprocs_test_edge_{}", nanos));
    std::fs::create_dir_all(&temp_dir).unwrap();

    let procfile_path = temp_dir.join("Procfile");
    let mut file = File::create(&procfile_path).unwrap();

    writeln!(file, "").unwrap(); // Empty line
    writeln!(file, "  # indented comment").unwrap();
    writeln!(file, "clean: echo clean").unwrap();
    writeln!(file, "  indented  :  echo indented  ").unwrap();
    writeln!(file, "invalid_line_no_colon").unwrap();

    let settings = Settings::default();

    let procs =
      load_procfile_procs(procfile_path.to_str().unwrap(), &settings).unwrap();

    assert_eq!(procs.len(), 2);

    let clean = procs.iter().find(|p| p.name == "clean").unwrap();
    match &clean.cmd {
      CmdConfig::Shell { shell } => assert_eq!(shell, "echo clean"),
      _ => panic!("Unexpected cmd type"),
    }

    let indented = procs.iter().find(|p| p.name == "indented").unwrap();
    match &indented.cmd {
      CmdConfig::Shell { shell } => assert_eq!(shell, "echo indented"),
      _ => panic!("Unexpected cmd type"),
    }

    std::fs::remove_dir_all(&temp_dir).unwrap();
  }
}
