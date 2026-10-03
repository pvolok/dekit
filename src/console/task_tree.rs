use std::{collections::HashMap, sync::Arc};

use indexmap::IndexSet;

use crate::console::{
  task_view::TaskView,
  widgets::list::{ListRow, ListState},
};
use crate::kernel::{
  task::{TaskId, TaskState},
  task_path::TaskPath,
};
use crate::term::grid::Rect;

/// The tasks under a path.
pub struct Group {
  path: Arc<TaskPath>,
  /// The paths right under this one, in list order.
  children: IndexSet<String>,
  tasks: usize,
  pub up: usize,
  pub failed: usize,
  pub collapsed: bool,
}

impl Group {
  pub fn len(&self) -> usize {
    self.tasks
  }

  fn add(&mut self, task: &TaskView) {
    self.tasks += 1;
    self.up += usize::from(task.is_up());
    self.failed += usize::from(task.failed());
  }

  fn remove(&mut self, task: &TaskView) {
    self.tasks -= 1;
    self.up -= usize::from(task.is_up());
    self.failed -= usize::from(task.failed());
  }
}

/// A path holds a task or has tasks under it, never both.
pub enum Node {
  Task(TaskView),
  Group(Group),
}

/// Tasks and the groups above them, with a cursor over the visible rows.
///
/// The rows are in the kernel's list order: each `add` says which task
/// the new one is listed after.
pub struct TaskTree {
  /// By path. The group at "" is the root, with every task under it.
  nodes: HashMap<String, Node>,
  paths: HashMap<TaskId, Arc<TaskPath>>,
  /// The visible paths, rebuilt when `dirty`.
  rows: Vec<Arc<TaskPath>>,
  dirty: bool,
  list: ListState,
}

impl TaskTree {
  pub fn new() -> Self {
    TaskTree {
      nodes: HashMap::new(),
      paths: HashMap::new(),
      rows: Vec::new(),
      dirty: false,
      list: ListState::new(1),
    }
  }

  /// `after` is the task listed just before the new one, if any.
  pub fn add(&mut self, task: TaskView, after: Option<TaskId>) {
    if self.nodes.contains_key(task.path.as_str()) {
      log::error!("The task list already has '{}'", task.path);
      return;
    }
    let after = after.and_then(|id| self.paths.get(&id)).cloned();
    for group in above(task.path.as_str()) {
      let node = self.nodes.entry(group.to_string()).or_insert_with(|| {
        Node::Group(Group {
          path: Arc::new(TaskPath::new(group).unwrap_or(TaskPath::root())),
          children: IndexSet::new(),
          tasks: 0,
          up: 0,
          failed: 0,
          collapsed: false,
        })
      });
      let (Node::Group(group), Some(child)) =
        (node, under(group, task.path.as_str()))
      else {
        continue;
      };
      group.add(&task);
      if group.children.contains(child) {
        continue;
      }
      // Right after the path the earlier task is at or under, or first
      // when that task is not under this group.
      let place = after
        .as_ref()
        .and_then(|after| under(group.path.as_str(), after.as_str()))
        .and_then(|beside| group.children.get_index_of(beside))
        .map_or(0, |index| index + 1);
      let last = group.children.len();
      group.children.insert(child.to_string());
      group.children.move_index(last, place);
    }
    self.paths.insert(task.id, task.path.clone());
    let key = task.path.as_str().to_string();
    self.nodes.insert(key, Node::Task(task));
    self.dirty = true;
  }

  pub fn remove(&mut self, id: TaskId) {
    let Some(path) = self.paths.remove(&id) else {
      return;
    };
    let Some(Node::Task(task)) = self.nodes.remove(path.as_str()) else {
      return;
    };
    for above in above(path.as_str()) {
      if let Some(Node::Group(group)) = self.nodes.get_mut(above) {
        group.remove(&task);
        if group.tasks == 0 {
          self.nodes.remove(above);
        }
      }
    }
    for above in above(path.as_str()) {
      let Some(child) = under(above, path.as_str()) else {
        continue;
      };
      if !self.nodes.contains_key(child)
        && let Some(Node::Group(group)) = self.nodes.get_mut(above)
      {
        group.children.shift_remove(child);
      }
    }
    self.dirty = true;
  }

  pub fn set_status(&mut self, id: TaskId, status: TaskState) {
    let Some(task) = self.task_mut(id) else {
      return;
    };
    let (was_up, had_failed) = (task.is_up(), task.failed());
    task.status = status;
    let (up, failed) = (task.is_up(), task.failed());
    if (was_up, had_failed) == (up, failed) {
      return;
    }
    let path = task.path.clone();
    for above in above(path.as_str()) {
      if let Some(Node::Group(group)) = self.nodes.get_mut(above) {
        group.up = group.up + usize::from(up) - usize::from(was_up);
        group.failed =
          group.failed + usize::from(failed) - usize::from(had_failed);
      }
    }
  }

