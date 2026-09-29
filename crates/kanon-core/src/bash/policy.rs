//! Lightweight checks for obvious destructive commands, not a script or filesystem sandbox.
//!
//! Parse Bash syntax to distinguish actual commands from quoted data, heredocs and case patterns.
//! The original source is executed unchanged; ordinary programs, flags and script contents remain
//! available. Only recognizable static command heads and a few destructive argument forms are checked.

use std::path::Path;
use tree_sitter::{Node, Parser};

/// Screens recognizable commands before any part of the original script starts.
pub(super) fn prepare(input: &str) -> Result<String, String> {
    if input.trim().is_empty() || input.len() > 16 * 1024 || input.contains('\0') {
        return Err("Command must be nonempty, NUL-free and at most 16384 bytes".into());
    }
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_bash::LANGUAGE.into())
        .map_err(|err| err.to_string())?;
    let tree = parser
        .parse(input, None)
        .ok_or("Failed to inspect Bash syntax")?;
    if tree.root_node().has_error() {
        return Err("Malformed or unsupported Bash syntax".into());
    }
    let mut pending = vec![tree.root_node()];
    while let Some(node) = pending.pop() {
        if node.kind() == "command" {
            let mut words = vec![
                node.child_by_field_name("name")
                    .and_then(|name| literal(name, input.as_bytes())),
            ];
            let mut cursor = node.walk();
            words.extend(
                node.children_by_field_name("argument", &mut cursor)
                    .map(|arg| literal(arg, input.as_bytes())),
            );
            check_command(&words)?;
        }
        let mut cursor = node.walk();
        pending.extend(node.named_children(&mut cursor));
    }
    Ok(input.to_string())
}

fn check_command(words: &[Option<String>]) -> Result<(), String> {
    let mut index = 0;
    let Some(name) = command_head(words, &mut index) else {
        return Ok(());
    };
    let args = &words[index + 1..];
    if matches!(
        name,
        "rm" | "rmdir"
            | "dd"
            | "sudo"
            | "su"
            | "doas"
            | "mkfs"
            | "shred"
            | "fdisk"
            | "cfdisk"
            | "sfdisk"
            | "parted"
            | "wipefs"
            | "mount"
            | "umount"
            | "shutdown"
            | "reboot"
            | "poweroff"
            | "halt"
            | "killall"
    ) || name.starts_with("mkfs.")
    {
        return Err(format!("High-risk command '{name}' is blocked"));
    }
    // Indexed targets are re-evaluated by Bash's printf builtin, after ordinary shell quoting.
    if name == "printf"
        && args
            .first()
            .and_then(Option::as_deref)
            .is_some_and(|arg| arg.starts_with("-v"))
    {
        return Err("printf -v assignment targets are blocked".into());
    }
    if name == "find" {
        let mut i = 0;
        while i < args.len() {
            match args[i].as_deref() {
                Some("-delete") => return Err("find -delete is blocked".into()),
                Some("-exec" | "-execdir" | "-ok" | "-okdir") => {
                    check_command(&args[i + 1..])?;
                    break;
                }
                Some(
                    "-name" | "-iname" | "-path" | "-ipath" | "-regex" | "-iregex" | "-type"
                    | "-user" | "-group",
                ) => i += 1,
                _ => {}
            }
            i += 1;
        }
    }
    if name == "git" {
        let mut i = 0;
        while let Some(Some(arg)) = args.get(i) {
            if !arg.starts_with('-') {
                break;
            }
            i += if matches!(
                arg.as_str(),
                "-C" | "-c" | "--git-dir" | "--work-tree" | "--namespace" | "--config-env"
            ) {
                2
            } else {
                1
            };
        }
        let subcommand = args.get(i).and_then(Option::as_deref);
        let flags: Vec<&str> = args
            .get(i + 1..)
            .unwrap_or_default()
            .iter()
            .filter_map(Option::as_deref)
            .collect();
        let hard = flags
            .iter()
            .any(|arg| arg.len() > 2 && "--hard".starts_with(arg.split('=').next().unwrap_or(arg)));
        let short = |flag: char| {
            flags
                .iter()
                .any(|arg| arg.starts_with('-') && !arg.starts_with("--") && arg.contains(flag))
        };
        let force = short('f') || flags.contains(&"--force");
        let preview = short('n') || flags.contains(&"--dry-run");
        if subcommand == Some("reset") && hard || subcommand == Some("clean") && force && !preview {
            return Err("Destructive git reset --hard / clean --force is blocked".into());
        }
    }
    Ok(())
}

