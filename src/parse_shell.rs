use anyhow::{Result, bail};

/// Splits a `cmd` string into argv the way sh splits a simple command,
/// without running a shell. Spaces and tabs separate words. `'...'` keeps
/// everything as written; `"..."` keeps everything but `\` before `"`,
/// `\`, `$`, `` ` ``, or a newline; a `\` outside quotes keeps the next
/// character, and `\` with a newline joins two lines.
///
/// Anything else sh would treat specially is an error rather than an
/// argument, so a string accepted now means the same in a shell later:
/// operators, `$`, `` ` ``, globs, `~` and `#` at the start of a word, a
/// newline between words, `NAME=value` before the program, and a reserved
/// word such as `!` or `if` as the program.
pub fn split_argv(s: &str) -> Result<Vec<String>> {
  let mut args: Vec<String> = Vec::new();
  let mut cur = String::new();
  // The word so far has a quote or a `\`, so it is not a keyword or an
  // assignment, and it is a word even if empty, as `''` is.
  let mut quoted = false;
  let mut chars = s.chars();

  while let Some(c) = chars.next() {
    match c {
      ' ' | '\t' | '\n' => {
        if quoted || !cur.is_empty() {
          end_word(&mut args, std::mem::take(&mut cur), quoted)?;
          quoted = false;
        }
        if c == '\n'
          && !args.is_empty()
          && !chars.as_str().trim_matches([' ', '\t', '\n']).is_empty()
        {
          bail!(
            "a newline would end the command in a shell; cmd runs one \
             program, so keep it on one line or end the line with `\\`"
          );
        }
      }
      '\'' => {
        quoted = true;
        loop {
          match chars.next() {
            Some('\'') => break,
            Some(c) => cur.push(c),
            None => bail!("missing closing single quote"),
          }
        }
      }
      '"' => {
        quoted = true;
        loop {
          match chars.next() {
            Some('"') => break,
            Some('\\') => match chars.next() {
              Some(c @ ('"' | '\\' | '$' | '`')) => cur.push(c),
              Some('\n') => {}
              Some(c) => {
                cur.push('\\');
                cur.push(c);
              }
              None => bail!("missing closing double quote"),
            },
            Some(c @ ('$' | '`')) => bail!(
              "{} inside \"...\" would expand in a shell; escape it with \
               `\\`, or use '...'",
              code(c)
            ),
            Some(c) => cur.push(c),
            None => bail!("missing closing double quote"),
          }
        }
      }
      '\\' => match chars.next() {
        Some('\n') => {}
        Some(c) => {
          quoted = true;
          cur.push(c);
        }
        None => bail!("a trailing `\\` escapes nothing"),
      },
      '#' | '~' if !quoted && cur.is_empty() => bail!(unsupported(c)),
      '|' | '&' | ';' | '<' | '>' | '(' | ')' | '$' | '`' | '*' | '?' | '[' => {
        bail!(unsupported(c))
      }
      '=' if args.is_empty() && !quoted && is_name(&cur) => bail!(
        "`{cur}=` would set a variable in a shell; cmd runs one program \
         without a shell, so set it in `env`, or quote the word"
      ),
      _ => cur.push(c),
    }
  }
  if quoted || !cur.is_empty() {
    end_word(&mut args, cur, quoted)?;
  }
  if args.is_empty() {
    bail!("cmd string is empty");
  }
  Ok(args)
}

fn end_word(args: &mut Vec<String>, word: String, quoted: bool) -> Result<()> {
  if args.is_empty() && !quoted && RESERVED.contains(&word.as_str()) {
    bail!(
      "`{word}` is a shell keyword; cmd runs one program without a shell, \
       so quote it to run a program named `{word}`"
    );
  }
  args.push(word);
  Ok(())
}

/// sh's reserved words, special only as the program.
const RESERVED: &[&str] = &[
  "!", "{", "}", "case", "do", "done", "elif", "else", "esac", "fi", "for",
  "if", "in", "then", "until", "while",
];

fn is_name(word: &str) -> bool {
  let mut chars = word.chars();
  match chars.next() {
    Some(c) if c.is_ascii_alphabetic() || c == '_' => {
      chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
    }
    _ => false,
  }
}

fn unsupported(c: char) -> String {
  let what = match c {
    '$' => "variables",
    '`' => "command substitution",
    '#' => "comments",
    '~' => "home directory expansion",
    '*' | '?' | '[' => "globs",
    _ => "operators",
  };
  format!(
    "{} needs quotes: cmd runs one program without a shell, so it has no \
     {what}; to use a shell, run one, as in `bash -c '...'`",
    code(c)
  )
}

/// `c` in backquotes, as in markdown.
fn code(c: char) -> String {
  match c {
    '`' => "`` ` ``".to_string(),
    c => format!("`{c}`"),
  }
}

