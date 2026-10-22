use rquickjs::{Ctx, Exception, Object, Value, function::Opt};

use crate::command::Command;
use crate::dekit::wait_client;
use crate::js::rquickjs_ext::ObjectExt;
use crate::protocol::{ActResult, RpcRequest, WaitFor, WaitMark, WaitOutcome};
use crate::runner::RunnerSpec;
use crate::target::Target;

pub(crate) struct RunnerStore(pub RunnerSpec);

unsafe impl<'js> rquickjs::JsLifetime<'js> for RunnerStore {
  type Changed<'to> = RunnerStore;
}

fn runner(ctx: &Ctx<'_>) -> rquickjs::Result<RunnerSpec> {
  Ok(
    ctx
      .userdata::<RunnerStore>()
      .ok_or_else(|| {
        Exception::throw_message(ctx, "runner identity is not initialized")
      })?
      .0
      .clone(),
  )
}

async fn run(ctx: &Ctx<'_>, command: Command) -> rquickjs::Result<usize> {
  let runner = runner(ctx)?;
  let spawn = command.starts_runner();
  let value = crate::dekit::rpc_client::rpc_request(
    &runner,
    RpcRequest::Command(command),
    spawn,
  )
  .await
  .map_err(|err| Exception::throw_message(ctx, &err.to_string()))?;
  serde_json::from_value::<ActResult>(value)
    .map(|result| result.matched)
    .map_err(|err| Exception::throw_message(ctx, &err.to_string()))
}

fn target(ctx: &Ctx<'_>, value: String) -> rquickjs::Result<Target> {
  let target: Target =
    value.parse().map_err(|err: crate::target::InvalidTarget| {
      Exception::throw_message(ctx, &err.to_string())
    })?;
  if target.runner().is_some() {
    return Err(Exception::throw_message(
      ctx,
      "std.dekit targets are local to the script's runner",
    ));
  }
  Ok(target)
}

macro_rules! command_fn {
  ($name:ident, $variant:ident) => {
    async fn $name(ctx: Ctx<'_>, value: String) -> rquickjs::Result<usize> {
      let target = target(&ctx, value)?;
      run(&ctx, Command::$variant { target }).await
    }
  };
}

command_fn!(start, Start);
command_fn!(run_fresh, ForceRestart);
command_fn!(stop, Stop);
command_fn!(kill, Kill);
command_fn!(veto, Veto);
command_fn!(restart, Restart);
command_fn!(remove, Remove);

async fn up(ctx: Ctx<'_>) -> rquickjs::Result<usize> {
  run(&ctx, Command::Up).await
}

async fn add(
  ctx: Ctx<'_>,
  path: String,
  cmd: Vec<String>,
) -> rquickjs::Result<usize> {
  let target = target(&ctx, path)?;
  run(
    &ctx,
    Command::Add {
      target,
      spec: crate::spec::TaskSpec::with_cmd(cmd),
      place: Default::default(),
      start: true,
      defaults: true,
      origin: None,
    },
  )
  .await
}

/// Waits as `dekit wait` does: resolves with the result when it is met,
/// and rejects with an error whose `result` is it on a failure or a
/// timeout.
async fn wait<'js>(
  ctx: Ctx<'js>,
  value: String,
  Opt(opts): Opt<Object<'js>>,
) -> rquickjs::Result<Value<'js>> {
  let target = target(&ctx, value)?;
  let (wait_for, timeout_ms) = match &opts {
    Some(opts) => {
      let wait_for = match opts.get::<_, Option<String>>("for")?.as_deref() {
        None | Some("ready") => WaitFor::Ready,
        Some("exit") => WaitFor::Exit,
        Some(other) => {
          return Err(Exception::throw_message(
            &ctx,
            &format!(
              "std.dekit.wait: `for` is \"ready\" or \"exit\", not {other:?}"
            ),
          ));
        }
      };
      let max = crate::config::duration::MAX.as_millis() as f64;
      let timeout = match opts.get::<_, Option<f64>>("timeoutMs")? {
        Some(ms) if !(0.0..=max).contains(&ms) => {
          return Err(Exception::throw_message(
            &ctx,
            "std.dekit.wait: `timeoutMs` is a number of milliseconds from 0 to a day",
          ));
        }
        Some(ms) => Some(ms as u64),
        None => None,
      };
      (wait_for, timeout)
    }
    None => (WaitFor::Ready, None),
  };
  let request = RpcRequest::Wait {
    target: Some(target),
    command: None,
    wait_for: Some(wait_for),
    timeout_ms,
  };
  let runner = runner(&ctx)?;
  let result = wait_client::wait(&runner, request, false, |_| {})
    .await
    .map_err(|err| Exception::throw_message(&ctx, &err.to_string()))?;
  let json = serde_json::to_string(&result)
    .map_err(|err| Exception::throw_message(&ctx, &err.to_string()))?;
  let value = ctx.json_parse(json)?;
  if result.wait == WaitOutcome::Met {
    return Ok(value);
  }
  let pending = if result.wait == WaitOutcome::Timeout {
    WaitMark::Pending
  } else {
    WaitMark::Failed
  };
  let names: Vec<&str> = result
    .tasks
    .iter()
    .filter(|task| task.wait == pending)
    .map(|task| task.info.path.as_str())
    .collect();
  let message = if result.wait == WaitOutcome::Timeout {
    format!("wait timed out: {} still pending", names.join(", "))
  } else if result.quitting {
    format!("wait failed: {} (the runner is stopping)", names.join(", "))
  } else {
    format!("wait failed: {}", names.join(", "))
  };
  let error = Exception::from_message(ctx.clone(), &message)?;
  error.as_object().set("result", value)?;
  Err(error.throw())
}

pub fn init(ctx: Ctx<'_>) -> rquickjs::Result<Object<'_>> {
  let obj = Object::new(ctx)?;
  obj.def_fn_async("start", start)?;
  obj.def_fn_async("up", up)?;
  obj.def_fn_async("run", run_fresh)?;
  obj.def_fn_async("stop", stop)?;
  obj.def_fn_async("kill", kill)?;
  obj.def_fn_async("veto", veto)?;
  obj.def_fn_async("restart", restart)?;
  obj.def_fn_async("remove", remove)?;
  obj.def_fn_async("add", add)?;
  obj.def_fn_async("wait", wait)?;
  Ok(obj)
}
