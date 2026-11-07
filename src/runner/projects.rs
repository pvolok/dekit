//! The projects the user keeps: the `projects:` list of the user config.
//! An entry is a project root, or a dir and `/*` for every dir in it. A
//! project goes by its root's basename.
//!
//! The list belongs to no runner, so no runner's config carries it: it
//! is read where it is used, fresh each time.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use anyhow::{Context, bail};
use serde_yaml::Value;

use crate::runner::{home_dir, user_config_file};

/// The user config's key.
const KEY: &str = "projects";

/// A project the list names.
pub struct Project {
  /// Its root's basename.
  pub name: String,
  /// Canonical when it exists.
  pub root: PathBuf,
  /// The `dir/*` entry that lists it, when that comes first.
  pub from: Option<String>,
}

/// The list as read and expanded.
pub struct Projects {
  /// In the list's order, each root once.
  pub projects: Vec<Project>,
  /// Entries skipped, and dirs that can't be read.
  pub warnings: Vec<String>,
  home: Option<PathBuf>,
}

/// The list in the user config, expanded. Fails only when the file can't
/// be read or `projects` is no list; bad entries are warnings.
pub fn read() -> anyhow::Result<Projects> {
  let path = user_config_file()?;
  let text = match std::fs::read_to_string(&path) {
    Err(err) if err.kind() == std::io::ErrorKind::NotFound => String::new(),
    read => read.with_context(|| format!("can't read {}", path.display()))?,
  };
  Projects::parse(&text, home_dir().map(canonical))
    .with_context(|| format!("in {}", path.display()))
}

/// The list in `yaml`, with `home` for `~`.
#[cfg(test)]
pub fn parse_for_test(yaml: &str, home: &Path) -> Projects {
  Projects::parse(yaml, Some(home.into())).unwrap()
}

impl Projects {
  /// `home` is canonical: roots are compared and shown canonical.
  fn parse(text: &str, home: Option<PathBuf>) -> anyhow::Result<Projects> {
    let value: Value =
      serde_yaml::from_str(text).context("the file is not valid YAML")?;
    let items = match value.get(KEY) {
      None | Some(Value::Null) => Vec::new(),
      Some(Value::Sequence(items)) => items.clone(),
      Some(_) => bail!("'{KEY}' is not a list"),
    };
    let mut projects = Projects {
      projects: Vec::new(),
      warnings: Vec::new(),
      home,
    };
    let mut seen = HashSet::new();
    for (index, item) in items.iter().enumerate() {
      let listed = match item.as_str() {
        Some(text) => projects.roots(text).map(|roots| (text, roots)),
        None => Err(anyhow::anyhow!("an entry is a path")),
      };
      let (text, (roots, all)) = match listed {
        Ok(listed) => listed,
        Err(err) => {
          (projects.warnings).push(format!("ignoring {KEY}[{index}]: {err:#}"));
          continue;
        }
      };
      for root in roots {
        if seen.insert(root.clone()) {
          projects.projects.push(Project {
            name: basename(&root),
            from: all.then(|| text.to_string()),
            root,
          });
        }
      }
    }
    Ok(projects)
  }

  /// The roots an entry lists, and whether it is a `dir/*`.
  fn roots(&self, text: &str) -> anyhow::Result<(Vec<PathBuf>, bool)> {
    if !(text.starts_with('/') || text == "~" || text.starts_with("~/")) {
      bail!("'{text}' is not absolute or under ~/");
    }
    let (dir, all) = split_all(text);
    let Some(dir) = untilde(dir, self.home.as_deref()) else {
      bail!("HOME is not set");
    };
    let dir = canonical(dir);
    if !all {
      return Ok((vec![dir], false));
    }
    let roots = subdirs(&dir)
      .with_context(|| format!("can't read {}", self.tilde(&dir)))?;
    Ok((roots, true))
  }

  /// A path under `~/` when it is in the home dir.
  pub fn tilde(&self, path: &Path) -> String {
    tilde_with(path, self.home.as_deref())
  }
}

/// `dir/*` as the dir and true, else the path and false.
fn split_all(text: &str) -> (&str, bool) {
  match text.strip_suffix("/*") {
    Some("") => ("/", true),
    Some(dir) => (dir, true),
    None => (text, false),
  }
}

/// The dirs in `dir`, hidden ones left out, by name.
fn subdirs(dir: &Path) -> std::io::Result<Vec<PathBuf>> {
  let mut roots: Vec<PathBuf> = std::fs::read_dir(dir)?
    .filter_map(Result::ok)
    .filter(|e| !e.file_name().to_string_lossy().starts_with('.'))
    .map(|e| e.path())
    .filter(|path| path.is_dir())
    .map(canonical)
    .collect();
  roots.sort_by_cached_key(|root| basename(root).to_lowercase());
  Ok(roots)
}

