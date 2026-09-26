pub(crate) mod dekit;
mod env;
mod fs;
mod log;
mod path;
mod process;
// Being redesigned; only in debug builds until then.
#[cfg(debug_assertions)]
mod tui;

use rquickjs::{Ctx, Object, function::This, object::Accessor};

pub fn init(ctx: &Ctx<'_>) -> rquickjs::Result<()> {
  let obj = Object::new(ctx.clone())?;

  log::define(&obj)?;

  register_lazy(&obj, "fs", fs::init)?;
  register_lazy(&obj, "path", path::init)?;
  register_lazy(&obj, "env", env::init)?;
  register_lazy(&obj, "dekit", dekit::init)?;
  register_lazy(&obj, "process", process::init)?;
  #[cfg(debug_assertions)]
  register_lazy(&obj, "tui", tui::init)?;

  ctx.globals().set("std", obj)?;
  Ok(())
}

pub(crate) fn register_lazy<'js>(
  obj: &Object<'js>,
  name: &str,
  factory: fn(Ctx<'js>) -> rquickjs::Result<Object<'js>>,
) -> rquickjs::Result<()> {
  let name_owned = name.to_string();
  obj.prop(
    name,
    Accessor::from(
      move |this: This<Object<'js>>,
            ctx: Ctx<'js>|
            -> rquickjs::Result<Object<'js>> {
        let obj = factory(ctx)?;
        this.0.prop(&*name_owned, obj.clone())?;
        Ok(obj)
      },
    )
    .configurable()
    .enumerable(),
  )?;
  Ok(())
}
