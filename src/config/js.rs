//! `!js` values and `vars`. A value tagged `!js` is a JavaScript
//! expression, and its value takes the tag's place while the config
//! loads, before any key reads it. `vars` in dekit.yaml are values every
//! `!js` reads as `ctx.vars`.

use std::path::Path;

use anyhow::{Result, anyhow, bail};
use serde_yaml::{Mapping, Value};

use crate::cfg::CfgPath;
use crate::js::expr::Exprs;
use crate::runner::RunnerSpec;

/// What the `!js` values of one load share: the runner, the vars read so
/// far, and the engine, made at the first `!js`.
pub(crate) struct Scope {
  runner: RunnerSpec,
  vars: Mapping,
  exprs: Option<Exprs>,
}

impl Scope {
  pub fn new(runner: &RunnerSpec) -> Self {
    Self {
      runner: runner.clone(),
      vars: Mapping::new(),
      exprs: None,
    }
  }

  /// Reads the `vars` of dekit.yaml, `root` being its top level and `dir`
  /// its directory: each in order, so one sees the ones before it. The
  /// block can be one `!js` itself, which sees none.
  pub fn read_vars(&mut self, root: &Value, dir: &Path) -> Result<()> {
    let Some(vars) = root.get("vars") else {
      return Ok(());
    };
    // Left empty, as any key can be, it sets none.
    if vars.is_null() {
      return Ok(());
    }
    let path = CfgPath::root().join("vars");
    let mut vars = vars.clone();
    if let Value::Tagged(_) = vars {
      self.resolve(&mut vars, &path, dir, &mut None)?;
    }
    let Value::Mapping(vars) = vars else {
      bail!("expected object at {path}");
    };
    for (name, mut value) in vars {
      let Value::String(name) = name else {
        bail!("a var's name must be a string at {path}");
      };
      self.resolve(&mut value, &path.join(&name), dir, &mut None)?;
      self.vars.insert(Value::String(name), value);
    }
    Ok(())
  }

  /// Replaces the `!js` values under the top-level keys of a file that
  /// `read` names, `dir` being its directory.
  pub fn resolve_keys(
    &mut self,
    root: &mut Value,
    dir: &Path,
    read: impl Fn(&str) -> bool,
  ) -> Result<()> {
    let Value::Mapping(map) = root else {
      return Ok(());
    };
    let mut ctx = None;
    for (key, item) in map.iter_mut() {
      if let Some(key) = key.as_str()
        && read(key)
      {
        self.resolve(item, &CfgPath::root().join(key), dir, &mut ctx)?;
      }
    }
    Ok(())
  }

  /// `ctx` for a value in a file at `dir`, with the vars read so far.
  fn ctx(&self, dir: &Path) -> Result<Value> {
    let mut runner = Mapping::new();
    runner.insert("root".into(), utf8(&self.runner.root)?.into());
    runner.insert("kind".into(), self.runner.kind.as_str().into());
    let mut ctx = Mapping::new();
    ctx.insert("dir".into(), utf8(dir)?.into());
    ctx.insert("runner".into(), Value::Mapping(runner));
    ctx.insert("vars".into(), Value::Mapping(self.vars.clone()));
    Ok(Value::Mapping(ctx))
  }

  /// Replaces the `!js` values in `value`. `ctx` is made at the first
  /// one, since most values have none, and is for this `dir` and the vars
  /// as they stand.
  fn resolve(
    &mut self,
    value: &mut Value,
    path: &CfgPath,
    dir: &Path,
    ctx: &mut Option<Value>,
  ) -> Result<()> {
    match value {
      Value::Mapping(map) => {
        for (key, item) in map.iter_mut() {
          let path = path.join(segment(key, path)?);
          self.resolve(item, &path, dir, ctx)?;
        }
      }
      Value::Sequence(items) => {
        for (i, item) in items.iter_mut().enumerate() {
          self.resolve(item, &path.join(i), dir, ctx)?;
        }
      }
      Value::Tagged(tagged) => {
        if tagged.tag != "js" {
          bail!(
            "unknown tag {}; the one tag dekit reads is !js at {path}",
            tagged.tag
          );
        }
        // YAML types a plain scalar by its look, so `!js 3000` arrives as
        // a number: the expression is its text either way.
        let src = match &tagged.value {
          Value::String(src) => src.clone(),
          Value::Number(n) => n.to_string(),
          Value::Bool(b) => b.to_string(),
          Value::Null => String::new(),
          _ => bail!("!js takes an expression, written as a string, at {path}"),
        };
        // A bare `!js` is null to YAML, as `!js null` is: nothing to run.
        if src.trim().is_empty() {
          bail!("!js has no expression at {path}; a null needs no tag");
        }
        let ctx = match ctx {
          Some(ctx) => &*ctx,
          None => ctx.insert(self.ctx(dir)?),
        };
        let exprs = match &mut self.exprs {
          Some(exprs) => exprs,
          None => self.exprs.insert(Exprs::new()?),
        };
        *value = exprs
          .eval(&src, ctx)
          .map_err(|err| anyhow!("!js: {err} at {path}"))?;
      }
      _ => (),
    }
    Ok(())
  }
}

