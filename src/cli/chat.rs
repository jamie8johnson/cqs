//! Chat command — interactive REPL wrapping batch mode
//!
//! Same commands and pipeline syntax as `cqs batch`, with readline editing,
//! history, and tab completion.

use anyhow::Result;
use clap::{CommandFactory, Parser};
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

    /// Provides command name autocompletion for the interactive shell.
    ///
    /// Filters the available commands to find those matching the prefix at the current cursor position. Only completes command names (first token); if the line contains a space, no completions are returned.
    ///
    /// # Arguments
    ///
    /// * `line` - The full input line being edited
    /// * `pos` - The cursor position within the line
    /// * `_ctx` - Rustyline context (unused)
    ///
    /// # Returns
    ///
    /// A tuple containing the start position for replacement (0 if completions found, otherwise `pos`) and a vector of completion candidates as `Pair` objects with matching command names.
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

/// Result of a meta-command, returned by [`handle_meta`].
#[derive(Debug, PartialEq, Eq)]
enum MetaAction {
    /// Quit the REPL.
    Exit,
    /// Continue the loop (no exit).
    Continue,
    /// Continue, and ask the caller to wipe persisted chat history.
    ClearHistory,
}

/// Handle meta-commands (help, exit, quit, clear, clear-history).
/// Returns `None` if `line` is not a meta-command.
fn handle_meta(line: &str) -> Option<MetaAction> {
    match line.to_ascii_lowercase().as_str() {
        "exit" | "quit" => Some(MetaAction::Exit),
        "help" => {
            let app = batch::BatchInput::command();
            let mut cmd_names: Vec<&str> = app.get_subcommands().map(|sc| sc.get_name()).collect();
            cmd_names.sort();
            println!("Available commands: {}", cmd_names.join(", "));
            println!();
            println!("Pipeline: search \"query\" | callers | test-map");
            println!("Meta: help, exit, quit, clear, clear-history");
            Some(MetaAction::Continue)
        }
        "clear" => {
            // ANSI clear screen
            print!("\x1b[2J\x1b[H");
            Some(MetaAction::Continue)
        }
        // P3 #137: wipe persisted history on demand without leaving the REPL.
        "clear-history" => Some(MetaAction::ClearHistory),
        _ => None,
    }
}

/// Build the sorted list of batch command names, derived from clap's subcommand
/// registry so it stays in sync automatically when new commands are added.
fn command_names() -> Vec<String> {
    let app = batch::BatchInput::command();
    let mut names: Vec<String> = app
        .get_subcommands()
        .map(|sc| sc.get_name().to_string())
        .collect();
    // Meta-commands not in BatchCmd
    for meta in ["exit", "quit", "clear"] {
        names.push(meta.to_string());
    }
    names.sort();
    names.dedup();
    names
}

// ─── REPL ────────────────────────────────────────────────────────────────────