  /// Not for the status: `set_status` keeps the group counts.
  pub fn task_mut(&mut self, id: TaskId) -> Option<&mut TaskView> {
    match self.nodes.get_mut(self.paths.get(&id)?.as_str())? {
      Node::Task(task) => Some(task),
      Node::Group(_) => None,
    }
  }

  pub fn iter(&self) -> impl Iterator<Item = &TaskView> {
    self.nodes.values().filter_map(|node| match node {
      Node::Task(task) => Some(task),
      Node::Group(_) => None,
    })
  }

  pub fn iter_mut(&mut self) -> impl Iterator<Item = &mut TaskView> {
    self.nodes.values_mut().filter_map(|node| match node {
      Node::Task(task) => Some(task),
      Node::Group(_) => None,
    })
  }

  /// A row as last drawn.
  pub fn row(&self, index: usize) -> Option<(&TaskPath, &Node)> {
    let path = self.rows.get(index)?;
    Some((path, self.nodes.get(path.as_str())?))
  }

  pub fn selected(&self) -> Option<(&TaskPath, &Node)> {
    self.row(self.list.selected()?)
  }

  /// The task of the selected row; None on a group row.
  pub fn current(&self) -> Option<&TaskView> {
    match self.selected()? {
      (_, Node::Task(task)) => Some(task),
      (_, Node::Group(_)) => None,
    }
  }

  pub fn next(&mut self) {
    self.refresh();
    self.list.next();
  }

  pub fn prev(&mut self) {
    self.refresh();
    self.list.prev();
  }

  /// Selects the `n`-th task in list order, or the last one when there
  /// are fewer, opening the groups it is in.
  pub fn select_task(&mut self, n: usize) {
    let mut seen = 0;
    let mut found = None;
    self.walk("", &mut |path, node| match node {
      Node::Task(_) if seen <= n => {
        seen += 1;
        found = Some(path.clone());
        false
      }
      Node::Task(_) => false,
      Node::Group(_) => true,
    });
    let Some(path) = found else {
      return;
    };
    for above in above(path.as_str()) {
      if let Some(Node::Group(group)) = self.nodes.get_mut(above)
        && group.collapsed
      {
        group.collapsed = false;
        self.dirty = true;
      }
    }
    self.select(&path);
  }

  /// Opens the selected group, or steps into it when it is open.
  pub fn expand(&mut self) {
    self.refresh();
    let Some(path) = self.selected_path() else {
      return;
    };
    match self.nodes.get_mut(path.as_str()) {
      Some(Node::Group(group)) if group.collapsed => {
        group.collapsed = false;
        self.dirty = true;
      }
      Some(Node::Group(_)) => self.list.next(),
      Some(Node::Task(_)) | None => (),
    }
  }

  /// Closes the selected group, or steps out to the group it is in.
  pub fn collapse(&mut self) {
    self.refresh();
    let Some(path) = self.selected_path() else {
      return;
    };
    match self.nodes.get_mut(path.as_str()) {
      Some(Node::Group(group)) if !group.collapsed => {
        group.collapsed = true;
        self.dirty = true;
      }
      Some(Node::Group(_) | Node::Task(_)) | None => {
        if let Some(parent) = path.parent() {
          self.select(&parent);
        }
      }
    }
  }

  /// Selects a row as last drawn; a group also opens or closes.
  pub fn click(&mut self, index: usize) {
    let Some(path) = self.rows.get(index).cloned() else {
      return;
    };
    if let Some(Node::Group(group)) = self.nodes.get_mut(path.as_str()) {
      group.collapsed = !group.collapsed;
      self.dirty = true;
    }
    self.select(&path);
  }

  pub fn scroll_by(&mut self, delta: isize) {
    self.list.scroll_by(delta);
  }

  pub fn rows(&mut self, area: Rect) -> impl Iterator<Item = ListRow> + use<> {
    self.refresh();
    self.list.rows(area)
  }

  pub fn row_at(&self, x: u16, y: u16) -> Option<ListRow> {
    self.list.row_at(x, y)
  }

  fn selected_path(&self) -> Option<Arc<TaskPath>> {
    self.rows.get(self.list.selected()?).cloned()
  }

  fn select(&mut self, path: &TaskPath) {
    self.refresh();
    if let Some(index) = self.rows.iter().position(|row| **row == *path) {
      self.list.select(index);
    }
  }

