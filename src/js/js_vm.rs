use std::path::Path;

use anyhow::anyhow;
use rquickjs::CatchResultExt;
use rquickjs::{AsyncContext, AsyncRuntime, Ctx, Module, Object, Persistent};

use crate::runner::RunnerSpec;

pub struct JsVm {
  #[allow(dead_code)]
  runtime: AsyncRuntime,
  pub context: AsyncContext,
}

impl JsVm {
  /// `runner` is the identity `std.dekit` calls act on; a standalone
  /// script (run outside any project) passes `None` and simply fails
  /// those calls lazily if it makes them.
  pub async fn new(runner: Option<RunnerSpec>) -> anyhow::Result<Self> {
    let runtime = AsyncRuntime::new()?;
    let context = AsyncContext::full(&runtime).await?;

    AsyncContext::async_with(&context, async |ctx| {
      if let Some(runner) = runner {
        ctx.store_userdata(super::lib::dekit::RunnerStore(runner))?;
      }
      super::lib::init(&ctx)
    })
    .await?;

    Ok(JsVm { runtime, context })
  }

  pub async fn eval_file(
    &self,
    path: &Path,
    src: &[u8],
  ) -> anyhow::Result<Persistent<Object<'static>>> {
    let src = src.to_vec();
    let path = path.to_path_buf();
    let module = AsyncContext::async_with(&self.context, async |ctx| {
      eval_module(&ctx, &path, src)
        .await
        .catch(&ctx)
        .map_err(|err| anyhow!("JavaScript module evaluation failed:\n{err}"))
    })
    .await?;
    Ok(module)
  }
}

async fn eval_module(
  ctx: &Ctx<'_>,
  path: &Path,
  src: Vec<u8>,
) -> rquickjs::Result<Persistent<Object<'static>>> {
  let name = if let Some(name) = path.file_name() {
    name.to_string_lossy()
  } else {
    path.to_string_lossy()
  };
  let module = Module::declare(ctx.clone(), name.as_bytes(), src)?;
  // Settles after top-level `await`s; a throw at the top level rejects it.
  let (module, promise) = module.eval()?;
  promise.into_future::<()>().await?;
  Ok(Persistent::save(ctx, module.namespace()?))
}
