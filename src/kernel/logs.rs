//! What tasks logged: the lines their screens log (`Grid` "Log lines"),
//! and when they started and exited. Kept per task, apart from the
//! scrollback, for `dekit logs`.

use std::{
  collections::{HashMap, VecDeque},
  sync::{Mutex, MutexGuard, PoisonError},
  time::SystemTime,
};

use tokio::sync::watch;

use super::task::{ExitInfo, TaskId};

/// Records a task keeps; older ones are dropped.
pub const RING_LEN: usize = 10_000;

#[derive(Clone, Debug, PartialEq)]
pub struct Record {
  /// The order across all tasks of the runner, from 1.
  pub seq: u64,
  pub time: SystemTime,
  pub task: TaskId,
  pub kind: RecordKind,
}

#[derive(Clone, Debug, PartialEq)]
pub enum RecordKind {
  Line(String),
  Started,
  Exited(ExitInfo),
}

/// What a follower reads.
#[derive(Clone, Debug, PartialEq)]
pub enum Item {
  Record(Record),
  /// The task's ring dropped this many records before they were read.
  Dropped {
    task: TaskId,
    count: u64,
  },
}

/// Where a follower is: every record up to `seq` was there to read, and
/// per task, the number of the next record to read.
#[derive(Clone, Debug, Default)]
pub struct Cursor {
  seq: u64,
  next: HashMap<TaskId, u64>,
}

/// The end of the tasks' logs.
pub struct Tail {
  /// Merged by seq.
  pub records: Vec<Record>,
  /// The lines under way, in the order of the tasks asked for.
  pub open: Vec<(TaskId, String)>,
  /// Where `read` goes on from.
  pub cursor: Cursor,
}

/// One per runner, shared by its tasks. Followers wake on `subscribe`.
pub struct LogStore {
  inner: Mutex<Inner>,
  last: watch::Sender<u64>,
}

#[derive(Default)]
struct Inner {
  /// The last seq given out.
  seq: u64,
  rings: HashMap<TaskId, Ring>,
}

#[derive(Default)]
struct Ring {
  records: VecDeque<Record>,
  /// Records dropped from the front: the first one kept is number
  /// `evicted`.
  evicted: u64,
  /// The line under way, and when it last changed.
  open: Option<(SystemTime, String)>,
}

impl Ring {
  fn end(&self) -> u64 {
    self.evicted + self.records.len() as u64
  }
}

impl Default for LogStore {
  fn default() -> Self {
    Self::new()
  }
}

impl LogStore {
  pub fn new() -> Self {
    LogStore {
      inner: Mutex::default(),
      last: watch::Sender::new(0),
    }
  }

