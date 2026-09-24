//! Blocks generated from the clap tree, so a command page never drifts
//! from the binary it ships in.

use clap::{Arg, ArgAction, Command};

use super::ir::{Block, CommandRecord, FieldKind, FieldRecord};

/// The words after `dekit` in a `cli:` binding; None unless it starts
/// with `dekit`.
pub fn words(cli: &str) -> Option<Vec<&str>> {
  let mut parts = cli.split_whitespace();
  if parts.next()? != "dekit" {
    return None;
  }
  Some(parts.collect())
}

pub fn find<'a>(cmd: &'a Command, words: &[&str]) -> Option<&'a Command> {
  match words.split_first() {
    None => Some(cmd),
    Some((first, rest)) => find(cmd.find_subcommand(first)?, rest),
  }
}

fn find_mut<'a>(
  cmd: &'a mut Command,
  words: &[&str],
) -> Option<&'a mut Command> {
  match words.split_first() {
    None => Some(cmd),
    Some((first, rest)) => find_mut(cmd.find_subcommand_mut(first)?, rest),
  }
}

/// A built copy of the tree: usage lines and the auto-added flags exist
/// only after clap's build step.
pub fn prepare(root: &Command) -> Command {
  let mut built = root.clone();
  built.build();
  built
}

pub fn about(cmd: &Command) -> String {
  cmd
    .get_about()
    .map(|about| about.to_string())
    .unwrap_or_default()
}

/// The `:::usage` expansion for `cli` (`dekit runner stop`): the usage
/// line, the command's own arguments and flags, and its subcommands.
pub fn usage(built: &mut Command, cli: &str, topic: &str) -> Vec<Block> {
  let words = words(cli).unwrap_or_default();
  let Some(cmd) = find_mut(built, &words) else {
    return Vec::new();
  };
  let usage_line = cmd.render_usage().to_string();
  let usage_line = usage_line
    .strip_prefix("Usage:")
    .unwrap_or(&usage_line)
    .trim()
    .to_string();

  let is_root = words.is_empty();
  let mut positionals = Vec::new();
  let mut options = Vec::new();
  for arg in cmd.get_arguments() {
    let id = arg.get_id().as_str();
    if id == "help" || id == "version" || arg.is_hide_set() {
      continue;
    }
    if !is_root && arg.is_global_set() {
      continue;
    }
    let record = FieldRecord {
      kind: FieldKind::CliFlag,
      key: arg_key(arg),
      desc: arg
        .get_help()
        .map(|help| help.to_string())
        .unwrap_or_default(),
      topic: topic.to_string(),
      ty: None,
      default: None,
      required: None,
      signature: None,
      tag: None,
    };
    if arg.is_positional() {
      positionals.push(record);
    } else {
      options.push(record);
    }
  }
  let subcommands: Vec<CommandRecord> = cmd
    .get_subcommands()
    .filter(|sub| !sub.is_hide_set())
    .map(|sub| CommandRecord {
      cmd: format!("{cli} {}", sub.get_name()),
      desc: about(sub),
      topic: topic.to_string(),
      tag: None,
    })
    .collect();

  let mut blocks = vec![Block::Code {
    lang: None,
    text: usage_line,
  }];
  let items: Vec<FieldRecord> =
    positionals.into_iter().chain(options).collect();
  if !items.is_empty() {
    blocks.push(Block::Fields {
      kind: FieldKind::CliFlag,
      items,
    });
  }
  if !subcommands.is_empty() {
    blocks.push(Block::Commands { items: subcommands });
  }
  blocks
}

fn arg_key(arg: &Arg) -> String {
  let value_name = arg
    .get_value_names()
    .and_then(|names| names.first())
    .map(|name| name.to_string())
    .unwrap_or_else(|| arg.get_id().to_string());
  if arg.is_positional() {
    let multiple = matches!(arg.get_action(), ArgAction::Append)
      || arg
        .get_num_args()
        .is_some_and(|range| range.max_values() > 1);
    let mut key = if arg.is_required_set() {
      format!("<{value_name}>")
    } else {
      format!("[{value_name}]")
    };
    if multiple {
      key.push_str("...");
    }
    return key;
  }
  let mut parts = Vec::new();
  if let Some(short) = arg.get_short() {
    parts.push(format!("-{short}"));
  }
  if let Some(long) = arg.get_long() {
    parts.push(format!("--{long}"));
  }
  let mut key = parts.join(", ");
  if arg.get_action().takes_values() {
    key.push_str(&format!(" <{value_name}>"));
  }
  key
}
