//! Config values written in JavaScript (`!js`). Each is one expression,
//! evaluated synchronously while the config loads: it reads `ctx` and
//! `std.path`, has no effects, and its value must be plain data.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use rquickjs::{
  Array, CatchResultExt, CaughtError, Context, Ctx, Exception, Function,
  Object, Runtime, Type, Value,
};
use serde_yaml::Value as Yaml;

/// How long one expression may run.
const TIME_LIMIT: Duration = Duration::from_secs(1);
const MEMORY_LIMIT: usize = 64 << 20;
const STACK_LIMIT: usize = 512 << 10;
/// How deep a value may nest; this also ends a cycle.
const MAX_DEPTH: usize = 64;

/// Freezes a value and everything in it, so an expression can't change
/// what it was given.
const FREEZE: &str = r#"(function freeze(value) {
  if (value !== null && typeof value === "object" && !Object.isFrozen(value)) {
    Object.freeze(value);
    for (const key of Object.keys(value)) freeze(value[key]);
  }
  return value;
})"#;

/// The engine for the expressions of one config load. Each expression
/// gets a context of its own, so none sees what another left behind.
pub struct Exprs {
  runtime: Runtime,
  deadline: Arc<Mutex<Instant>>,
  limit: Duration,
}

impl Exprs {
  pub fn new() -> anyhow::Result<Self> {
    Self::with_limit(TIME_LIMIT)
  }

  pub(crate) fn with_limit(limit: Duration) -> anyhow::Result<Self> {
    let runtime = Runtime::new()?;
    runtime.set_memory_limit(MEMORY_LIMIT);
    runtime.set_max_stack_size(STACK_LIMIT);
    let deadline = Arc::new(Mutex::new(Instant::now()));
    let check = deadline.clone();
    runtime.set_interrupt_handler(Some(Box::new(move || {
      Instant::now() > *check.lock().unwrap()
    })));
    Ok(Self {
      runtime,
      deadline,
      limit,
    })
  }

  /// The value of the expression `src` with `ctx` in scope as `ctx`. An
  /// error says what went wrong in a few words, for the config error
  /// that names where the expression is.
  pub fn eval(&self, src: &str, ctx: &Yaml) -> Result<Yaml, String> {
    let deadline = Instant::now() + self.limit;
    *self.deadline.lock().unwrap() = deadline;
    let context =
      Context::full(&self.runtime).map_err(|err| err.to_string())?;
    context.with(|cx| {
      let failed = |err: CaughtError<'_>| {
        if Instant::now() > deadline {
          format!("took longer than {:?}", self.limit)
        } else {
          describe(err)
        }
      };
      let (value, plain) = eval_in(&cx, src, ctx).catch(&cx).map_err(failed)?;
      match to_yaml(&value, &plain, "", 0) {
        Ok(value) => Ok(value),
        Err(Bad::Value(why)) => Err(why),
        Err(Bad::Js(err)) => Err(failed(CaughtError::from_error(&cx, err))),
      }
    })
  }
}

/// The expression's value, and the prototype of a plain object, taken
/// before the expression runs, which could replace `Object`.
fn eval_in<'js>(
  cx: &Ctx<'js>,
  src: &str,
  ctx: &Yaml,
) -> rquickjs::Result<(Value<'js>, Object<'js>)> {
  crate::js::lib::init_config(cx)?;
  let globals = cx.globals();
  let plain: Object = globals.get::<_, Object>("Object")?.get("prototype")?;
  let freeze: Function = cx.eval(FREEZE)?;
  freeze.call::<_, ()>((globals.get::<_, Value>("std")?,))?;
  let ctx: Value = freeze.call((from_yaml(cx, ctx)?,))?;
  globals.set("ctx", ctx)?;
  // The parentheses make it one expression, so `{a: 1}` is an object; the
  // newline lets it end in a `//` comment and keeps its line numbers.
  Ok((cx.eval(format!("({src}\n)"))?, plain))
}

fn describe(err: CaughtError<'_>) -> String {
  match err {
    CaughtError::Exception(exception) => {
      let message = exception.message().unwrap_or_default();
      match exception.get::<_, String>("name") {
        Ok(name) if !name.is_empty() => format!("{name}: {message}"),
        _ => message,
      }
    }
    CaughtError::Value(value) => match value.as_string() {
      Some(text) => format!("threw {:?}", text.to_string().unwrap_or_default()),
      None => format!("threw {}", what(&value)),
    },
    CaughtError::Error(err) => err.to_string(),
  }
}

