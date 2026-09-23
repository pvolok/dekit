use super::Cell;

#[derive(Clone, Debug)]
pub struct Row {
  pub cells: Vec<super::cell::Cell>,
  size: u16,
  wrapped: bool,
}

impl Row {
  pub fn new(cols: u16) -> Self {
    Self {
      cells: vec![super::cell::Cell::default(); usize::from(cols)],
      size: 0,
      wrapped: false,
    }
  }

  pub fn new_with_attrs(cols: u16, attrs: super::attrs::Attrs) -> Self {
    let mut cell = super::cell::Cell::default();
    cell.set_attrs(attrs);
    Self {
      cells: vec![cell; usize::from(cols)],
      size: 0,
      wrapped: false,
    }
  }

  pub fn cols(&self) -> u16 {
    self
      .cells
      .len()
      .try_into()
      // we limit the number of cols to a u16 (see Size)
      .unwrap()
  }

  pub fn clear(&mut self, attrs: super::attrs::Attrs) {
    for cell in &mut self.cells {
      cell.clear(attrs);
    }
    self.size = 0;
    self.wrapped = false;
  }

  fn cells(&self) -> impl Iterator<Item = &super::cell::Cell> {
    self.cells.iter()
  }

  pub fn get(&self, col: u16) -> Option<&super::cell::Cell> {
    self.cells.get(usize::from(col))
  }

  pub fn get_mut(&mut self, col: u16) -> Option<&mut super::cell::Cell> {
    self.size = self.size.max(col + 1);
    self.cells.get_mut(usize::from(col))
  }

  pub fn insert(&mut self, i: u16, cell: super::cell::Cell) {
    self.cells.insert(usize::from(i), cell);
    self.wrapped = false;
  }

  pub fn remove(&mut self, i: u16) {
    self.clear_wide(i);
    self.cells.remove(usize::from(i));
    self.wrapped = false;
  }

  pub fn erase(&mut self, i: u16, attrs: super::attrs::Attrs) {
    let wide = self.cells[usize::from(i)].is_wide();
    self.clear_wide(i);
    self.cells[usize::from(i)].clear(attrs);
    if i == self.cols() - if wide { 2 } else { 1 } {
      self.wrapped = false;
    }
  }

  pub fn truncate(&mut self, len: u16) {
    self.cells.truncate(usize::from(len));
    self.wrapped = false;
    let last_cell = &mut self.cells[usize::from(len) - 1];
    if last_cell.is_wide() {
      last_cell.clear(*last_cell.attrs());
    }
  }

  pub fn resize(&mut self, len: u16, cell: super::cell::Cell) {
    self.cells.resize(usize::from(len), cell);
    self.wrapped = false;
  }

  pub fn wrap(&mut self, wrap: bool) {
    self.wrapped = wrap;
  }

  pub fn wrapped(&self) -> bool {
    self.wrapped
  }

  pub fn clear_wide(&mut self, col: u16) {
    let cell = &self.cells[usize::from(col)];
    let other = if cell.is_wide() {
      self.cells.get_mut(usize::from(col + 1))
    } else if self.is_wide_continuation(col) {
      self.cells.get_mut(usize::from(col - 1))
    } else {
      return;
    };
    if let Some(other) = other {
      other.clear(*other.attrs());
    }
  }

  pub fn take_cells(&self, vec: &mut Vec<Cell>) {
    vec.extend(self.cells.iter().take(self.size.into()).cloned());
  }

  pub fn write_contents(
    &self,
    contents: &mut String,
    start: u16,
    width: u16,
    wrapping: bool,
  ) {
    let mut prev_was_wide = false;

    let mut prev_col = start;
    for (col, cell) in self
      .cells()
      .enumerate()
      .skip(usize::from(start))
      .take(usize::from(width))
    {
      if prev_was_wide {
        prev_was_wide = false;
        continue;
      }
      prev_was_wide = cell.is_wide();

      // we limit the number of cols to a u16 (see Size)
      let col: u16 = col.try_into().unwrap();
      if cell.has_contents() {
        for _ in 0..(col - prev_col) {
          contents.push(' ');
        }
        prev_col += col - prev_col;

        contents.push_str(cell.contents());
        prev_col += if cell.is_wide() { 2 } else { 1 };
      }
    }
    if prev_col == start && wrapping {
      contents.push('\n');
    }
  }

