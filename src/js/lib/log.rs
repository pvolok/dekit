use rquickjs::{
  CatchResultExt, Ctx, Object, Type, Value, convert::Coerced, function::Rest,
};

use crate::js::rquickjs_ext::ObjectExt;

pub fn define(obj: &Object<'_>) -> rquickjs::Result<()> {
  obj.def_fn("log", print)?;
  obj.def_fn("warn", print)?;
  obj.def_fn("error", print)?;
  Ok(())
}

fn print<'js>(
  ctx: Ctx<'js>,
  Rest(args): Rest<Value<'js>>,
) -> rquickjs::Result<()> {
  let mut parts = Vec::with_capacity(args.len());
  for value in args {
    let part = match value.type_of() {
      Type::String => value.get::<String>()?,
      Type::Uninitialized | Type::Undefined => "undefined".to_string(),
      Type::Null
      | Type::Bool
      | Type::Int
      | Type::Float
      | Type::BigInt
      | Type::Promise
      | Type::Module
      | Type::Unknown => value.get::<Coerced<String>>()?.0,
      Type::Symbol => {
        let symbol = value.as_symbol().expect("type checked as symbol");
        let description = symbol.description()?.get::<Option<String>>()?;
        format!("Symbol({})", description.unwrap_or_default())
      }
      Type::Function | Type::Constructor => {
        let func = value.as_object().expect("functions are objects");
        match func.get::<_, Coerced<String>>("name")?.0.as_str() {
          "" => "[Function (anonymous)]".to_string(),
          name => format!("[Function: {name}]"),
        }
      }
      Type::Exception => {
        let error = value.as_object().expect("errors are objects");
        let text = value.get::<Coerced<String>>()?.0;
        match error.get::<_, Option<Coerced<String>>>("stack")? {
          Some(Coerced(stack)) if !stack.trim().is_empty() => {
            format!("{text}\n{}", stack.trim_end())
          }
          _ => text,
        }
      }
      Type::Array | Type::Object | Type::Proxy => {
        // JSON.stringify throws on cycles and BigInts.
        match ctx.json_stringify(value.clone()).catch(&ctx).ok().flatten() {
          Some(json) => json.to_string()?,
          None => value.get::<Coerced<String>>()?.0,
        }
      }
    };
    parts.push(part);
  }
  eprintln!("{}", parts.join(" "));
  Ok(())
}
