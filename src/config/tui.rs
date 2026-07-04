use anyhow::Result;

use crate::cfg::{CfgCx, CfgObj};

const DEFAULT_PROC_LIST_WIDTH: usize = 30;
const DEFAULT_PROC_LIST_TITLE: &str = "Processes";

pub struct TuiConfig {
  pub procs: ProcListConfig,
  pub tips: TipsConfig,
  pub hide_zen_message: bool,
  pub hide_proc_frame: bool,
}

pub struct ProcListConfig {
  pub title: String,
  pub width: usize,
}

pub struct TipsConfig {
  pub show: bool,
}

impl TuiConfig {
  pub(crate) fn builtin() -> Self {
    TuiConfig {
      procs: ProcListConfig {
        title: DEFAULT_PROC_LIST_TITLE.to_string(),
        width: DEFAULT_PROC_LIST_WIDTH,
      },
      tips: TipsConfig { show: true },
      hide_zen_message: false,
      hide_proc_frame: false,
    }
  }

  pub(crate) fn merge(&mut self, obj: &CfgObj<'_>, cx: &CfgCx) -> Result<()> {
    let tui_obj = match obj.get("tui") {
      Some(node) => node.as_obj()?,
      None => return Ok(()),
    };

    if let Some(pl) = tui_obj.get("procs") {
      let pl = pl.as_obj()?;
      self.procs.title = pl.default("title", self.procs.title.clone(), cx)?;
      self.procs.width = pl.default("width", self.procs.width, cx)?;
    }

    if let Some(tips) = tui_obj.get("tips") {
      self.tips.show = tips.as_obj()?.default("show", self.tips.show, cx)?;
    }

    self.hide_zen_message =
      tui_obj.default("hide_zen_message", self.hide_zen_message, cx)?;
    self.hide_proc_frame =
      tui_obj.default("hide_proc_frame", self.hide_proc_frame, cx)?;

    Ok(())
  }
}