/// The argv that runs `line` through the system shell: `/bin/sh -c`, or
/// PowerShell on Windows. Only for mprocs.yaml (`dekit mprocs`) and for
/// stop lines saved by older binaries.
#[cfg(windows)]
pub fn system_argv(line: &str) -> Vec<String> {
  // Prefer PowerShell 7, but fall back to Windows PowerShell if not installed.
  let shell_exe = if which::which("pwsh.exe").is_ok() {
    "pwsh.exe"
  } else {
    "powershell.exe"
  };
  vec![shell_exe.into(), "-Command".into(), line.into()]
}

#[cfg(not(windows))]
pub fn system_argv(line: &str) -> Vec<String> {
  vec!["/bin/sh".into(), "-c".into(), line.into()]
}

#[cfg(test)]
mod tests {
  use super::*;

  fn split(s: &str) -> Vec<String> {
    split_argv(s).unwrap_or_else(|err| panic!("{s}: {err}"))
  }

  #[test]
  fn splits_like_sh() {
    for (line, argv) in [
      ("npm  run\tdev", &["npm", "run", "dev"][..]),
      ("\n  npm run dev\n\n", &["npm", "run", "dev"]),
      ("echo 'a b' \"c d\"", &["echo", "a b", "c d"]),
      ("echo a'b'\"c\"d", &["echo", "abcd"]),
      ("echo '' \"\"", &["echo", "", ""]),
      (
        r#"echo 'it''s' "say \"hi\"""#,
        &["echo", "its", "say \"hi\""],
      ),
      (
        r"echo 'a\b' 'x$y' '#' '*'",
        &["echo", r"a\b", "x$y", "#", "*"],
      ),
      (
        r#"echo "a\b" "\$x" "\\" "\`""#,
        &["echo", r"a\b", "$x", "\\", "`"],
      ),
      (
        r"echo a\ b \' \# \| \$x \* \~",
        &["echo", "a b", "'", "#", "|", "$x", "*", "~"],
      ),
      (
        "echo \"~/x\" '*.log' \"[a]\"",
        &["echo", "~/x", "*.log", "[a]"],
      ),
      ("echo a \\\n b", &["echo", "a", "b"]),
      ("echo \"a \\\nb\" \"c\nd\"", &["echo", "a b", "c\nd"]),
      (
        "git log a#b HEAD~1 x] {} !",
        &["git", "log", "a#b", "HEAD~1", "x]", "{}", "!"],
      ),
      (
        "env A=1 node --port=3000",
        &["env", "A=1", "node", "--port=3000"],
      ),
      ("'A=1' node", &["A=1", "node"]),
      ("A\\=1 node", &["A=1", "node"]),
      ("1A=1 node", &["1A=1", "node"]),
      ("'if' x", &["if", "x"]),
      ("iffy then", &["iffy", "then"]),
    ] {
      assert_eq!(split(line), argv, "{line}");
    }
  }

  #[test]
  fn shell_syntax_is_an_error() {
    for (line, expected) in [
      ("", "cmd string is empty"),
      ("  \n", "cmd string is empty"),
      ("echo 'oops", "missing closing single quote"),
      ("echo \"oops", "missing closing double quote"),
      ("echo \"oops\\", "missing closing double quote"),
      ("echo \\", "a trailing `\\` escapes nothing"),
      ("a && b", "`&` needs quotes"),
      ("a | b", "`|` needs quotes"),
      ("a; b", "`;` needs quotes"),
      ("a > log", "`>` needs quotes"),
      ("a < in", "`<` needs quotes"),
      ("(a)", "`(` needs quotes"),
      ("echo $HOME", "`$` needs quotes"),
      (
        "echo \"$HOME\"",
        "`$` inside \"...\" would expand in a shell",
      ),
      ("echo \"`date`\"", "`` ` `` inside \"...\""),
      ("echo `date`", "`` ` `` needs quotes"),
      ("echo `date`", "no command substitution"),
      ("node app.js # dev", "no comments"),
      (
        "rm *.log",
        "`*` needs quotes: cmd runs one program without a shell, so it has no globs",
      ),
      ("ls a?", "no globs"),
      ("ls [ab]", "no globs"),
      ("ls ~/x", "no home directory expansion"),
      (
        "npm run a\nnpm run b",
        "a newline would end the command in a shell",
      ),
      (
        "PORT=3000 node app.js",
        "`PORT=` would set a variable in a shell",
      ),
      ("A='x y' node", "`A=` would set a variable"),
      ("! false", "`!` is a shell keyword"),
      ("if", "`if` is a shell keyword"),
      ("{ a", "`{` is a shell keyword"),
    ] {
      match split_argv(line) {
        Ok(argv) => panic!("{line:?}: accepted {argv:?}"),
        Err(err) => {
          let err = err.to_string();
          assert!(err.contains(expected), "{line:?}: {err}");
        }
      }
    }
  }
}