/// A path as `!js` reads it.
fn utf8(path: &Path) -> Result<&str> {
  path.to_str().ok_or_else(|| {
    anyhow!(
      "{} is not valid UTF-8, so !js can't have it",
      path.display()
    )
  })
}

/// Whether `value` has a tag anywhere in it.
pub(crate) fn has_tag(value: &Value) -> bool {
  match value {
    Value::Tagged(_) => true,
    Value::Mapping(map) => map.iter().any(|(k, v)| has_tag(k) || has_tag(v)),
    Value::Sequence(items) => items.iter().any(has_tag),
    _ => false,
  }
}

/// A key as a step of a path. A key is never computed.
fn segment(key: &Value, path: &CfgPath) -> Result<String> {
  Ok(match key {
    Value::String(s) => s.clone(),
    Value::Number(n) => n.to_string(),
    Value::Bool(b) => b.to_string(),
    Value::Tagged(_) => bail!("a key can't have a tag at {path}"),
    _ => "?".to_string(),
  })
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::runner::RunnerKind;

  fn scope() -> Scope {
    Scope::new(&RunnerSpec {
      kind: RunnerKind::Project,
      root: "/p".into(),
    })
  }

  fn yaml(text: &str) -> Value {
    serde_yaml::from_str(text).unwrap()
  }

  /// A plain scalar YAML types for itself is the expression all the same.
  #[test]
  fn a_plain_scalar_is_an_expression() {
    let mut value = yaml("{a: !js 3000, b: !js true, d: !js 1e3}");
    scope()
      .resolve_keys(&mut value, Path::new("/p"), |_| true)
      .unwrap();
    assert_eq!(value, yaml("{a: 3000, b: true, d: 1000}"));
  }

  /// A `!js` with no expression is refused, not read as null.
  #[test]
  fn a_bare_js_is_refused() {
    for text in ["a: !js\n", "a: !js null\n", "a: !js ''\n"] {
      let mut value = yaml(text);
      let err = scope()
        .resolve_keys(&mut value, Path::new("/p"), |_| true)
        .unwrap_err();
      assert_eq!(
        err.to_string(),
        "!js has no expression at <config>.a; a null needs no tag",
        "{text:?}"
      );
    }
  }

  /// `vars` left empty sets none, as any key left empty is unset.
  #[test]
  fn empty_vars_set_none() {
    let mut scope = scope();
    scope.read_vars(&yaml("vars:\n"), Path::new("/p")).unwrap();
    assert!(scope.vars.is_empty());
  }

  /// `ctx` is made at the first `!js`: a file without one loads from a
  /// directory `!js` could not name.
  #[cfg(unix)]
  #[test]
  fn ctx_is_made_at_the_first_js() {
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;

    let dir = Path::new(OsStr::from_bytes(b"/p/\xff"));
    let mut value = yaml("{tasks: {a: {cmd: x}}}");
    scope().resolve_keys(&mut value, dir, |_| true).unwrap();
    assert_eq!(value, yaml("{tasks: {a: {cmd: x}}}"));
    let mut value = yaml("{tasks: {a: {cmd: !js ctx.dir}}}");
    let err = scope().resolve_keys(&mut value, dir, |_| true).unwrap_err();
    assert!(err.to_string().contains("not valid UTF-8"), "{err}");
  }
}