  pub(crate) fn is_wide_continuation(&self, col: u16) -> bool {
    if col == 0 {
      return false;
    }

    self
      .cells
      .get(Into::<usize>::into(col) - 1)
      .is_some_and(super::cell::Cell::is_wide)
  }
}

impl Row {
  pub fn snapshot(
    &self,
    table: &mut super::snapshot::AttrsTable,
  ) -> crate::upgrade::snapshot::Row {
    let mut text = String::with_capacity(self.cells.len());
    let mut cells: Vec<(u32, u16)> = Vec::new();
    let mut attrs: Vec<(usize, u16)> = Vec::new();
    for cell in &self.cells {
      text.push_str(cell.contents());
      push_run(&mut cells, cell.contents().chars().count() as u32);
      push_run(&mut attrs, table.index(cell.attrs()));
    }
    crate::upgrade::snapshot::Row {
      text,
      cells,
      attrs,
      wrapped: self.wrapped,
      size: self.size,
    }
  }

  pub fn from_snapshot(
    row: &crate::upgrade::snapshot::Row,
    table: &[super::attrs::Attrs],
  ) -> anyhow::Result<Self> {
    let mut cells = Vec::new();
    let mut rest = row.text.as_str();
    for &(chars, len) in &row.cells {
      for _ in 0..len {
        let (text, tail) =
          split_chars(rest, chars as usize).ok_or_else(|| {
            anyhow::anyhow!("row text is shorter than its cells")
          })?;
        rest = tail;
        let mut cell = super::cell::Cell::default();
        cell.set_str(text);
        cells.push(cell);
      }
    }
    if !rest.is_empty() {
      anyhow::bail!("row text is longer than its cells");
    }
    let mut runs = row
      .attrs
      .iter()
      .flat_map(|(index, len)| std::iter::repeat_n(*index, usize::from(*len)));
    for cell in &mut cells {
      let index = runs
        .next()
        .ok_or_else(|| anyhow::anyhow!("row attrs cover fewer cells"))?;
      let attrs = table
        .get(index)
        .ok_or_else(|| anyhow::anyhow!("row attrs index out of range"))?;
      cell.set_attrs(*attrs);
    }
    if runs.next().is_some() {
      anyhow::bail!("row attrs cover more cells");
    }
    if cells.is_empty() {
      anyhow::bail!("row has no cells");
    }
    Ok(Row {
      cells,
      size: row.size,
      wrapped: row.wrapped,
    })
  }
}

fn push_run<T: PartialEq>(runs: &mut Vec<(T, u16)>, value: T) {
  match runs.last_mut() {
    Some((last, len)) if *last == value => *len += 1,
    _ => runs.push((value, 1)),
  }
}

/// The first `n` chars of `s` and the rest, or None if `s` is shorter.
fn split_chars(s: &str, n: usize) -> Option<(&str, &str)> {
  let mut chars = s.chars();
  let mut end = 0;
  for _ in 0..n {
    end += chars.next()?.len_utf8();
  }
  Some(s.split_at(end))
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::upgrade::snapshot as snap;

  fn row(text: &str, cells: Vec<(u32, u16)>) -> snap::Row {
    snap::Row {
      text: text.to_string(),
      cells,
      attrs: vec![(0, 3)],
      wrapped: false,
      size: 3,
    }
  }

  #[test]
  fn text_and_cells_must_agree() {
    let table = [super::super::attrs::Attrs::default()];
    assert!(Row::from_snapshot(&row("abc", vec![(1, 3)]), &table).is_ok());
    assert!(Row::from_snapshot(&row("ab", vec![(1, 3)]), &table).is_err());
    assert!(Row::from_snapshot(&row("abcd", vec![(1, 3)]), &table).is_err());
    // Attrs must cover exactly the cells.
    let short = snap::Row {
      attrs: vec![(0, 2)],
      ..row("abc", vec![(1, 3)])
    };
    assert!(Row::from_snapshot(&short, &table).is_err());
  }
}
