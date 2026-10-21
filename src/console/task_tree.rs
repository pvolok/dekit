use std::{
  collections::{BTreeMap, HashMap},
  sync::Arc,
};

use indexmap::IndexSet;

use crate::console::{
  action::FoldOp,
  task_view::TaskView,
  widgets::list::{ListRow, ListState},
};
use crate::kernel::{
  task::{TaskId, TaskState},
  task_path::TaskPath,
};
use crate::term::grid::Rect;

/// The tasks under a path: a group, or a folder.
pub struct Group {
  path: Arc<TaskPath>,
  /// The paths right under this one, in list order.
  children: IndexSet<String>,
  tasks: usize,
  pub up: usize,
  pub failed: usize,
  pub collapsed: bool,
  /// The folder's id, when it is one: it stays while it is empty.
  pub folder: Option<TaskId>,
}

impl Group {
  fn new(path: &str, collapsed: bool) -> Self {
    Group {
      path: Arc::new(TaskPath::new(path).unwrap_or(TaskPath::root())),
      children: IndexSet::new(),
      tasks: 0,
      up: 0,
      failed: 0,
      collapsed,
      folder: None,
    }
  }

  pub fn len(&self) -> usize {
    self.tasks
  }

  /// Nothing is under it: an empty folder.
  pub fn is_empty(&self) -> bool {
    self.children.is_empty()
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

/// A path holds a task, or is a group or folder, never both.
pub enum Node {
  Task(TaskView),
  Group(Group),
}

/// Tasks and folders, and the groups above them, with a cursor over the
/// visible rows.
///
/// The rows are in the kernel's list order: each `add` says which task
/// or folder the new one is listed after.
pub struct TaskTree {
  /// By path. The group at "" is the root, with every task under it.
  nodes: HashMap<String, Node>,
  /// Where each task and folder is.
  paths: HashMap<TaskId, Arc<TaskPath>>,
  /// The visible paths, rebuilt on every change.
  rows: Vec<Arc<TaskPath>>,
  list: ListState,
  /// Whether each group not listed yet starts collapsed, by path: as the
  /// console had them when it was saved. Each is taken once.
  seeds: HashMap<String, bool>,
}

impl TaskTree {
  pub fn new() -> Self {
    TaskTree {
      nodes: HashMap::new(),
      paths: HashMap::new(),
      rows: Vec::new(),
      list: ListState::new(1),
      seeds: HashMap::new(),
    }
  }

  /// A tree whose groups start as `groups` says, once listed.
  pub fn with_groups(groups: BTreeMap<String, bool>) -> Self {
    TaskTree {
      seeds: groups.into_iter().collect(),
      ..TaskTree::new()
    }
  }

  /// How the groups are shown, by path, for `with_groups`: each folder,
  /// whose entry may say otherwise by the next start, and each other
  /// group that is collapsed. A tree with neither gives none, which a
  /// save then leaves out.
  pub fn groups(&self) -> BTreeMap<String, bool> {
    self
      .nodes
      .iter()
      .filter_map(|(path, node)| match node {
        Node::Group(group)
          if !path.is_empty()
            && (group.collapsed || group.folder.is_some()) =>
        {
          Some((path.clone(), group.collapsed))
        }
        Node::Group(_) | Node::Task(_) => None,
      })
      .collect()
  }

  /// `after` is the task or folder listed just before the new one, if
  /// any.
  pub fn add(&mut self, task: TaskView, after: Option<TaskId>) {
    if self.nodes.contains_key(task.path.as_str()) {
      log::error!("The task list already has '{}'", task.path);
      return;
    }
    let after = after.and_then(|id| self.paths.get(&id)).cloned();
    let path = task.path.clone();
    self.list(&path, after, Some(&task));
    self.paths.insert(task.id, path.clone());
    self.nodes.insert(path.to_string(), Node::Task(task));
    self.rebuild();
  }

  /// A folder, `collapsed` unless its group is listed already or was
  /// saved otherwise; `after` as for `add`.
  pub fn add_folder(
    &mut self,
    id: TaskId,
    path: TaskPath,
    after: Option<TaskId>,
    collapsed: bool,
  ) {
    if let Some(Node::Task(_)) = self.nodes.get(path.as_str()) {
      log::error!("The task list has a task at folder '{}'", path);
      return;
    }
    let after = after.and_then(|id| self.paths.get(&id)).cloned();
    let path = Arc::new(path);
    self.list(&path, after, None);
    let seeds = &mut self.seeds;
    let node = self.nodes.entry(path.to_string()).or_insert_with(|| {
      let collapsed = seeds.remove(path.as_str()).unwrap_or(collapsed);
      Node::Group(Group::new(path.as_str(), collapsed))
    });
    if let Node::Group(group) = node {
      group.folder = Some(id);
    }
    self.paths.insert(id, path);
    self.rebuild();
  }

  /// Lists `path` in each group above it, the groups not there made,
  /// right after the path `after` is at or under, or first when that is
  /// not under the group. A path already there moves there too: a moved
  /// task brings its groups along. Each group counts `task`.
  fn list(
    &mut self,
    path: &Arc<TaskPath>,
    after: Option<Arc<TaskPath>>,
    task: Option<&TaskView>,
  ) {
    for group in above(path.as_str()) {
      let seeds = &mut self.seeds;
      let node = self.nodes.entry(group.to_string()).or_insert_with(|| {
        let collapsed = seeds.remove(group).unwrap_or(false);
        Node::Group(Group::new(group, collapsed))
      });
      let (Node::Group(group), Some(child)) =
        (node, under(group, path.as_str()))
      else {
        continue;
      };
      if let Some(task) = task {
        group.add(task);
      }
      let beside = after
        .as_ref()
        .and_then(|after| under(group.path.as_str(), after.as_str()));
      if beside == Some(child) {
        continue;
      }
      let beside =
        beside.and_then(|beside| group.children.get_index_of(beside));
      let index = match group.children.get_index_of(child) {
        Some(index) => index,
        None => {
          group.children.insert(child.to_string());
          group.children.len() - 1
        }
      };
      let place = match beside {
        Some(beside) if beside < index => beside + 1,
        Some(beside) => beside,
        None => 0,
      };
      group.children.move_index(index, place);
    }
  }

  /// A task or folder is gone.
  pub fn remove(&mut self, id: TaskId) {
    match self.is_folder(id) {
      true => self.take_folder(id),
      false => drop(self.take(id)),
    }
  }

  fn is_folder(&self, id: TaskId) -> bool {
    self.paths.get(&id).is_some_and(|path| {
      matches!(
        self.nodes.get(path.as_str()),
        Some(Node::Group(group)) if group.folder == Some(id)
      )
    })
  }

  /// Takes a folder away; its group stays while something is under it.
  fn take_folder(&mut self, id: TaskId) {
    let Some(path) = self.paths.remove(&id) else {
      return;
    };
    if let Some(Node::Group(group)) = self.nodes.get_mut(path.as_str()) {
      group.folder = None;
      if group.children.is_empty() {
        self.nodes.remove(path.as_str());
      }
    }
    self.prune(path.as_str());
    self.rebuild();
  }

  /// Takes `path` off its group's list once nothing is at it, and each
  /// group above that is left with nothing in it and is no folder.
  fn prune(&mut self, path: &str) {
    let mut child = path;
    for parent in above(path).collect::<Vec<_>>().into_iter().rev() {
      if self.nodes.contains_key(child) {
        return;
      }
      let Some(Node::Group(group)) = self.nodes.get_mut(parent) else {
        return;
      };
      group.children.shift_remove(child);
      if !parent.is_empty()
        && group.children.is_empty()
        && group.folder.is_none()
      {
        self.nodes.remove(parent);
      }
      child = parent;
    }
  }

  /// Lists a task at another path, or elsewhere at its own, right after
  /// `after`. The selection stays on it when it was, else on the row it
  /// was on, and a group the move empties and fills again stays
  /// collapsed.
  pub fn move_task(
    &mut self,
    id: TaskId,
    path: TaskPath,
    after: Option<TaskId>,
  ) {
    if self.is_folder(id) {
      return self.move_folder(id, path, after);
    }
    let selected = self.current().map(|task| task.id) == Some(id);
    let was = self.selected_path();
    let collapsed: Vec<String> = self
      .paths
      .get(&id)
      .into_iter()
      .flat_map(|path| above(path.as_str()))
      .filter(|group| {
        matches!(self.nodes.get(*group), Some(Node::Group(g)) if g.collapsed)
      })
      .map(str::to_string)
      .collect();
    let Some(mut task) = self.take(id) else {
      return;
    };
    task.path = Arc::new(path);
    let path = task.path.clone();
    self.add(task, after);
    for group in collapsed {
      if let Some(Node::Group(group)) = self.nodes.get_mut(&group) {
        group.collapsed = true;
      }
    }
    self.rebuild();
    match was {
      _ if selected => self.select(&path),
      Some(was) => self.select(&was),
      None => (),
    }
  }

  /// A folder listed elsewhere: its group is shown as it was, and the
  /// selection stays on it when it was.
  fn move_folder(&mut self, id: TaskId, path: TaskPath, after: Option<TaskId>) {
    let Some(old) = self.paths.get(&id).cloned() else {
      return;
    };
    let was = self.selected_path();
    let selected = was.as_ref() == Some(&old);
    let shown = |tree: &Self, path: &str| match tree.nodes.get(path) {
      Some(Node::Group(group)) => group.collapsed,
      Some(Node::Task(_)) | None => false,
    };
    let own = shown(self, old.as_str());
    // The groups above it that it leaves empty are shown as they were
    // when they come again.
    let above_it: Vec<String> = above(old.as_str())
      .filter(|group| shown(self, group))
      .map(str::to_string)
      .collect();
    self.take_folder(id);
    self.add_folder(id, path.clone(), after, own);
    let fold = above_it.iter().map(|group| (group.as_str(), true));
    for (group, collapsed) in fold.chain([(path.as_str(), own)]) {
      if let Some(Node::Group(group)) = self.nodes.get_mut(group) {
        group.collapsed = collapsed;
      }
    }
    self.rebuild();
    match was {
      _ if selected => self.select(&path),
      Some(was) => self.select(&was),
      None => (),
    }
  }

  fn take(&mut self, id: TaskId) -> Option<TaskView> {
    let path = self.paths.remove(&id)?;
    let Some(Node::Task(task)) = self.nodes.remove(path.as_str()) else {
      return None;
    };
    for above in above(path.as_str()) {
      if let Some(Node::Group(group)) = self.nodes.get_mut(above) {
        group.remove(&task);
      }
    }
    self.prune(path.as_str());
    self.rebuild();
    Some(task)
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

  /// A row as listed now.
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
    self.list.next();
  }

  pub fn prev(&mut self) {
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
    self.reveal(path.as_str());
    self.select(&path);
  }

  /// Opens the selected group, or steps into it when it is open.
  pub fn expand(&mut self) {
    let Some(path) = self.selected_path() else {
      return;
    };
    match self.nodes.get_mut(path.as_str()) {
      Some(Node::Group(group)) if group.collapsed => {
        group.collapsed = false;
        self.rebuild();
      }
      Some(Node::Group(_)) => self.list.next(),
      Some(Node::Task(_)) | None => (),
    }
  }

  /// Closes the selected group, or steps out to the group it is in.
  pub fn collapse(&mut self) {
    let Some(path) = self.selected_path() else {
      return;
    };
    match self.nodes.get_mut(path.as_str()) {
      Some(Node::Group(group)) if !group.collapsed => {
        group.collapsed = true;
        self.rebuild();
      }
      Some(Node::Group(_) | Node::Task(_)) | None => {
        if let Some(parent) = path.parent() {
          self.select(&parent);
        }
      }
    }
  }

  /// Opens, closes, or toggles the groups `target` matches, as a path or
  /// glob, or else the selected group or the one the selected task is
  /// in. Toggling several closes them all unless all are closed. A
  /// selection that a group closes over moves up to it.
  pub fn fold(&mut self, op: FoldOp, target: Option<&str>) {
    let paths: Vec<String> = match target {
      Some(pattern) => self
        .nodes
        .iter()
        .filter(|(path, node)| {
          matches!(node, Node::Group(_))
            && !path.is_empty()
            && glob_matches(pattern, path)
        })
        .map(|(path, _)| path.clone())
        .collect(),
      None => match self.selected() {
        Some((path, Node::Group(_))) => vec![path.to_string()],
        Some((path, Node::Task(_))) => path
          .parent()
          .filter(|parent| !parent.is_root())
          .map(|parent| parent.to_string())
          .into_iter()
          .collect(),
        None => Vec::new(),
      },
    };
    let groups = || {
      paths.iter().filter_map(|path| match self.nodes.get(path) {
        Some(Node::Group(group)) => Some(group),
        Some(Node::Task(_)) | None => None,
      })
    };
    let collapsed = match op {
      FoldOp::Open => false,
      FoldOp::Close => true,
      FoldOp::Toggle => !groups().all(|group| group.collapsed),
    };
    let selected = self.selected_path();
    for path in &paths {
      if let Some(Node::Group(group)) = self.nodes.get_mut(path) {
        group.collapsed = collapsed;
      }
    }
    self.rebuild();
    // Up to the nearest group still shown.
    if let Some(selected) = selected
      && self.row_of(&selected).is_none()
    {
      let shown = std::iter::successors(selected.parent(), TaskPath::parent)
        .take_while(|path| !path.is_root())
        .find(|path| self.row_of(path).is_some());
      if let Some(shown) = shown {
        self.select(&shown);
      }
    }
  }

  /// The paths listed beside `path`, under the same group, in order.
  pub fn siblings(&self, path: &TaskPath) -> Vec<TaskPath> {
    let parent = path.parent().unwrap_or_else(TaskPath::root);
    match self.nodes.get(parent.as_str()) {
      Some(Node::Group(group)) => group
        .children
        .iter()
        .filter_map(|child| TaskPath::new(child.as_str()).ok())
        .collect(),
      Some(Node::Task(_)) | None => Vec::new(),
    }
  }

  /// Selects the task at a path, opening the groups it is in.
  pub fn select_path(&mut self, path: &str) {
    self.reveal(path);
    if let Some(path) =
      self.paths.values().find(|p| p.as_str() == path).cloned()
    {
      self.select(&path);
    }
  }

  /// Opens the collapsed groups above `path`, so its row shows.
  fn reveal(&mut self, path: &str) {
    let mut opened = false;
    for above in above(path) {
      if let Some(Node::Group(group)) = self.nodes.get_mut(above)
        && group.collapsed
      {
        group.collapsed = false;
        opened = true;
      }
    }
    if opened {
      self.rebuild();
    }
  }

  /// Where the selected row was last drawn.
  pub fn selected_area(&self) -> Option<Rect> {
    self.list.row_area(self.list.selected()?)
  }

  /// The area the rows were last drawn in.
  pub fn area(&self) -> Rect {
    self.list.area()
  }

  /// Selects a row, leaving a group as it is.
  pub fn select_row(&mut self, index: usize) {
    if let Some(path) = self.rows.get(index).cloned() {
      self.select(&path);
    }
  }

  /// How many rows are listed now.
  pub fn row_count(&self) -> usize {
    self.rows.len()
  }

  /// Selects a row; a group also opens or closes.
  pub fn click(&mut self, index: usize) {
    let Some(path) = self.rows.get(index).cloned() else {
      return;
    };
    if let Some(Node::Group(group)) = self.nodes.get_mut(path.as_str()) {
      group.collapsed = !group.collapsed;
      self.rebuild();
    }
    self.select(&path);
  }

  pub fn scroll_by(&mut self, delta: isize) {
    self.list.scroll_by(delta);
  }

  pub fn rows(&mut self, area: Rect) -> impl Iterator<Item = ListRow> + use<> {
    self.list.rows(area)
  }

  pub fn row_at(&self, x: u16, y: u16) -> Option<ListRow> {
    self.list.row_at(x, y)
  }

  fn selected_path(&self) -> Option<Arc<TaskPath>> {
    self.rows.get(self.list.selected()?).cloned()
  }

  fn select(&mut self, path: &TaskPath) {
    if let Some(index) = self.row_of(path) {
      self.list.select(index);
    }
  }

  /// The row a path is listed at now, if its groups are open.
  pub fn row_of(&self, path: &TaskPath) -> Option<usize> {
    self.rows.iter().position(|row| **row == *path)
  }

  /// Rebuilds the rows and puts the cursor back on the selected row, or on
  /// the row that took its place.
  fn rebuild(&mut self) {
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

/// Whether `path` matches a glob as a target writes one: `*` is one
/// part, and `**` any number of them.
fn glob_matches(pattern: &str, path: &str) -> bool {
  fn matches(pattern: &[&str], path: &[&str]) -> bool {
    match (pattern.split_first(), path.split_first()) {
      (None, None) => true,
      (Some((&"**", rest)), _) => {
        matches(rest, path)
          || (!path.is_empty() && matches(pattern, &path[1..]))
      }
      (Some((part, rest)), Some((name, names))) => {
        (*part == "*" || part == name) && matches(rest, names)
      }
      (Some(_), None) | (None, Some(_)) => false,
    }
  }
  fn parts(text: &str) -> Vec<&str> {
    text.split('/').filter(|part| !part.is_empty()).collect()
  }
  matches(&parts(pattern), &parts(path))
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
  use crate::console::action::FoldOp;
  use crate::kernel::kernel_message::{Place, ScreenHandle};
  use crate::kernel::path_trie::PathTrie;
  use crate::kernel::task::ExitInfo;

  fn task(id: usize, path: &str) -> TaskView {
    TaskView {
      id: TaskId(id),
      label: None,
      path: Arc::new(TaskPath::new(path).unwrap()),
      kind: crate::kernel::task::TaskKind::Service,
      status: TaskState::Idle,
      vt: ScreenHandle::new(crate::term::Frame::empty(
        crate::term::Size::default(),
      )),
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
    let place = after.map_or(Place::Last, |after| {
      Place::After(TaskPath::new(after).unwrap())
    });
    let path = TaskPath::new(path).unwrap();
    trie.insert(&path, TaskId(id), &place).unwrap();
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
    // The selection stays on what was selected.
    assert_eq!(
      shown(&mut tree),
      [
        "first",
        "db*",
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
      trie.place(&TaskPath::new(path).unwrap(), &Place::Last);
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
  fn a_collapsed_group_moved_stays_collapsed() {
    let mut tree = tree(&["a", "tools/lint"]);
    tree.next();
    tree.collapse();
    assert_eq!(shown(&mut tree), ["a", "tools/*"]);
    tree.move_task(TaskId(2), TaskPath::new("tools/lint").unwrap(), None);
    assert_eq!(shown(&mut tree), ["tools/*", "a"]);
  }

  /// A task moved up onto the group that held only it is listed in the
  /// group's place.
  #[test]
  fn a_task_takes_the_name_of_the_group_it_empties() {
    let mut tree = tree(&["x", "a/b", "y"]);
    tree.next();
    tree.collapse();
    assert_eq!(shown(&mut tree), ["x", "a/*", "y"]);
    tree.move_task(TaskId(2), TaskPath::new("a").unwrap(), Some(TaskId(1)));
    assert_eq!(shown(&mut tree), ["x", "a*", "y"]);
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

  /// A folder is a group row of its own: listed where the kernel says,
  /// kept while empty, and starting collapsed if it says so.
  #[test]
  fn folders_are_group_rows() {
    let mut tree = tree(&["db"]);
    tree.add_folder(
      TaskId(10),
      TaskPath::new("apps").unwrap(),
      Some(TaskId(1)),
      true,
    );
    tree.add_folder(
      TaskId(11),
      TaskPath::new("tmp").unwrap(),
      Some(TaskId(10)),
      false,
    );
    assert_eq!(shown(&mut tree), ["db*", "apps/", "tmp/"]);
    tree.add(task(2, "apps/web"), Some(TaskId(10)));
    assert_eq!(shown(&mut tree), ["db*", "apps/", "tmp/"], "collapsed");
    tree.select_task(1);
    assert_eq!(shown(&mut tree), ["db", "apps/", "  apps/web*", "tmp/"]);

    // Emptied, it stays; taken away, its group goes once it is empty.
    tree.remove(TaskId(2));
    assert_eq!(shown(&mut tree), ["db", "apps/", "tmp/*"]);
    tree.remove(TaskId(10));
    tree.remove(TaskId(11));
    assert_eq!(shown(&mut tree), ["db*"]);
  }

  /// Groups listed again start as they were saved; the others as their
  /// folder says, or open.
  #[test]
  fn groups_start_as_saved() {
    let saved =
      BTreeMap::from([("web".to_string(), true), ("apps".to_string(), false)]);
    let mut tree = TaskTree::with_groups(saved);
    tree.add_folder(TaskId(10), TaskPath::new("apps").unwrap(), None, true);
    tree.add(task(1, "apps/api"), Some(TaskId(10)));
    tree.add(task(2, "web/ui"), Some(TaskId(1)));
    tree.add(task(3, "db/pg"), Some(TaskId(2)));
    assert_eq!(
      shown(&mut tree),
      ["apps/*", "  apps/api", "web/", "db/", "  db/pg"]
    );
    // Each folder, and the other groups closed.
    assert_eq!(
      tree.groups(),
      BTreeMap::from([("apps".to_string(), false), ("web".to_string(), true)])
    );
  }

  #[test]
  fn folds_the_groups_a_target_matches() {
    let mut tree = tree(&["a/x/1", "a/y", "b/z", "c"]);
    tree.fold(FoldOp::Close, Some("**"));
    assert_eq!(shown(&mut tree), ["a/*", "b/", "c"]);
    tree.fold(FoldOp::Open, Some("a"));
    assert_eq!(shown(&mut tree), ["a/*", "  a/x/", "  a/y", "b/", "c"]);
    tree.fold(FoldOp::Open, Some("**"));
    assert_eq!(
      shown(&mut tree),
      ["a/*", "  a/x/", "    a/x/1", "  a/y", "b/", "  b/z", "c"]
    );
    // Toggling several closes them unless all are closed.
    tree.fold(FoldOp::Close, Some("b"));
    tree.fold(FoldOp::Toggle, Some("*"));
    assert_eq!(shown(&mut tree), ["a/*", "b/", "c"]);
    tree.fold(FoldOp::Toggle, Some("*"));
    assert_eq!(
      shown(&mut tree),
      ["a/*", "  a/x/", "    a/x/1", "  a/y", "b/", "  b/z", "c"]
    );

    // The selected group, or the one the selected task is in; a
    // selection closed over moves up to it.
    tree.select_task(0);
    tree.fold(FoldOp::Close, None);
    assert_eq!(
      shown(&mut tree),
      ["a/", "  a/x/*", "  a/y", "b/", "  b/z", "c"]
    );
    tree.fold(FoldOp::Toggle, None);
    tree.select_task(0);
    tree.fold(FoldOp::Close, Some("a"));
    assert_eq!(shown(&mut tree), ["a/*", "b/", "  b/z", "c"]);
  }

  #[test]
  fn a_glob_matches_as_a_target_does() {
    for (pattern, path, matches) in [
      ("a", "a", true),
      ("a", "a/b", false),
      ("*", "a", true),
      ("*", "a/b", false),
      ("a/*", "a/b", true),
      ("**", "a/b/c", true),
      ("a/**", "a", true),
      ("a/**", "a/b/c", true),
      ("a/**", "b/a", false),
      ("**/c", "a/b/c", true),
      ("**/c", "c", true),
      ("**/c", "a/c/d", false),
    ] {
      assert_eq!(glob_matches(pattern, path), matches, "{pattern} {path}");
    }
  }

  /// A folder moved elsewhere keeps how its group was shown, and the
  /// selection stays on it.
  #[test]
  fn a_moved_folder_keeps_its_fold() {
    let mut tree = tree(&["db"]);
    tree.add_folder(
      TaskId(10),
      TaskPath::new("g/apps").unwrap(),
      Some(TaskId(1)),
      false,
    );
    tree.add(task(2, "g/apps/web"), Some(TaskId(10)));
    tree.select_task(1);
    tree.fold(FoldOp::Close, Some("g/apps"));
    assert_eq!(shown(&mut tree), ["db", "g/", "  g/apps/*"]);
    tree.move_task(TaskId(10), TaskPath::new("h/apps").unwrap(), None);
    tree.move_task(
      TaskId(2),
      TaskPath::new("h/apps/web").unwrap(),
      Some(TaskId(10)),
    );
    assert_eq!(shown(&mut tree), ["h/", "  h/apps/*", "db"]);
  }

  /// A plain group that a folder's move empties for a moment comes back
  /// as it was shown.
  #[test]
  fn a_folder_move_keeps_the_fold_above_it() {
    let mut tree = tree(&["db"]);
    let f = TaskPath::new("g/f").unwrap();
    tree.add_folder(TaskId(10), f.clone(), Some(TaskId(1)), false);
    assert_eq!(shown(&mut tree), ["db*", "g/", "  g/f/"]);
    tree.fold(FoldOp::Close, Some("g"));
    assert_eq!(shown(&mut tree), ["db*", "g/"]);
    tree.move_task(TaskId(10), f, Some(TaskId(1)));
    assert_eq!(shown(&mut tree), ["db*", "g/"]);
  }
}
