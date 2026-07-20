use anyhow::Result;

use crate::cfg::{CfgCx, CfgObj};

const DEFAULT_SIDEBAR_WIDTH: usize = 30;
const DEFAULT_SIDEBAR_TITLE: &str = "Tasks";

pub struct TuiConfig {
  pub sidebar: SidebarConfig,
  pub tips: TipsConfig,
  pub zoom_tip: bool,
}

pub struct SidebarConfig {
  pub title: String,
  pub width: usize,
}

pub struct TipsConfig {
  pub show: bool,
}

impl TuiConfig {
  pub(crate) fn builtin() -> Self {
    TuiConfig {
      sidebar: SidebarConfig {
        title: DEFAULT_SIDEBAR_TITLE.to_string(),
        width: DEFAULT_SIDEBAR_WIDTH,
      },
      tips: TipsConfig { show: true },
      zoom_tip: true,
    }
  }

  pub(crate) fn merge(&mut self, obj: &CfgObj<'_>, cx: &CfgCx) -> Result<()> {
    let tui_obj = match obj.get("tui") {
      Some(node) => node.as_obj()?,
      None => return Ok(()),
    };

    if let Some(pl) = tui_obj.get("sidebar") {
      let pl = pl.as_obj()?;
      self.sidebar.title =
        pl.default("title", self.sidebar.title.clone(), cx)?;
      self.sidebar.width = pl.default("width", self.sidebar.width, cx)?;
    }

    if let Some(tips) = tui_obj.get("tips") {
      self.tips.show = tips.as_obj()?.default("show", self.tips.show, cx)?;
    }

    self.zoom_tip = tui_obj.default("zoom_tip", self.zoom_tip, cx)?;

    Ok(())
  }
}