/// Starts an interactive chat session with command-line interface for querying.
///
/// # Arguments
///
/// None. Uses internal context and configuration.
///
/// # Returns
///
/// Returns `Ok(())` on successful completion of the chat session, or an error if context creation or editor initialization fails.
///
/// # Errors
///
/// Returns an error if the CQS context cannot be created or if the rustyline editor cannot be initialized.
///
/// # Panics
///
/// Panics if the history size configuration (1000) is invalid, though this should never occur with a valid u64 value.
pub(crate) fn cmd_chat() -> Result<()> {
    let _span = tracing::info_span!("cmd_chat").entered();

    let ctx = batch::create_context()?;
    ctx.warm(); // Pre-warm embedder so first query doesn't pay ~500ms ONNX init

    // P3 #137: `CQS_CHAT_HISTORY=0` opts out of disk-persisted history.
    // The `chat_history` file otherwise captures every command (including
    // sensitive search queries) in plain text under the project `.cqs/`
    // directory. Default is enabled for ergonomic up-arrow recall.
    let history_enabled = std::env::var("CQS_CHAT_HISTORY")
        .ok()
        .as_deref()
        .map(|v| v != "0")
        .unwrap_or(true);
    let history_path = ctx.cqs_dir.join("chat_history");
    // #1127: wrap in Arc<Mutex> so the chat path uses the same view-based
    // dispatch as the daemon and `cmd_batch`. Single-threaded loop, so
    // contention is zero — wrapper is two pointer indirections per command.
    let ctx = std::sync::Arc::new(std::sync::Mutex::new(ctx));

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
    if history_enabled {
        let _ = editor.load_history(&history_path);
    }

    println!("cqs interactive mode. Type 'help' for commands, 'exit' to quit.");
    if !history_enabled {
        println!("(chat history disabled by CQS_CHAT_HISTORY=0)");
    }

    loop {
        match editor.readline("cqs> ") {
            Ok(line) => {
                // Input length guard (RT-RES-1) — matches batch mode's 1MB limit
                if line.len() > 1_048_576 {
                    eprintln!("Input too long ({} bytes, max 1MB)", line.len());
                    continue;
                }
                let trimmed = line.trim();
                if trimmed.is_empty() || trimmed.starts_with('#') {
                    continue;
                }

                // Check meta-commands
                if let Some(action) = handle_meta(trimmed) {
                    match action {
                        MetaAction::Exit => break,
                        MetaAction::Continue => {}
                        MetaAction::ClearHistory => {
                            // P3 #137: wipe both in-memory and on-disk history.
                            editor.clear_history()?;
                            if history_path.exists() {
                                if let Err(e) = std::fs::remove_file(&history_path) {
                                    tracing::warn!(
                                        error = %e,
                                        path = %history_path.display(),
                                        "Failed to remove chat history file"
                                    );
                                } else {
                                    println!("Chat history cleared.");
                                }
                            } else {
                                println!("No chat history to clear.");
                            }
                        }
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

                // #1127: snapshot a BatchView. Idle-timeout sweep runs inside
                // `checkout_view_from_arc`. Refresh re-locks the BatchContext
                // briefly via the view's outer_lock; everything else runs
                // outside the lock.
                let view = batch::checkout_view_from_arc(&ctx);

                // Execute: pipeline or single command
                let result = if batch::has_pipe_token(&tokens) {
                    match batch::execute_pipeline(&view, &tokens, trimmed) {
                        Ok(value) => value,
                        Err(pe) => {
                            tracing::warn!(code = pe.code, message = %pe.message, command = trimmed, "Pipeline failed");
                            eprintln!("Error: {}", pe.message);
                            continue;
                        }
                    }
                } else {
                    match batch::BatchInput::try_parse_from(&tokens) {
                        Ok(input) => match batch::dispatch(&view, input.cmd) {
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

                // Pretty-print result wrapped in standard envelope so the chat
                // surface matches batch / CLI shape. Routes through the shared
                // `format_envelope_to_string` helper so chat inherits the
                // sanitize-on-NaN retry that batch's `write_json_line` and
                // CLI's `emit_json` already perform — D.1 parity fix.
                //
                // P2 #28: `wrap_value` takes `&Value` so the chat output stays
                // reference-only at the call site; the envelope itself still
                // allocates the outer object.
                let wrapped = crate::cli::json_envelope::wrap_value(&result);
                match crate::cli::json_envelope::format_envelope_to_string(&wrapped) {
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

    // P3 #137: only save history when not opted out, and chmod to 0o600
    // immediately after rustyline writes (mirrors QueryCache::open behaviour).
    if history_enabled {
        if let Err(e) = editor.save_history(&history_path) {
            tracing::warn!(error = %e, "Failed to save chat history");
        } else {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                if history_path.exists() {
                    if let Err(e) = std::fs::set_permissions(
                        &history_path,
                        std::fs::Permissions::from_mode(0o600),
                    ) {
                        tracing::warn!(
                            error = %e,
                            path = %history_path.display(),
                            "Failed to set chat history file mode to 0o600"
                        );
                    }
                }
            }
        }
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
        assert_eq!(handle_meta("help"), Some(MetaAction::Continue));
        assert_eq!(handle_meta("HELP"), Some(MetaAction::Continue));
        assert_eq!(handle_meta("Help"), Some(MetaAction::Continue));
    }

    #[test]
    fn test_handle_meta_exit() {
        assert_eq!(handle_meta("exit"), Some(MetaAction::Exit));
        assert_eq!(handle_meta("quit"), Some(MetaAction::Exit));
        assert_eq!(handle_meta("EXIT"), Some(MetaAction::Exit));
        assert_eq!(handle_meta("Quit"), Some(MetaAction::Exit));
    }

    #[test]
    fn test_handle_meta_not_meta() {
        assert_eq!(handle_meta("search foo"), None);
        assert_eq!(handle_meta("callers bar"), None);
        assert_eq!(handle_meta(""), None);
    }

    /// P3 #137: `clear-history` returns the dedicated MetaAction so the
    /// REPL knows to wipe both editor history and the on-disk file.
    #[test]
    fn test_handle_meta_clear_history() {
        assert_eq!(handle_meta("clear-history"), Some(MetaAction::ClearHistory));
        assert_eq!(handle_meta("CLEAR-HISTORY"), Some(MetaAction::ClearHistory));
        // Unrelated patterns must not collide.
        assert_eq!(handle_meta("clearhistory"), None);
        assert_eq!(handle_meta("clear "), None);
    }

    // ===== ChatHelper::complete tests (TC-4) =====

    #[test]
    fn test_complete_empty_prefix() {
        use rustyline::completion::Completer;
        let helper = ChatHelper {
            commands: vec!["search".into(), "callers".into(), "explain".into()],
        };
        let history = rustyline::history::DefaultHistory::new();
        let ctx = rustyline::Context::new(&history);
        let (pos, matches) = helper.complete("", 0, &ctx).unwrap();
        assert_eq!(pos, 0);
        assert_eq!(matches.len(), 3);
    }

    #[test]
    fn test_complete_partial_prefix() {
        use rustyline::completion::Completer;
        let helper = ChatHelper {
            commands: vec!["search".into(), "similar".into(), "stats".into()],
        };
        let history = rustyline::history::DefaultHistory::new();
        let ctx = rustyline::Context::new(&history);
        let (pos, matches) = helper.complete("s", 1, &ctx).unwrap();
        assert_eq!(pos, 0);
        assert_eq!(matches.len(), 3);

        let (pos, matches) = helper.complete("se", 2, &ctx).unwrap();
        assert_eq!(pos, 0);
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].display, "search");
    }

    #[test]
    fn test_complete_after_space_returns_empty() {
        use rustyline::completion::Completer;
        let helper = ChatHelper {
            commands: vec!["search".into(), "callers".into()],
        };
        let history = rustyline::history::DefaultHistory::new();
        let ctx = rustyline::Context::new(&history);
        // After a space (user is typing arguments), no command completion
        let (_, matches) = helper.complete("search foo", 10, &ctx).unwrap();
        assert!(matches.is_empty());
    }

    #[test]
    fn test_complete_no_match() {
        use rustyline::completion::Completer;
        let helper = ChatHelper {
            commands: vec!["search".into(), "callers".into()],
        };
        let history = rustyline::history::DefaultHistory::new();
        let ctx = rustyline::Context::new(&history);
        let (_, matches) = helper.complete("xyz", 3, &ctx).unwrap();
        assert!(matches.is_empty());
    }
}
