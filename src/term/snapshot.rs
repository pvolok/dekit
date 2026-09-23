//! Snapshot helpers shared by the screen, grid, and row conversions.

use std::collections::HashMap;

use crate::upgrade::snapshot as snap;

use super::{attrs::Attrs, color::Color};

/// Deduplicates cell attributes into the per-screen table rows index.
#[derive(Default)]
pub struct AttrsTable {
  entries: Vec<snap::Attrs>,
  index: HashMap<snap::Attrs, usize>,
  /// Neighbouring cells mostly share attributes: the last answer.
  last: Option<(Attrs, usize)>,
}

impl AttrsTable {
  pub fn index(&mut self, attrs: &Attrs) -> usize {
    if let Some((last, index)) = &self.last
      && last == attrs
    {
      return *index;
    }
    let key = attrs_snapshot(attrs);
    let index = match self.index.get(&key) {
      Some(index) => *index,
      None => {
        self.entries.push(key);
        self.index.insert(key, self.entries.len() - 1);
        self.entries.len() - 1
      }
    };
    self.last = Some((*attrs, index));
    index
  }

  pub fn into_entries(self) -> Vec<snap::Attrs> {
    self.entries
  }
}

pub fn attrs_snapshot(attrs: &Attrs) -> snap::Attrs {
  snap::Attrs {
    fg: color_snapshot(attrs.fgcolor),
    bg: color_snapshot(attrs.bgcolor),
    mode: attrs.mode,
  }
}

pub fn attrs_from_snapshot(attrs: &snap::Attrs) -> Attrs {
  Attrs {
    fgcolor: color_from_snapshot(attrs.fg),
    bgcolor: color_from_snapshot(attrs.bg),
    mode: attrs.mode,
  }
}

fn color_snapshot(color: Color) -> snap::Color {
  match color {
    Color::Default => snap::Color::Default,
    Color::Idx(i) => snap::Color::Idx(i),
    Color::Rgb(r, g, b) => snap::Color::Rgb(r, g, b),
  }
}

fn color_from_snapshot(color: snap::Color) -> Color {
  match color {
    snap::Color::Default => Color::Default,
    snap::Color::Idx(i) => Color::Idx(i),
    snap::Color::Rgb(r, g, b) => Color::Rgb(r, g, b),
  }
}
