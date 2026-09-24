use std::fmt;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DocError {
  pub path: String,
  pub line: usize,
  pub column: Option<usize>,
  pub message: String,
}

impl fmt::Display for DocError {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self.column {
      Some(column) => {
        write!(
          f,
          "{}:{}:{}: {}",
          self.path, self.line, column, self.message
        )
      }
      None => write!(f, "{}:{}: {}", self.path, self.line, self.message),
    }
  }
}

pub fn err(path: &str, line: usize, message: impl Into<String>) -> DocError {
  DocError {
    path: path.to_string(),
    line,
    column: None,
    message: message.into(),
  }
}

pub fn err_at(
  path: &str,
  line: usize,
  column: usize,
  message: impl Into<String>,
) -> DocError {
  DocError {
    path: path.to_string(),
    line,
    column: Some(column),
    message: message.into(),
  }
}

pub fn join(errors: &[DocError]) -> String {
  errors
    .iter()
    .map(|error| error.to_string())
    .collect::<Vec<_>>()
    .join("\n")
}