/// Unwraps common static launchers, preserving the positions of dynamic arguments.
fn command_head<'a>(words: &'a [Option<String>], index: &mut usize) -> Option<&'a str> {
    loop {
        let name = basename(words.get(*index)?.as_deref()?);
        if !matches!(
            name,
            "command"
                | "builtin"
                | "exec"
                | "env"
                | "nohup"
                | "nice"
                | "time"
                | "xargs"
                | "busybox"
        ) {
            return Some(name);
        }
        *index += 1;
        while let Some(Some(arg)) = words.get(*index) {
            // These modes discover a command rather than execute it.
            if name == "command" && matches!(arg.as_str(), "-v" | "-V") {
                return None;
            }
            if arg == "--" {
                *index += 1;
                break;
            }
            if !(arg.starts_with('-') || name == "env" && arg.contains('=')) {
                break;
            }
            let takes_value = match name {
                "env" => matches!(
                    arg.as_str(),
                    "-u" | "--unset" | "-C" | "--chdir" | "-a" | "--argv0"
                ),
                "xargs" => matches!(
                    arg.as_str(),
                    "-I" | "-n"
                        | "-L"
                        | "-P"
                        | "-s"
                        | "-d"
                        | "-E"
                        | "--replace"
                        | "--max-args"
                        | "--max-lines"
                        | "--max-procs"
                        | "--max-chars"
                        | "--delimiter"
                        | "--eof"
                ),
                "nice" => matches!(arg.as_str(), "-n" | "--adjustment"),
                "exec" => arg == "-a",
                "time" => matches!(arg.as_str(), "-o" | "-f" | "--output" | "--format"),
                _ => false,
            };
            *index += if takes_value { 2 } else { 1 };
        }
    }
}

/// Resolves literal quoting without evaluating substitutions or interpreter strings.
fn literal(node: Node<'_>, source: &[u8]) -> Option<String> {
    let text = node.utf8_text(source).ok()?;
    match node.kind() {
        "command_name" => literal(node.named_child(0)?, source),
        "raw_string" => Some(text.strip_prefix('\'')?.strip_suffix('\'')?.to_string()),
        "ansi_c_string" => unescape(text.strip_prefix("$'")?.strip_suffix('\'')?, true),
        "string" => {
            let mut cursor = node.walk();
            if node
                .named_children(&mut cursor)
                .any(|child| child.kind() != "string_content")
            {
                return None;
            }
            unescape(text.strip_prefix('"')?.strip_suffix('"')?, false)
        }
        "word" => unescape(text, false),
        "concatenation" => {
            let mut cursor = node.walk();
            node.named_children(&mut cursor)
                .map(|child| literal(child, source))
                .collect::<Option<Vec<_>>>()
                .map(|parts| parts.concat())
        }
        _ => None,
    }
}

fn unescape(text: &str, ansi: bool) -> Option<String> {
    let mut chars = text.chars().peekable();
    let mut output = String::new();
    while let Some(ch) = chars.next() {
        if ch != '\\' {
            output.push(ch);
            continue;
        }
        let escaped = chars.next()?;
        let value = if ansi {
            match escaped {
                'a' => '\u{0007}',
                'b' => '\u{0008}',
                'e' | 'E' => '\u{001b}',
                'f' => '\u{000c}',
                'n' => '\n',
                'r' => '\r',
                't' => '\t',
                'v' => '\u{000b}',
                'x' | 'u' | 'U' | '0'..='7' => {
                    let radix = if escaped.is_ascii_digit() { 8 } else { 16 };
                    let limit = match escaped {
                        'x' => 2,
                        'u' => 4,
                        'U' => 8,
                        '0' => 4,
                        _ => 3,
                    };
                    let mut digits = if radix == 8 {
                        escaped.to_string()
                    } else {
                        String::new()
                    };
                    while digits.len() < limit && chars.peek().is_some_and(|ch| ch.is_digit(radix))
                    {
                        digits.push(chars.next()?);
                    }
                    char::from_u32(u32::from_str_radix(&digits, radix).ok()?)?
                }
                'c' => char::from_u32(chars.next()?.to_ascii_uppercase() as u32 & 0x1f)?,
                '\\' | '\'' | '"' => escaped,
                _ => {
                    output.push('\\');
                    escaped
                }
            }
        } else {
            escaped
        };
        if value == '\0' {
            break;
        }
        if ansi || value != '\n' {
            output.push(value);
        }
    }
    Some(output)
}

fn basename(word: &str) -> &str {
    Path::new(word)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(word)
}
