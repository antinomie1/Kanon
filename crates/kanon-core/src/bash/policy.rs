//! A deliberately small, static shell grammar. Unknown executables and syntax fail closed.
//!
//! The parsed arguments are quoted again before Bash sees them, so validation and execution never
//! disagree about expansions, quoting, executable names or shell operators.

use std::path::Path;

const SEARCH_DIRS: &[&str] = &["/usr/bin", "/bin", "/usr/local/bin", "/opt/homebrew/bin"];

/// Validates every command before returning any executable shell text.
pub(super) fn prepare(input: &str) -> Result<String, String> {
    if input.trim().is_empty() || input.len() > 16 * 1024 || input.contains('\0') {
        return Err("Command must be nonempty, NUL-free and at most 16384 bytes".into());
    }
    let mut chars = input.chars().peekable();
    let mut word = String::new();
    let mut started = false;
    let mut quote = None;
    let mut words = Vec::new();
    let mut output = Vec::new();
    let mut last_operator = None;
    while let Some(ch) = chars.next() {
        match (quote, ch) {
            (Some('\''), '\'') | (Some('"'), '"') => quote = None,
            (Some('\''), _) => word.push(ch),
            (_, '\\') => {
                let next = chars.next().ok_or("Incomplete escape")?;
                if next == '\n' || next == '\r' {
                    return Err("Line continuations are blocked".into());
                }
                if quote == Some('"') && !matches!(next, '$' | '`' | '"' | '\\') {
                    word.push('\\');
                }
                word.push(next);
                started = true;
            }
            (_, '$' | '`') => {
                return Err("Shell expansion and command substitution are blocked".into());
            }
            (Some('"'), _) => word.push(ch),
            (None, '\'' | '"') => {
                quote = Some(ch);
                started = true;
            }
            (None, ' ' | '\t' | '\r') => {
                if started {
                    words.push(std::mem::take(&mut word));
                    started = false;
                }
            }
            (None, ';' | '\n' | '|' | '&') => {
                if started {
                    words.push(std::mem::take(&mut word));
                    started = false;
                }
                let op = match ch {
                    '|' if chars.peek() == Some(&'|') => {
                        chars.next();
                        "||"
                    }
                    '&' if chars.peek() == Some(&'&') => {
                        chars.next();
                        "&&"
                    }
                    '&' => return Err("Background execution is blocked".into()),
                    '|' => "|",
                    _ => ";",
                };
                if words.is_empty() {
                    if op == ";" && (output.is_empty() || last_operator == Some(";")) {
                        continue;
                    }
                    return Err("Empty command or unsupported shell operator".into());
                }
                output.push(prepare_command(&words)?);
                words.clear();
                output.push(op.to_string());
                last_operator = Some(op);
            }
            (None, '<' | '>' | '(' | ')' | '{' | '}' | '#' | '*' | '?' | '[' | ']' | '~') => {
                return Err(
                    "Redirection, control syntax, comments and unquoted expansions are blocked"
                        .into(),
                );
            }
            _ => {
                word.push(ch);
                started = true;
            }
        }
    }
    if quote.is_some() {
        return Err("Unclosed shell quote".into());
    }
    if started {
        words.push(word);
    }
    if !words.is_empty() {
        output.push(prepare_command(&words)?);
    } else if last_operator.is_some_and(|op| op != ";") {
        return Err("Missing command after operator".into());
    }
    if output.is_empty() {
        return Err("No command supplied".into());
    }
    Ok(output.join(" "))
}

