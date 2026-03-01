//! Chat command — interactive REPL wrapping batch mode
//!
//! Same commands and pipeline syntax as `cqs batch`, with readline editing,
//! history, and tab completion.

use anyhow::Result;
use clap::Parser;
use rustyline::completion::{Completer, Pair};
use rustyline::error::ReadlineError;
use rustyline::highlight::Highlighter;
use rustyline::hint::Hinter;
use rustyline::validate::Validator;
use rustyline::{Editor, Helper};

use super::batch;

// ─── Completer ───────────────────────────────────────────────────────────────

struct ChatHelper {
    commands: Vec<String>,
}

impl Completer for ChatHelper {
    type Candidate = Pair;

    fn complete(
        &self,
        line: &str,
        pos: usize,
        _ctx: &rustyline::Context<'_>,
    ) -> rustyline::Result<(usize, Vec<Pair>)> {
        // Only complete the first token (command name)
        let prefix = &line[..pos];
        if prefix.contains(' ') {
            return Ok((pos, vec![]));
        }

        let matches: Vec<Pair> = self
            .commands
            .iter()
            .filter(|cmd| cmd.starts_with(prefix))
            .map(|cmd| Pair {
                display: cmd.clone(),
                replacement: cmd.clone(),
            })
            .collect();

        Ok((0, matches))
    }
}

impl Hinter for ChatHelper {
    type Hint = String;
}
impl Highlighter for ChatHelper {}
impl Validator for ChatHelper {}
impl Helper for ChatHelper {}

// ─── Meta-commands ───────────────────────────────────────────────────────────

/// Handle meta-commands (help, exit, quit, clear).
/// Returns Some(true) for exit/quit, Some(false) for other meta-commands, None if not a meta-command.
fn handle_meta(line: &str) -> Option<bool> {
    match line.to_ascii_lowercase().as_str() {
        "exit" | "quit" => Some(true),
        "help" => {
            println!(
                "Available commands: search, blame, callers, callees, deps, explain, similar,"
            );
            println!("  gather, impact, test-map, trace, dead, related, context, stats, onboard,");
            println!("  scout, where, read, stale, health, drift, notes, task, help");
            println!();
            println!("Pipeline: search \"query\" | callers | test-map");
            println!("Meta: help, exit, quit, clear");
            Some(false)
        }
        "clear" => {
            // ANSI clear screen
            print!("\x1b[2J\x1b[H");
            Some(false)
        }
        _ => None,
    }
}

/// Build the sorted list of batch command names.
fn command_names() -> Vec<String> {
    let mut names = vec![
        "search", "blame", "callers", "callees", "deps", "explain", "similar", "gather", "impact",
        "test-map", "trace", "dead", "related", "context", "stats", "onboard", "scout", "where",
        "read", "stale", "health", "drift", "notes", "task", "help", // meta-commands
        "exit", "quit", "clear",
    ];
    names.sort();
    names.into_iter().map(String::from).collect()
}

// ─── REPL ────────────────────────────────────────────────────────────────────

pub(crate) fn cmd_chat(_cli: &super::Cli) -> Result<()> {
    let _span = tracing::info_span!("cmd_chat").entered();

    let ctx = batch::create_context()?;
    let history_path = ctx.cqs_dir.join("chat_history");

    let helper = ChatHelper {
        commands: command_names(),
    };

    let config = rustyline::Config::builder()
        .max_history_size(1000)
        .expect("valid history size")
        .build();
    let mut editor = Editor::with_config(config)?;
    editor.set_helper(Some(helper));

    // Load history (ignore if missing)
    let _ = editor.load_history(&history_path);

    println!("cqs interactive mode. Type 'help' for commands, 'exit' to quit.");

    loop {
        match editor.readline("cqs> ") {
            Ok(line) => {
                let trimmed = line.trim();
                if trimmed.is_empty() || trimmed.starts_with('#') {
                    continue;
                }

                // Check meta-commands
                if let Some(should_exit) = handle_meta(trimmed) {
                    if should_exit {
                        break;
                    }
                    continue;
                }

                let _ = editor.add_history_entry(trimmed);

                // Tokenize
                let tokens = match shell_words::split(trimmed) {
                    Ok(t) => t,
                    Err(e) => {
                        eprintln!("Parse error: {}", e);
                        continue;
                    }
                };

                if tokens.is_empty() {
                    continue;
                }

                // Check idle timeout
                ctx.check_idle_timeout();

                // Execute: pipeline or single command
                let result = if batch::has_pipe_token(&tokens) {
                    batch::execute_pipeline(&ctx, &tokens, trimmed)
                } else {
                    match batch::BatchInput::try_parse_from(&tokens) {
                        Ok(input) => match batch::dispatch(&ctx, input.cmd) {
                            Ok(value) => value,
                            Err(e) => {
                                tracing::warn!(error = %e, command = trimmed, "Command failed");
                                eprintln!("Error: {}", e);
                                continue;
                            }
                        },
                        Err(e) => {
                            eprintln!("{}", e);
                            continue;
                        }
                    }
                };

                // Pretty-print result
                match serde_json::to_string_pretty(&result) {
                    Ok(s) => println!("{}", s),
                    Err(e) => {
                        tracing::warn!(error = %e, "Failed to format result");
                        eprintln!("Error formatting output: {}", e);
                    }
                }
            }
            Err(ReadlineError::Interrupted) => {
                // Ctrl+C — just show new prompt
                continue;
            }
            Err(ReadlineError::Eof) => {
                // Ctrl+D — exit
                break;
            }
            Err(e) => {
                tracing::warn!(error = %e, "Readline error");
                eprintln!("Error: {}", e);
                break;
            }
        }
    }

    // Save history
    if let Err(e) = editor.save_history(&history_path) {
        tracing::warn!(error = %e, "Failed to save chat history");
    }

    Ok(())
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_command_names_complete() {
        let names = command_names();
        assert!(names.contains(&"search".to_string()));
        assert!(names.contains(&"callers".to_string()));
        assert!(names.contains(&"blame".to_string()));
        assert!(names.contains(&"explain".to_string()));
        assert!(names.contains(&"help".to_string()));
        assert!(names.contains(&"exit".to_string()));
    }

    #[test]
    fn test_command_names_sorted() {
        let names = command_names();
        let mut sorted = names.clone();
        sorted.sort();
        assert_eq!(names, sorted);
    }

    #[test]
    fn test_handle_meta_help() {
        assert_eq!(handle_meta("help"), Some(false));
        assert_eq!(handle_meta("HELP"), Some(false));
        assert_eq!(handle_meta("Help"), Some(false));
    }

    #[test]
    fn test_handle_meta_exit() {
        assert_eq!(handle_meta("exit"), Some(true));
        assert_eq!(handle_meta("quit"), Some(true));
        assert_eq!(handle_meta("EXIT"), Some(true));
        assert_eq!(handle_meta("Quit"), Some(true));
    }

    #[test]
    fn test_handle_meta_not_meta() {
        assert_eq!(handle_meta("search foo"), None);
        assert_eq!(handle_meta("callers bar"), None);
        assert_eq!(handle_meta(""), None);
    }
}
