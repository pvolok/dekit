use rquickjs::{Ctx, Exception, Object, function::Opt};

use crate::js::rquickjs_ext::ObjectExt;

async fn stat_fn(ctx: Ctx<'_>, path: String) -> rquickjs::Result<Object<'_>> {
  let meta = tokio::fs::metadata(&path)
    .await
    .map_err(|e| Exception::throw_message(&ctx, &format!("fs.stat: {e}")))?;
  // `metadata` follows links; `isSymlink` is about the path itself.
  let link_meta = tokio::fs::symlink_metadata(&path)
    .await
    .map_err(|e| Exception::throw_message(&ctx, &format!("fs.stat: {e}")))?;
  let obj = Object::new(ctx.clone())?;
  obj.set("size", meta.len() as f64)?;
  obj.set("isDir", meta.is_dir())?;
  obj.set("isFile", meta.is_file())?;
  obj.set("isSymlink", link_meta.file_type().is_symlink())?;
  let mtime = meta
    .modified()
    .ok()
    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
    .map(|d| d.as_millis() as f64)
    .unwrap_or(0.0);
  obj.set("mtime", mtime)?;
  Ok(obj)
}

pub fn init(ctx: Ctx<'_>) -> rquickjs::Result<Object<'_>> {
  let obj = Object::new(ctx.clone())?;

  obj.def_fn_async("read", async |ctx: Ctx<'_>, path: String| {
    tokio::fs::read_to_string(&path)
      .await
      .map_err(|e| Exception::throw_message(&ctx, &format!("fs.read: {e}")))
  })?;

  obj.def_fn_async(
    "write",
    async |ctx: Ctx<'_>, path: String, content: String| {
      tokio::fs::write(&path, content)
        .await
        .map_err(|e| Exception::throw_message(&ctx, &format!("fs.write: {e}")))
    },
  )?;

  obj.def_fn_async("exists", async |ctx: Ctx<'_>, path: String| {
    match tokio::fs::try_exists(&path).await {
      Ok(exists) => Ok(exists),
      // A path under a regular file cannot exist.
      Err(e) if e.kind() == std::io::ErrorKind::NotADirectory => Ok(false),
      Err(e) => Err(Exception::throw_message(&ctx, &format!("fs.exists: {e}"))),
    }
  })?;

  obj.def_fn_async(
    "mkdir",
    async |ctx: Ctx<'_>, path: String, Opt(opts): Opt<Object<'_>>| {
      let recursive = opts
        .as_ref()
        .and_then(|o| o.get::<_, bool>("recursive").ok())
        .unwrap_or(false);
      if recursive {
        tokio::fs::create_dir_all(&path).await
      } else {
        tokio::fs::create_dir(&path).await
      }
      .map_err(|e| Exception::throw_message(&ctx, &format!("fs.mkdir: {e}")))
    },
  )?;

  obj.def_fn_async(
    "rm",
    async |ctx: Ctx<'_>, path: String, Opt(opts): Opt<Object<'_>>| {
      let recursive = opts
        .as_ref()
        .and_then(|o| o.get::<_, bool>("recursive").ok())
        .unwrap_or(false);
      let res = if recursive {
        match tokio::fs::remove_dir_all(&path).await {
          Ok(()) => Ok(()),
          Err(_) => tokio::fs::remove_file(&path).await,
        }
      } else {
        match tokio::fs::remove_file(&path).await {
          Ok(()) => Ok(()),
          Err(_) => tokio::fs::remove_dir(&path).await,
        }
      };
      res.map_err(|e| Exception::throw_message(&ctx, &format!("fs.rm: {e}")))
    },
  )?;

  obj.def_fn_async("readDir", async |ctx: Ctx<'_>, path: String| {
    let mut entries = Vec::new();
    let mut dir = tokio::fs::read_dir(&path).await.map_err(|e| {
      Exception::throw_message(&ctx, &format!("fs.readDir: {e}"))
    })?;
    while let Some(entry) = dir.next_entry().await.map_err(|e| {
      Exception::throw_message(&ctx, &format!("fs.readDir: {e}"))
    })? {
      entries.push(entry.file_name().to_string_lossy().to_string());
    }
    Ok::<_, rquickjs::Error>(entries)
  })?;

  obj.def_fn_async("stat", stat_fn)?;

  obj.def_fn_async(
    "rename",
    async |ctx: Ctx<'_>, from: String, to: String| {
      tokio::fs::rename(&from, &to)
        .await
        .map_err(|e| Exception::throw_message(&ctx, &format!("fs.rename: {e}")))
    },
  )?;

  obj.def_fn_async(
    "copy",
    async |ctx: Ctx<'_>, from: String, to: String| {
      tokio::fs::copy(&from, &to)
        .await
        .map(|_| ())
        .map_err(|e| Exception::throw_message(&ctx, &format!("fs.copy: {e}")))
    },
  )?;

  Ok(obj)
}