fn from_yaml<'js>(cx: &Ctx<'js>, value: &Yaml) -> rquickjs::Result<Value<'js>> {
  Ok(match value {
    Yaml::Null => Value::new_null(cx.clone()),
    Yaml::Bool(b) => Value::new_bool(cx.clone(), *b),
    Yaml::Number(n) => match n.as_i64().and_then(|n| i32::try_from(n).ok()) {
      Some(n) => Value::new_int(cx.clone(), n),
      None => Value::new_number(cx.clone(), n.as_f64().unwrap_or(f64::NAN)),
    },
    Yaml::String(s) => rquickjs::String::from_str(cx.clone(), s)?.into_value(),
    Yaml::Sequence(items) => {
      let array = Array::new(cx.clone())?;
      for (i, item) in items.iter().enumerate() {
        array.set(i, from_yaml(cx, item)?)?;
      }
      array.into_value()
    }
    Yaml::Mapping(map) => {
      let object = Object::new(cx.clone())?;
      for (key, item) in map {
        let key = match key {
          Yaml::String(s) => s.clone(),
          Yaml::Number(n) => n.to_string(),
          Yaml::Bool(b) => b.to_string(),
          _ => {
            return Err(Exception::throw_type(
              cx,
              "a key in ctx is not a string, number, or boolean",
            ));
          }
        };
        object.set(key, from_yaml(cx, item)?)?;
      }
      object.into_value()
    }
    Yaml::Tagged(tagged) => from_yaml(cx, &tagged.value)?,
  })
}

enum Bad {
  /// The value is not plain data.
  Value(String),
  /// Reading it threw, as a getter can.
  Js(rquickjs::Error),
}

impl From<rquickjs::Error> for Bad {
  fn from(err: rquickjs::Error) -> Self {
    Bad::Js(err)
  }
}

/// The value as YAML data. `at` is where it is in the expression's value,
/// as in `.ports[0]`, for the error.
fn to_yaml<'js>(
  value: &Value<'js>,
  plain: &Object<'js>,
  at: &str,
  depth: usize,
) -> Result<Yaml, Bad> {
  let bad = |what: &str| {
    let at = if at.is_empty() {
      String::new()
    } else {
      format!(" in {at}")
    };
    Bad::Value(format!("returned {what}{at}, which is not data"))
  };
  if depth > MAX_DEPTH {
    return Err(bad(&format!("a value nested over {MAX_DEPTH} levels deep")));
  }
  Ok(match value.type_of() {
    Type::Null => Yaml::Null,
    Type::Bool => Yaml::Bool(value.as_bool().unwrap_or_default()),
    Type::Int => Yaml::from(value.as_int().unwrap_or_default()),
    Type::Float => {
      let n = value.as_float().unwrap_or(f64::NAN);
      if !n.is_finite() {
        return Err(bad(&n.to_string()));
      }
      // A whole number reads as an integer, as it would in YAML.
      if n.fract() == 0.0 && n.abs() < 2f64.powi(53) {
        Yaml::from(n as i64)
      } else {
        Yaml::from(n)
      }
    }
    Type::String => {
      Yaml::String(value.as_string().expect("a string").to_string()?)
    }
    Type::Array => {
      let array = value.as_array().expect("an array");
      let mut items = Vec::with_capacity(array.len());
      for i in 0..array.len() {
        let item: Value = array.get(i)?;
        items.push(to_yaml(&item, plain, &format!("{at}[{i}]"), depth + 1)?);
      }
      Yaml::Sequence(items)
    }
    Type::Object => {
      let object = value.as_object().expect("an object");
      if !is_plain(object, plain) {
        return Err(bad(&what(value)));
      }
      let mut map = serde_yaml::Mapping::new();
      for prop in object.props::<String, Value>() {
        let (key, item) = prop?;
        let item = to_yaml(&item, plain, &format!("{at}.{key}"), depth + 1)?;
        map.insert(Yaml::String(key), item);
      }
      Yaml::Mapping(map)
    }
    Type::Promise => {
      return Err(Bad::Value(
        "returned a promise; it runs synchronously and can't await".to_string(),
      ));
    }
    _ => return Err(bad(&what(value))),
  })
}

/// An object made by `{...}` or with no prototype, not a Date, Map, or
/// class instance: its prototype is `plain`, `Object.prototype`, or none.
fn is_plain<'js>(object: &Object<'js>, plain: &Object<'js>) -> bool {
  match object.get_prototype() {
    Some(proto) => proto.as_value() == plain.as_value(),
    None => true,
  }
}