fn canonical(path: PathBuf) -> PathBuf {
  dunce::canonicalize(&path).unwrap_or(path)
}

fn basename(root: &Path) -> String {
  root
    .file_name()
    .map(|name| name.to_string_lossy().into_owned())
    .unwrap_or_else(|| root.display().to_string())
}

/// `~` and `~/...` under `home`; none when that is needed and there is
/// no home. Any other path is as given.
pub fn untilde(path: &str, home: Option<&Path>) -> Option<PathBuf> {
  if path != "~" && !path.starts_with("~/") {
    return Some(PathBuf::from(path));
  }
  Some(match path.strip_prefix("~/") {
    Some(rest) => home?.join(rest),
    None => home?.to_path_buf(),
  })
}

/// A path, under `~/` when it is in `home`.
fn tilde_with(path: &Path, home: Option<&Path>) -> String {
  if let Some(home) = home
    && let Ok(rest) = path.strip_prefix(home)
  {
    let rest = rest.to_string_lossy();
    return if rest.is_empty() {
      "~".to_string()
    } else {
      format!("~/{rest}")
    };
  }
  path.to_string_lossy().into_owned()
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::test_util::TempDir;

  /// A home with `dev/{api,web,.hidden}`, a file `dev/notes.txt`, and
  /// `oss/mprocs`.
  fn home(name: &str) -> (TempDir, PathBuf) {
    let dir = TempDir::new(name);
    let home = dunce::canonicalize(&dir.0).unwrap();
    for path in ["dev/api", "dev/web", "dev/.hidden", "oss/mprocs"] {
      std::fs::create_dir_all(home.join(path)).unwrap();
    }
    std::fs::write(home.join("dev/notes.txt"), "").unwrap();
    (dir, home)
  }

  fn names(projects: &Projects) -> Vec<&str> {
    (projects.projects.iter())
      .map(|p| p.name.as_str())
      .collect()
  }

  #[test]
  fn a_star_lists_every_dir_in_it() {
    let (_dir, home) = home("projects-star");
    let projects = parse_for_test(
      "projects:\n  - ~/oss/mprocs\n  - ~/dev/*\n  - ~/dev/api\n  - ~/gone\n",
      &home,
    );
    assert_eq!(names(&projects), ["mprocs", "api", "web", "gone"]);
    assert_eq!(projects.projects[1].root, home.join("dev/api"));
    assert_eq!(projects.projects[1].from.as_deref(), Some("~/dev/*"));
    assert_eq!(projects.projects[0].from, None);
    assert!(projects.warnings.is_empty(), "{:?}", projects.warnings);
  }

  #[test]
  fn bad_entries_are_warnings() {
    let (_dir, home) = home("projects-bad");
    let projects = parse_for_test(
      "projects:\n  - dev\n  - {path: ~/dev/api}\n  - ~/nope/*\n  - ~/dev/web\n",
      &home,
    );
    assert_eq!(names(&projects), ["web"]);
    assert_eq!(projects.warnings.len(), 3, "{:?}", projects.warnings);
    assert!(projects.warnings[2].starts_with("ignoring projects[2]: "));
    let broken = Projects::parse("projects: x\n", None);
    assert!(broken.is_err());
  }

  #[test]
  fn the_home_dir_and_the_root() {
    let (_dir, home) = home("projects-home");
    let projects = parse_for_test("projects:\n  - \"~\"\n", &home);
    assert_eq!(projects.projects[0].root, home);
    assert_eq!(split_all("~/*"), ("~", true));
    assert_eq!(split_all("/*"), ("/", true));
    assert_eq!(split_all("/a"), ("/a", false));
  }

  #[test]
  fn tilde_paths() {
    let home = Path::new("/home/me");
    assert_eq!(tilde_with(Path::new("/home/me/dev"), Some(home)), "~/dev");
    assert_eq!(tilde_with(home, Some(home)), "~");
    assert_eq!(tilde_with(Path::new("/srv/x"), Some(home)), "/srv/x");
    assert_eq!(untilde("~", Some(home)), Some(home.to_path_buf()));
    assert_eq!(untilde("~/dev", Some(home)), Some(home.join("dev")));
    assert_eq!(untilde("~/dev", None), None);
    assert_eq!(untilde("/srv", None), Some("/srv".into()));
  }
}
