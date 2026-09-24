use std::path::{Path, PathBuf};

/// Embeds every topic under `docs/` so `dekit help` ships its own sources.
fn main() {
  let root = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap());
  let docs = root.join("docs");
  println!("cargo:rerun-if-changed={}", docs.display());

  let mut files = Vec::new();
  walk(&docs, "", &mut files);
  files.sort();

  let mut out = String::new();
  out.push_str(&format!(
    "pub static MANIFEST: &str = include_str!({:?});\n",
    docs.join("index.yaml")
  ));
  out.push_str("pub static SOURCES: &[(&str, &str)] = &[\n");
  for (rel, abs) in &files {
    out.push_str(&format!("  ({:?}, include_str!({:?})),\n", rel, abs));
  }
  out.push_str("];\n");
  let out_dir = PathBuf::from(std::env::var_os("OUT_DIR").unwrap());
  std::fs::write(out_dir.join("docs_embed.rs"), out).unwrap();
}

fn walk(dir: &Path, rel: &str, files: &mut Vec<(String, String)>) {
  let mut entries: Vec<_> = std::fs::read_dir(dir)
    .unwrap()
    .map(|entry| entry.unwrap())
    .collect();
  entries.sort_by_key(|entry| entry.file_name());
  for entry in entries {
    let name = entry.file_name().to_string_lossy().into_owned();
    if name.starts_with('.') {
      continue;
    }
    let path = entry.path();
    let child = if rel.is_empty() {
      name.clone()
    } else {
      format!("{rel}/{name}")
    };
    if path.is_dir() {
      walk(&path, &child, files);
    } else if name.ends_with(".md") && name != "README.md" {
      files.push((child, path.to_string_lossy().into_owned()));
    }
  }
}