/// A value's kind, for an error: `undefined`, `a function`, `a Date`.
fn what(value: &Value<'_>) -> String {
  match value.type_of() {
    Type::Undefined | Type::Uninitialized => "undefined".to_string(),
    Type::Function | Type::Constructor => "a function".to_string(),
    Type::Symbol => "a symbol".to_string(),
    Type::BigInt => "a bigint".to_string(),
    Type::Object | Type::Exception | Type::Proxy => value
      .as_object()
      .and_then(|object| object.get::<_, Object>("constructor").ok())
      .and_then(|constructor| constructor.get::<_, String>("name").ok())
      .filter(|name| !name.is_empty())
      .map(|name| {
        let article = if name.starts_with(['A', 'E', 'I', 'O', 'U']) {
          "an"
        } else {
          "a"
        };
        format!("{article} {name}")
      })
      .unwrap_or_else(|| "an object that is not plain".to_string()),
    other => other.as_str().to_string(),
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  fn yaml(text: &str) -> Yaml {
    serde_yaml::from_str(text).unwrap()
  }

  fn eval(src: &str) -> Result<Yaml, String> {
    let ctx =
      yaml("{dir: /p, runner: {root: /p, kind: project}, vars: {n: 2}}");
    Exprs::new().unwrap().eval(src, &ctx)
  }

  #[test]
  fn values_come_back_as_data() {
    assert_eq!(eval("1 + 1"), Ok(yaml("2")));
    assert_eq!(eval("2 ** 40"), Ok(yaml("1099511627776")));
    assert_eq!(eval("0.5"), Ok(yaml("0.5")));
    assert_eq!(
      eval("{a: [true, null, 'x']}"),
      Ok(yaml("{a: [true, null, x]}"))
    );
    assert_eq!(eval("Object.create(null)"), Ok(yaml("{}")));
    assert_eq!(eval("`${ctx.dir}/x` // a comment"), Ok(yaml("/p/x")));
    assert_eq!(eval("ctx.vars.n * 3"), Ok(yaml("6")));
    assert_eq!(
      eval("std.path.join(ctx.runner.root, 'a')"),
      Ok(yaml("/p/a"))
    );
  }

  #[test]
  fn what_is_not_data_is_refused() {
    let err = |src: &str| eval(src).unwrap_err();
    assert!(
      err("undefined").contains("returned undefined"),
      "{}",
      err("undefined")
    );
    assert!(err("[1, () => 2]").contains("a function in [1]"));
    assert!(err("({d: new Date()})").contains("a Date in .d"));
    assert!(err("({e: new Error('x')})").contains("an Error in .e"));
    assert!(err("new Map()").contains("a Map"));
    assert!(err("0 / 0").contains("NaN"));
    assert!(err("Promise.resolve(1)").contains("can't await"));
    assert!(
      err("(() => { const a = {}; a.a = a; return a })()").contains("deep")
    );
  }

  #[test]
  fn errors_say_what_was_thrown() {
    let err = eval("dri + 1").unwrap_err();
    assert!(err.starts_with("ReferenceError: "), "{err}");
    assert!(err.contains("dri"), "{err}");
    assert_eq!(
      eval("(() => { throw 'no' })()"),
      Err("threw \"no\"".to_string())
    );
    let err = eval("1;").unwrap_err();
    assert!(err.starts_with("SyntaxError: "), "{err}");
  }

  #[test]
  fn an_expression_has_no_effects() {
    // `ctx` is frozen, and strict mode makes writing to it throw.
    assert!(eval("ctx.vars.n = 3").unwrap_err().starts_with("TypeError"));
    // Only std.path, and not `resolve`, which reads the working directory.
    assert_eq!(eval("Object.keys(std)"), Ok(yaml("[path]")));
    assert_eq!(eval("typeof std.path.resolve"), Ok(yaml("undefined")));
    // Replacing `Object` doesn't change what a plain object is.
    assert_eq!(eval("(Object = null, {a: 1})"), Ok(yaml("{a: 1}")));
    // A global set by one expression is gone in the next.
    let exprs = Exprs::new().unwrap();
    let ctx = yaml("{}");
    exprs.eval("globalThis.x = 1", &ctx).unwrap();
    assert_eq!(exprs.eval("typeof x", &ctx), Ok(yaml("undefined")));
  }

  #[test]
  fn a_runaway_expression_is_stopped() {
    let exprs = Exprs::with_limit(Duration::from_millis(50)).unwrap();
    let err = exprs.eval("(() => { while (true) {} })()", &yaml("{}"));
    assert_eq!(err, Err("took longer than 50ms".to_string()));
    let err = exprs.eval("(function f() { return f() + 1 })()", &yaml("{}"));
    assert!(err.is_err(), "{err:?}");
    // The next one runs again.
    assert_eq!(exprs.eval("1", &yaml("{}")), Ok(yaml("1")));
  }
}
