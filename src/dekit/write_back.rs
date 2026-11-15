//! The `write_back` method: what changed here, written back to the
//! config by the runner, or by an agent the client runs with a prompt
//! the runner writes, whose edits the runner then checks.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use serde_json::Value;

use crate::dekit::server::ServerCtx;
use crate::kernel::task::TaskId;
use crate::protocol::{
  RpcError, codes,
  rpc::{RpcWriteFailure, RpcWriteField, RpcWriteItem, WriteBackResult},
};
use crate::task::config_tasks::project_dir;
use crate::write_back::{self, WriteKind, WritePlan};

/// How many prompted plans are kept for a `check`: an agent's run is
/// one at a time, in practice.
const KEPT: usize = 8;

/// The plans prompts were written for, newest last, until checked.
#[derive(Default)]
pub struct Prompts {
  plans: Mutex<VecDeque<(u64, Arc<WritePlan>)>>,
  next: std::sync::atomic::AtomicU64,
}

impl Prompts {
  fn keep(&self, plan: Arc<WritePlan>) -> u64 {
    let id = 1 + self.next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    if let Ok(mut plans) = self.plans.lock() {
      if plans.len() == KEPT {
        plans.pop_front();
      }
      plans.push_back((id, plan));
    }
    id
  }

  fn take(&self, id: u64) -> Option<Arc<WritePlan>> {
    let mut plans = self.plans.lock().ok()?;
    let at = plans.iter().position(|(kept, _)| *kept == id)?;
    plans.remove(at).map(|(_, plan)| plan)
  }
}

/// `write_back`, with the request's params.
pub async fn write_back(
  ctx: &ServerCtx,
  include: Vec<TaskId>,
  paths: Option<Vec<String>>,
  prompt: bool,
  plan: Option<u64>,
) -> Result<Value, RpcError> {
  let (pc, project) = (&ctx.pc, &ctx.project);
  let mut result = WriteBackResult::default();
  if let Some(id) = plan {
    if paths.is_some() || prompt || !include.is_empty() {
      return Err(RpcError::new(
        codes::INVALID_PARAMS,
        "`plan` checks a prompted plan: it takes nothing else",
      ));
    }
    let Some(plan) = ctx.prompts.take(id) else {
      return Err(RpcError::new(
        codes::NO_MATCH,
        "no such prompt: the runner forgot it, or it was checked",
      ));
    };
    let mut failed = Vec::new();
    write_back::written(pc, project, &plan, &mut failed).await;
    result.failed = failures(failed);
    return to_value(result);
  }
  let (config, live, base) = crate::reload::load(pc, project)
    .await
    .map_err(|err| RpcError::new(codes::BAD_CONFIG, format!("{err:#}")))?;
  let root = project_dir(&config);
  let mut plan = write_back::plan(&config, &root, &base, &live, &include);
  if let Some(chosen) = &paths {
    for item in &mut plan.items {
      item.apply = chosen.contains(&item.path);
    }
  }
  if prompt {
    result.prompt = Some(write_back::prompt(&plan));
    result.plan = Some(ctx.prompts.keep(Arc::new(plan)));
  } else if paths.is_some() {
    let plan = Arc::new(plan);
    let to_write = plan.clone();
    let mut failed =
      tokio::task::spawn_blocking(move || write_back::write(&to_write))
        .await
        .unwrap_or_default();
    write_back::written(pc, project, &plan, &mut failed).await;
    result.failed = failures(failed);
  } else {
    result.items = items(&plan);
  }
  to_value(result)
}

fn to_value(result: WriteBackResult) -> Result<Value, RpcError> {
  serde_json::to_value(result).map_err(RpcError::internal)
}

fn failures(failed: Vec<(String, String)>) -> Vec<RpcWriteFailure> {
  failed
    .into_iter()
    .map(|(path, why)| RpcWriteFailure { path, why })
    .collect()
}

/// The plan's items as the wire has them.
pub fn items(plan: &WritePlan) -> Vec<RpcWriteItem> {
  let json = |value: &Option<serde_yaml::Value>| {
    value.as_ref().and_then(|v| serde_json::to_value(v).ok())
  };
  plan
    .items
    .iter()
    .map(|item| RpcWriteItem {
      path: item.path.clone(),
      source: item.source.clone(),
      kind: match item.kind {
        WriteKind::Changed => "changed",
        WriteKind::Added => "added",
        WriteKind::Removed => "removed",
      }
      .to_string(),
      fields: item
        .fields
        .iter()
        .map(|field| RpcWriteField {
          key: field.key.clone(),
          was: json(&field.was),
          now: json(&field.now),
        })
        .collect(),
      apply: item.apply,
      note: item.note.clone(),
      task: item.here(),
    })
    .collect()
}
