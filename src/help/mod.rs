//! `dekit help`: the documentation tree embedded in the binary, its
//! parser, and its renderers.

mod compile;
mod error;
mod generate;
mod inline;
pub mod ir;
mod parse;
mod patterns;
#[cfg(test)]
mod tests;

pub use error::DocError;

mod embed {
  include!(concat!(env!("OUT_DIR"), "/docs_embed.rs"));
}

pub fn compile(
  sources: &[(&str, &str)],
  manifest: &str,
  cli: &clap::Command,
) -> Result<ir::DocsIr, Vec<DocError>> {
  compile::compile(sources, manifest, cli)
}

/// The embedded tree, compiled against this binary's command tree.
pub fn load() -> Result<ir::DocsIr, Vec<DocError>> {
  compile::compile(embed::SOURCES, embed::MANIFEST, &crate::dekit::main::cli())
}