  fn lock(&self) -> MutexGuard<'_, Inner> {
    self.inner.lock().unwrap_or_else(PoisonError::into_inner)
  }

  /// Gives the task a log. Records for a task without one are ignored,
  /// so a task removed while it still reports leaves nothing behind.
  pub fn add_task(&self, task: TaskId) {
    self.lock().rings.entry(task).or_default();
  }

  pub fn remove_task(&self, task: TaskId) {
    self.lock().rings.remove(&task);
  }

  /// Logs `records` for the task, and sets the line under way.
  pub fn push(
    &self,
    task: TaskId,
    records: impl IntoIterator<Item = RecordKind>,
    open: Option<String>,
  ) {
    let mut inner = self.lock();
    let Inner { seq, rings } = &mut *inner;
    let Some(ring) = rings.get_mut(&task) else {
      return;
    };
    let time = SystemTime::now();
    if ring.open.as_ref().map(|(_, text)| text) != open.as_ref() {
      ring.open = open.map(|text| (time, text));
    }
    let start = *seq;
    for kind in records {
      *seq += 1;
      if ring.records.len() == RING_LEN {
        ring.records.pop_front();
        ring.evicted += 1;
      }
      ring.records.push_back(Record {
        seq: *seq,
        time,
        task,
        kind,
      });
    }
    if *seq != start {
      self.last.send_replace(*seq);
    }
  }

  /// Changes whenever a record is logged.
  pub fn subscribe(&self) -> watch::Receiver<u64> {
    self.last.subscribe()
  }

  /// The tasks' last `lines` records and their lines under way, from
  /// `since` on, if given.
  pub fn tail(
    &self,
    tasks: &[TaskId],
    lines: usize,
    since: Option<SystemTime>,
  ) -> Tail {
    let inner = self.lock();
    let mut records = Vec::new();
    let mut open = Vec::new();
    let mut next = HashMap::new();
    for &task in tasks {
      let Some(ring) = inner.rings.get(&task) else {
        continue;
      };
      let recent = |time: SystemTime| since.is_none_or(|since| time >= since);
      let kept = ring.records.iter().rev().filter(|r| recent(r.time));
      records.extend(kept.take(lines).cloned());
      if let Some((time, line)) = &ring.open
        && recent(*time)
      {
        open.push((task, line.clone()));
      }
      next.insert(task, ring.end());
    }
    records.sort_unstable_by_key(|r| r.seq);
    records.drain(..records.len().saturating_sub(lines));
    let cursor = Cursor {
      seq: inner.seq,
      next,
    };
    Tail {
      records,
      open,
      cursor,
    }
  }

  /// The tasks' records after `cursor`, merged by seq, which moves past
  /// them. A task new to the cursor starts after its seq; one the cursor
  /// knows reports what its ring dropped first.
  pub fn read(&self, tasks: &[TaskId], cursor: &mut Cursor) -> Vec<Item> {
    let inner = self.lock();
    let mut dropped = Vec::new();
    let mut records = Vec::new();
    let mut next = HashMap::new();
    for &task in tasks {
      let Some(ring) = inner.rings.get(&task) else {
        continue;
      };
      let from = match cursor.next.get(&task) {
        Some(&from) if from < ring.evicted => {
          let count = ring.evicted - from;
          dropped.push(Item::Dropped { task, count });
          0
        }
        Some(&from) => (from - ring.evicted) as usize,
        None => ring.records.partition_point(|r| r.seq <= cursor.seq),
      };
      records.extend(ring.records.range(from..).cloned());
      next.insert(task, ring.end());
    }
    records.sort_unstable_by_key(|r| r.seq);
    cursor.seq = inner.seq;
    cursor.next = next;
    dropped.extend(records.into_iter().map(Item::Record));
    dropped
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  const A: TaskId = TaskId(1);
  const B: TaskId = TaskId(2);

  fn store() -> LogStore {
    let store = LogStore::new();
    store.add_task(A);
    store.add_task(B);
    store
  }

  fn line(text: &str) -> RecordKind {
    RecordKind::Line(text.to_string())
  }

  fn lines(store: &LogStore, task: TaskId, texts: &[&str]) {
    store.push(task, texts.iter().map(|t| line(t)), None);
  }

  fn texts(records: &[Record]) -> Vec<(TaskId, String)> {
    records
      .iter()
      .map(|r| match &r.kind {
        RecordKind::Line(text) => (r.task, text.clone()),
        RecordKind::Started => (r.task, "started".into()),
        RecordKind::Exited(info) => (r.task, format!("exited {:?}", info.code)),
      })
      .collect()
  }

  fn read_texts(items: &[Item]) -> Vec<String> {
    items
      .iter()
      .map(|item| match item {
        Item::Record(Record {
          kind: RecordKind::Line(text),
          ..
        }) => text.clone(),
        item => format!("{item:?}"),
      })
      .collect()
  }

  #[test]
  fn records_merge_in_the_order_they_were_logged() {
    let store = store();
    lines(&store, A, &["a1"]);
    store.push(B, [RecordKind::Started], None);
    lines(&store, B, &["b1", "b2"]);
    lines(&store, A, &["a2"]);
    store.push(A, [RecordKind::Exited(ExitInfo::code(0))], None);

    let tail = store.tail(&[A, B], usize::MAX, None);
    let seqs: Vec<u64> = tail.records.iter().map(|r| r.seq).collect();
    assert_eq!(seqs, [1, 2, 3, 4, 5, 6]);
    assert_eq!(
      texts(&tail.records),
      [
        (A, "a1".into()),
        (B, "started".into()),
        (B, "b1".into()),
        (B, "b2".into()),
        (A, "a2".into()),
        (A, "exited Some(0)".into()),
      ]
    );
    assert_eq!(texts(&store.tail(&[B], usize::MAX, None).records).len(), 3);
  }

  #[test]
  fn tail_keeps_the_last_lines_across_tasks() {
    let store = store();
    lines(&store, A, &["a1", "a2"]);
    lines(&store, B, &["b1"]);
    lines(&store, A, &["a3"]);
    let tail = store.tail(&[A, B], 2, None);
    assert_eq!(texts(&tail.records), [(B, "b1".into()), (A, "a3".into())]);
    assert!(store.tail(&[A, B], 0, None).records.is_empty());
  }

  #[test]
  fn tail_since_skips_older_records() {
    let store = store();
    lines(&store, A, &["old"]);
    let since = SystemTime::now();
    std::thread::sleep(std::time::Duration::from_millis(2));
    lines(&store, A, &["new"]);
    let tail = store.tail(&[A], usize::MAX, Some(since));
    assert_eq!(texts(&tail.records), [(A, "new".into())]);
  }

  #[test]
  fn tail_since_skips_an_open_line_unchanged_since() {
    let store = store();
    store.push(A, [], Some("old".into()));
    store.push(B, [], Some("b 1".into()));
    let since = SystemTime::now();
    std::thread::sleep(std::time::Duration::from_millis(2));
    // The same text again is no change.
    store.push(A, [], Some("old".into()));
    store.push(B, [], Some("b 2".into()));
    let tail = store.tail(&[A, B], usize::MAX, Some(since));
    assert_eq!(tail.open, [(B, "b 2".into())]);
  }

  #[test]
  fn the_open_line_is_replaced_and_cleared() {
    let store = store();
    store.push(A, [], Some("10%".into()));
    store.push(A, [], Some("50%".into()));
    store.push(B, [], Some("b".into()));
    let tail = store.tail(&[A, B], usize::MAX, None);
    assert!(tail.records.is_empty());
    assert_eq!(tail.open, [(A, "50%".into()), (B, "b".into())]);

    store.push(A, [line("100%")], None);
    let tail = store.tail(&[A, B], usize::MAX, None);
    assert_eq!(texts(&tail.records), [(A, "100%".into())]);
    assert_eq!(tail.open, [(B, "b".into())]);
  }

  #[test]
  fn a_full_ring_drops_its_oldest_records() {
    let store = store();
    let all: Vec<String> = (0..RING_LEN + 5).map(|i| i.to_string()).collect();
    store.push(A, all.iter().map(|t| line(t)), None);
    let tail = store.tail(&[A], usize::MAX, None);
    assert_eq!(tail.records.len(), RING_LEN);
    assert_eq!(texts(&tail.records)[0].1, "5");
    assert_eq!(tail.records[0].seq, 6);
  }

  #[test]
  fn read_goes_on_from_the_tail() {
    let store = store();
    lines(&store, A, &["a1"]);
    let mut cursor = store.tail(&[A, B], usize::MAX, None).cursor;
    assert!(store.read(&[A, B], &mut cursor).is_empty());
    lines(&store, B, &["b1"]);
    lines(&store, A, &["a2"]);
    assert_eq!(read_texts(&store.read(&[A, B], &mut cursor)), ["b1", "a2"]);
    assert!(store.read(&[A, B], &mut cursor).is_empty());
  }

  #[test]
  fn a_task_new_to_the_cursor_starts_after_it() {
    let store = store();
    lines(&store, B, &["b-before"]);
    let mut cursor = store.tail(&[A], usize::MAX, None).cursor;
    lines(&store, A, &["a1"]);
    assert_eq!(read_texts(&store.read(&[A], &mut cursor)), ["a1"]);
    // B joins the set: only what it logged after the cursor shows.
    lines(&store, B, &["b1"]);
    assert_eq!(read_texts(&store.read(&[A, B], &mut cursor)), ["b1"]);
  }

  #[test]
  fn a_follower_left_behind_is_told_how_much_it_missed() {
    let store = store();
    let mut cursor = store.tail(&[A], usize::MAX, None).cursor;
    lines(&store, A, &["first"]);
    let rest: Vec<String> = (0..RING_LEN).map(|i| i.to_string()).collect();
    store.push(A, rest.iter().map(|t| line(t)), None);
    let items = store.read(&[A], &mut cursor);
    assert_eq!(items[0], Item::Dropped { task: A, count: 1 });
    assert_eq!(items.len(), RING_LEN + 1);
    assert_eq!(read_texts(&items[1..2]), ["0"]);
  }

  #[test]
  fn records_need_a_task_with_a_log() {
    let store = store();
    lines(&store, A, &["a1"]);
    store.remove_task(A);
    lines(&store, A, &["late"]);
    store.push(TaskId(9), [line("unknown")], Some("open".into()));
    let tail = store.tail(&[A, TaskId(9)], usize::MAX, None);
    assert!(tail.records.is_empty());
    assert!(tail.open.is_empty());
  }

  #[test]
  fn followers_wake_on_new_records_only() {
    let store = store();
    let mut last = store.subscribe();
    store.push(A, [], Some("open".into()));
    assert!(!last.has_changed().unwrap());
    lines(&store, A, &["a1"]);
    assert!(last.has_changed().unwrap());
    assert_eq!(*last.borrow_and_update(), 1);
  }
}