  /// Rebuilds the rows and puts the cursor back on the selected row, or on
  /// the row that took its place.
  fn refresh(&mut self) {
    if !self.dirty {
      return;
    }
    self.dirty = false;
    let selected = self.selected_path();
    let mut rows = std::mem::take(&mut self.rows);
    rows.clear();
    self.walk("", &mut |path, node| {
      rows.push(path.clone());
      match node {
        Node::Task(_) => false,
        Node::Group(group) => !group.collapsed,
      }
    });
    self.rows = rows;
    let index = selected
      .and_then(|path| self.rows.iter().position(|row| *row == path))
      .or(self.list.selected())
      .unwrap_or(0);
    self.list.set(self.rows.len(), index);
  }

  /// Visits the rows under a group in list order. `visit` says whether
  /// to go on into a group.
  fn walk(
    &self,
    group: &str,
    visit: &mut impl FnMut(&Arc<TaskPath>, &Node) -> bool,
  ) {
    let Some(Node::Group(group)) = self.nodes.get(group) else {
      return;
    };
    for child in &group.children {
      match self.nodes.get(child) {
        Some(node @ Node::Task(task)) => {
          visit(&task.path, node);
        }
        Some(node @ Node::Group(under)) => {
          if visit(&under.path, node) {
            self.walk(child, visit);
          }
        }
        None => (),
      }
    }
  }
}

/// The groups a path is under, from the root: `a/b/c` gives "", `a` and
/// `a/b`.
fn above(path: &str) -> impl Iterator<Item = &str> {
  std::iter::once("").chain(path.match_indices('/').map(|(i, _)| &path[..i]))
}