fn prepare_command(words: &[String]) -> Result<String, String> {
    let name = Path::new(&words[0])
        .file_name()
        .and_then(|v| v.to_str())
        .unwrap_or("");
    // Explicit risk names make refusals actionable; the allowlist also rejects unknown programs,
    // shell wrappers, interpreters, user scripts and launchers such as env, xargs and busybox.
    if matches!(
        name,
        "rm" | "rmdir"
            | "dd"
            | "sudo"
            | "su"
            | "doas"
            | "mkfs"
            | "shred"
            | "chmod"
            | "chown"
            | "mv"
            | "cp"
            | "tee"
            | "mount"
            | "umount"
            | "shutdown"
            | "reboot"
            | "kill"
            | "pkill"
            | "killall"
    ) || name.starts_with("mkfs.")
    {
        return Err(format!("High-risk command '{name}' is blocked"));
    }
    if words[0] != name
        && !SEARCH_DIRS
            .iter()
            .any(|dir| words[0] == format!("{dir}/{name}"))
    {
        return Err("Executable paths outside trusted system directories are blocked".into());
    }
    let args = &words[1..];
    if !matches!(
        name,
        "echo"
            | "printf"
            | "pwd"
            | "ls"
            | "cat"
            | "head"
            | "tail"
            | "wc"
            | "grep"
            | "rg"
            | "cut"
            | "tr"
            | "du"
            | "df"
            | "uname"
            | "whoami"
            | "id"
            | "ps"
            | "uptime"
            | "sleep"
            | "seq"
            | "git"
            | "true"
            | "false"
    ) {
        return Err(format!(
            "Command '{name}' is not allowed by the default Bash policy"
        ));
    }

    // Ripgrep can launch a preprocessor or hostname helper. Arguments must never extend
    // the executable allowlist, even when the top-level program is a diagnostic utility.
    if name == "rg"
        && args
            .iter()
            .any(|arg| arg.starts_with("--pre") || arg.starts_with("--hostname-bin"))
    {
        return Err("Ripgrep external helpers are blocked".into());
    }

    let mut executable = if matches!(name, "echo" | "printf" | "pwd" | "true" | "false") {
        format!("builtin {name}")
    } else {
        let path = SEARCH_DIRS
            .iter()
            .map(|dir| format!("{dir}/{name}"))
            .find(|path| Path::new(path).is_file())
            .ok_or_else(|| format!("Allowed executable '{name}' is unavailable on this host"))?;
        shell_quote(&path)
    };
    if name == "git" {
        let subcommand = args.first().map(String::as_str).unwrap_or("");
        // Keep Git options fail-closed too: even a read-only subcommand can delegate to a helper
        // (for example --help or --show-signature). Unknown future flags must never open that gate.
        let safe_options = [
            "--short",
            "--porcelain",
            "--branch",
            "--untracked-files",
            "--ignored",
            "--stat",
            "--numstat",
            "--shortstat",
            "--name-only",
            "--name-status",
            "--summary",
            "--compact-summary",
            "--patch",
            "--no-patch",
            "--cached",
            "--staged",
            "--oneline",
            "--all",
            "--branches",
            "--tags",
            "--remotes",
            "--decorate",
            "--no-decorate",
            "--graph",
            "--max-count",
            "--date",
            "--format",
            "--pretty",
            "--abbrev",
            "--no-abbrev",
            "--abbrev-commit",
            "--no-abbrev-commit",
            "--show-toplevel",
            "--show-prefix",
            "--is-inside-work-tree",
            "--verify",
            "--symbolic-full-name",
            "--git-dir",
            "--count",
            "--others",
            "--stage",
            "--deleted",
            "--modified",
            "--exclude-standard",
            "--color",
            "--no-color",
            "--end-of-options",
            "--no-ext-diff",
            "--no-textconv",
            "--no-renames",
            "--find-renames",
            "--unified",
            "-s",
            "-b",
            "-p",
            "-u",
            "-z",
            "-n",
            "-l",
            "-m",
            "-d",
            "-h",
            "--",
        ];
        if !matches!(
            subcommand,
            "status" | "diff" | "log" | "show" | "ls-files" | "rev-parse"
        ) || args.iter().any(|arg| {
            let flag = arg.split('=').next().unwrap_or(arg);
            let numeric = flag
                .strip_prefix("-n")
                .or_else(|| flag.strip_prefix("-U"))
                .or_else(|| flag.strip_prefix('-'))
                .is_some_and(|value| {
                    !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit())
                });
            flag.starts_with('-') && !safe_options.contains(&flag) && !numeric
        }) {
            return Err(
                "Only read-only Git status/diff/log/show/ls-files/rev-parse are allowed".into(),
            );
        }
        // Repository configuration must not launch a pager, fsmonitor, diff or signature helper.
        executable.push_str(
            " --no-pager --no-optional-locks -c core.fsmonitor=false -c log.showSignature=false",
        );
        executable.push_str(
            " -c gpg.program=/dev/null -c gpg.ssh.program=/dev/null -c gpg.x509.program=/dev/null",
        );
        executable.push(' ');
        executable.push_str(subcommand);
        if matches!(subcommand, "diff" | "log" | "show") {
            executable.push_str(" --no-ext-diff --no-textconv");
        }
        return Ok(format!(
            "{executable} {}",
            args[1..]
                .iter()
                .map(|arg| shell_quote(arg))
                .collect::<Vec<_>>()
                .join(" ")
        ));
    }
    Ok(format!(
        "{executable} {}",
        args.iter()
            .map(|arg| shell_quote(arg))
            .collect::<Vec<_>>()
            .join(" ")
    ))
}

fn shell_quote(word: &str) -> String {
    format!("'{}'", word.replace('\'', "'\\''"))
}