/// The path right under `group` that `path` is at or under: `a/b` for
/// `a` and `a/b/c`.
fn under<'a>(group: &str, path: &'a str) -> Option<&'a str> {
  let start = if group.is_empty() {
    0
  } else {
    path.strip_prefix(group)?.strip_prefix('/')?;
    group.len() + 1
  };
  let end = path[start..].find('/').map_or(path.len(), |i| start + i);
  Some(&path[..end])
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::kernel::kernel_message::SharedVt;
  use crate::kernel::path_trie::PathTrie;
  use crate::kernel::task::ExitInfo;
  use crate::term::Screen;

  fn task(id: usize, path: &str) -> TaskView {
    TaskView {
      id: TaskId(id),
      label: None,
      path: Arc::new(TaskPath::new(path).unwrap()),
      kind: crate::kernel::task::TaskKind::Service,
      status: TaskState::Idle,
      vt: SharedVt::new(Screen::new(crate::term::Size::default(), 0)),
      present: None,
    }
  }

  /// A tree the kernel's `Added` events built: the tasks are registered
  /// in this order, with ids from 1.
  fn tree(paths: &[&str]) -> TaskTree {
    let mut trie = PathTrie::new();
    let mut tree = TaskTree::new();
    for (i, path) in paths.iter().enumerate() {
      add(&mut trie, &mut tree, i + 1, path, None);
    }
    tree
  }

  fn add(
    trie: &mut PathTrie,
    tree: &mut TaskTree,
    id: usize,
    path: &str,
    after: Option<&str>,
  ) {
    let after = after.map(|after| TaskPath::new(after).unwrap());
    let path = TaskPath::new(path).unwrap();
    trie.insert(&path, TaskId(id), after.as_ref()).unwrap();
    tree.add(task(id, path.as_str()), trie.before(&path));
  }

  /// The visible rows as `path` indented by depth, `*` on the selection.
  fn shown(tree: &mut TaskTree) -> Vec<String> {
    let rows: Vec<ListRow> = tree.rows(Rect::new(0, 0, 20, 50)).collect();
    rows
      .iter()
      .map(|r| {
        let (path, node) = tree.row(r.index).unwrap();
        let mark = if r.selected { "*" } else { "" };
        let kind = match node {
          Node::Task(_) => "",
          Node::Group(_) => "/",
        };
        format!("{}{}{}{}", "  ".repeat(path.depth() - 1), path, kind, mark)
      })
      .collect()
  }

  #[test]
  fn keeps_a_group_together_where_its_first_task_is() {
    let mut tree = tree(&["web/dev", "db", "web/ui", "api/a/b"]);
    assert_eq!(
      shown(&mut tree),
      [
        "web/*",
        "  web/dev",
        "  web/ui",
        "db",
        "api/",
        "  api/a/",
        "    api/a/b"
      ]
    );
  }

  #[test]
  fn lists_a_task_right_after_the_one_before_it() {
    let mut tree = tree(&["db", "web/api", "cache", "web/ui"]);
    tree.add(task(5, "web/api_5"), Some(TaskId(2)));
    tree.add(task(6, "db_6"), Some(TaskId(1)));
    tree.add(task(7, "new/a"), Some(TaskId(4)));
    tree.add(task(8, "first"), None);
    assert_eq!(
      shown(&mut tree),
      [
        "first*",
        "db",
        "db_6",
        "web/",
        "  web/api",
        "  web/api_5",
        "  web/ui",
        "new/",
        "  new/a",
        "cache"
      ]
    );
  }

  /// The order `dekit ls` and the task list share, whatever order the
  /// tasks come in.
  #[test]
  fn lists_tasks_as_the_kernel_does() {
    let paths = ["c/x/1", "a", "c/y", "b/1", "c/x/2", "b/2", "d"];
    // As a batch registers them: a place for each in list order, then
    // the tasks in another order.
    let mut trie = PathTrie::new();
    let mut tree = TaskTree::new();
    for path in paths {
      trie.place(&TaskPath::new(path).unwrap());
    }
    for i in [6, 2, 0, 4, 5, 3, 1] {
      add(&mut trie, &mut tree, i + 1, paths[i], None);
    }
    add(&mut trie, &mut tree, 8, "c/y_8", Some("c/y"));
    add(&mut trie, &mut tree, 9, "a_9", Some("a"));
    trie.remove(&TaskPath::new("c/x/1").unwrap());
    tree.remove(TaskId(1));
    add(&mut trie, &mut tree, 10, "c/x/3", None);
    add(&mut trie, &mut tree, 11, "e/f", None);

    let listed: Vec<TaskId> = shown(&mut tree)
      .iter()
      .filter(|row| !row.contains("/*") && !row.ends_with('/'))
      .filter_map(|row| {
        let path = TaskPath::new(row.trim().trim_end_matches('*')).ok()?;
        trie.resolve(&path)
      })
      .collect();
    assert_eq!(listed, trie.tasks());
    assert_eq!(
      listed.iter().map(|id| id.0).collect::<Vec<_>>(),
      [5, 10, 3, 8, 2, 9, 4, 6, 7, 11]
    );
  }

  #[test]
  fn keeps_the_selection_on_its_row() {
    let mut tree = tree(&["b", "c"]);
    shown(&mut tree);
    tree.next();
    assert_eq!(tree.current().map(|t| t.id), Some(TaskId(2)));
    tree.add(task(3, "a"), None);
    assert_eq!(shown(&mut tree), ["a", "b", "c*"]);
    tree.remove(TaskId(2));
    assert_eq!(
      shown(&mut tree),
      ["a", "b*"],
      "the last row takes its place"
    );
  }

  #[test]
  fn collapses_and_acts_on_groups() {
    let mut tree = tree(&["web/api", "web/ui", "zz"]);
    tree.next();
    assert_eq!(shown(&mut tree), ["web/", "  web/api*", "  web/ui", "zz"]);

    tree.collapse();
    assert_eq!(shown(&mut tree), ["web/*", "  web/api", "  web/ui", "zz"]);
    assert_eq!(tree.current().map(|t| t.id), None);

    tree.collapse();
    assert_eq!(shown(&mut tree), ["web/*", "zz"]);
    tree.expand();
    assert_eq!(shown(&mut tree), ["web/*", "  web/api", "  web/ui", "zz"]);
    tree.expand();
    assert_eq!(tree.current().map(|t| t.id), Some(TaskId(1)));
  }

  #[test]
  fn counts_tasks_up_and_failed_under_a_group() {
    let mut tree = tree(&["web/api", "web/ui"]);
    let counts = |tree: &TaskTree| match tree.nodes.get("web") {
      Some(Node::Group(group)) => Some((group.len(), group.up, group.failed)),
      Some(Node::Task(_)) | None => None,
    };
    tree.set_status(TaskId(1), TaskState::Running);
    tree.set_status(TaskId(2), TaskState::Exited(ExitInfo::code(1)));
    assert_eq!(counts(&tree), Some((2, 1, 1)));

    tree.remove(TaskId(2));
    assert_eq!(counts(&tree), Some((1, 1, 0)));
    tree.remove(TaskId(1));
    assert_eq!(counts(&tree), None);
  }

  #[test]
  fn selecting_a_hidden_task_opens_its_groups() {
    let mut tree = tree(&["a/b/c", "d"]);
    tree.select_task(0);
    tree.collapse();
    tree.collapse();
    tree.collapse();
    tree.collapse();
    assert_eq!(shown(&mut tree), ["a/*", "d"]);
    tree.select_task(0);
    assert_eq!(shown(&mut tree), ["a/", "  a/b/", "    a/b/c*", "d"]);
  }

  #[test]
  fn selects_tasks_by_number() {
    let mut tree = tree(&["web/ui", "db", "web/api"]);
    tree.select_task(1);
    assert_eq!(tree.current().map(|t| t.id), Some(TaskId(3)));
    tree.select_task(7);
    assert_eq!(
      tree.current().map(|t| t.id),
      Some(TaskId(2)),
      "past the end selects the last task"
    );
  }
}
