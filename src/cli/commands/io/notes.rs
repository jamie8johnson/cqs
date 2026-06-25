//! Notes command for cqs
//!
//! Lists and manages notes from docs/notes.toml.

use anyhow::{bail, Context, Result};
use std::path::PathBuf;

use cqs::{
    parse_notes, rewrite_notes_file, NoteEntry, MAX_NOTE_MENTIONS, MAX_NOTE_MENTION_BYTES,
    MAX_NOTE_TEXT_BYTES, NOTES_HEADER,
};

use crate::cli::definitions::TextJsonArgs;
use crate::cli::{find_project_root, Cli};

// ---------------------------------------------------------------------------
// Output structs
// ---------------------------------------------------------------------------

/// JSON output for note mutation commands (add, update, remove).
#[derive(Debug, serde::Serialize)]
struct NoteMutationOutput {
    status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(rename = "type")]
    note_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    sentiment: Option<f32>,
    text_preview: String,
    file: String,
    indexed: bool,
    total_notes: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    index_error: Option<String>,
}

/// A single note entry in the list output — the union schema both surfaces
/// emit. `id` + `type` came from the CLI path; `sentiment_label` came from the
/// daemon path; the union carries all of them so a consumer gets the same
/// fields regardless of which surface served the request.
#[derive(Debug, serde::Serialize)]
struct NoteListEntry {
    id: String,
    sentiment: f32,
    /// Coarse sentiment bucket: `"warning"` / `"pattern"` / `"neutral"`.
    /// Distinct from `sentiment_label` (the note-injection header label,
    /// `WARNING` / `PATTERN` / `NOTE`).
    #[serde(rename = "type")]
    note_type: String,
    /// Note-injection header label (`WARNING` / `PATTERN` / `NOTE`), mirroring
    /// `Note::sentiment_label`. Present on both surfaces post-unification.
    sentiment_label: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    kind: Option<String>,
    text: String,
    mentions: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stale_mentions: Option<Vec<String>>,
}

/// Envelope for `notes list --json` — the union object both surfaces emit.
///
/// An object (not a bare array) so the daemon path can splice `_meta`
/// (worktree-staleness) onto it — a bare array drops `_meta` in the
/// slim-envelope translation. `count` mirrors the array length for cheap
/// pagination/summary reads.
#[derive(Debug, serde::Serialize)]
struct NotesListOutput {
    notes: Vec<NoteListEntry>,
    count: usize,
}

// ─── Mutation cores (surface-agnostic, MCP-ready) ──────────────────────────────

/// Input for [`notes_add_core`]: the field surface a note-add consumer sets.
/// Derives `serde::Deserialize` + `schemars::JsonSchema` so it doubles as the
/// MCP `cqs_notes_add` `inputSchema` source and the daemon deserialize target.
///
/// `#[serde(default)]` on the struct so a wire/MCP caller can supply just
/// `text` and inherit the production defaults for the rest.
#[derive(Debug, Clone, PartialEq, Default, serde::Deserialize, schemars::JsonSchema)]
#[serde(default)]
pub(crate) struct NotesAddArgs {
    /// Note text (natural language). Required; whitespace-only is rejected.
    pub text: String,
    /// Sentiment on the discrete grid {-1, -0.5, 0, 0.5, 1}; off-grid values
    /// snap to the nearest.
    pub sentiment: f32,
    /// File paths or concepts this note relates to.
    pub mentions: Option<Vec<String>>,
    /// Structured kind tag (`todo`, `design-decision`, `known-bug`, …);
    /// kebab-case lowercase by convention. Empty string is treated as absent.
    pub kind: Option<String>,
}

/// Input for [`notes_update_core`]: the exact match-text plus the optional new
/// fields. At least one `new_*` field must be set (enforced in the core).
#[derive(Debug, Clone, PartialEq, Default, serde::Deserialize, schemars::JsonSchema)]
#[serde(default)]
pub(crate) struct NotesUpdateArgs {
    /// Exact (trimmed) text of the note to update — the match key.
    pub text: String,
    /// Replacement text. When omitted, the existing text is kept.
    pub new_text: Option<String>,
    /// Replacement sentiment (snapped to the discrete grid). When omitted, kept.
    pub new_sentiment: Option<f32>,
    /// Replacement mentions (replaces the whole list). When omitted, kept.
    pub new_mentions: Option<Vec<String>>,
    /// Replacement kind tag. Pass an empty string to clear; omit to keep.
    pub new_kind: Option<String>,
}

/// Input for [`notes_remove_core`]: the exact match-text of the note to delete.
#[derive(Debug, Clone, PartialEq, Default, serde::Deserialize, schemars::JsonSchema)]
#[serde(default)]
pub(crate) struct NotesRemoveArgs {
    /// Exact (trimmed) text of the note to remove — the match key.
    pub text: String,
}

/// What a mutation core produced for the adapter to format. Carries the
/// post-write facts (status, the preview text, the sentiment/label for `add`)
/// but does NOT perform any reindex — the gate-safe daemon path leaves the
/// `docs/notes.toml` reindex to the watch loop, and the CLI adapter runs its
/// own reindex on top of this.
pub(crate) struct NoteMutationCore {
    /// `"added"` / `"updated"` / `"removed"`.
    pub status: &'static str,
    /// Text-preview for the confirmation (already truncated).
    pub text_preview: String,
    /// The coarse sentiment label (`"warning"`/`"pattern"`/`"observation"`),
    /// set for `add` only — `update`/`remove` leave it `None`.
    pub note_type: Option<&'static str>,
    /// The snapped sentiment, set for `add` only.
    pub sentiment: Option<f32>,
}

/// Surface-agnostic core for `cqs notes add`: validate text/sentiment/mentions/
/// kind, then append to `docs/notes.toml` under the file lock (creating the file
/// with header if absent). Rejects whitespace-only text, over-cap text/mentions,
/// and exact (post-trim) duplicates.
///
/// **No reindex.** This is the load-bearing gate-safe path: the daemon
/// (`Store<ReadOnly>`) drives this core, writes the `notes.toml` *file*, and
/// relies on the watch loop to reindex — it never acquires a writable `Store`.
/// The CLI adapter calls this core and then reindexes itself.
pub(crate) fn notes_add_core(
    root: &std::path::Path,
    args: &NotesAddArgs,
) -> Result<NoteMutationCore> {
    let _span = tracing::info_span!(
        "notes_add_core",
        text_len = args.text.len(),
        sentiment = args.sentiment,
        kind = args.kind.as_deref().unwrap_or(""),
    )
    .entered();
    let text = validate_note_text(&args.text, "Note text")?;
    // Snap to the discrete grid BEFORE the TOML write so the file, the
    // confirmation, and (after reindex) the DB all agree — snapping only on
    // read-back would leave the raw value in notes.toml and disagree with the DB.
    let sentiment = cqs::note::snap_sentiment(args.sentiment);
    let mentions = validate_mentions(args.mentions.as_deref().unwrap_or(&[]), "mentions")?;
    let kind = normalize_kind(args.kind.as_deref());

    let note_entry = NoteEntry {
        sentiment,
        text: text.to_string(),
        mentions,
        kind,
    };

    let notes_path = ensure_notes_file(root)?;
    // Duplicate check inside the rewrite closure so it runs under the same
    // exclusive lock as the append (no read-then-write race).
    rewrite_notes_file(&notes_path, |entries| {
        if entries.iter().any(|e| e.text.trim() == text) {
            return Err(cqs::NoteError::Duplicate(format!(
                "a note with this text already exists in docs/notes.toml: '{}' \
                 (use 'cqs notes update' or 'cqs notes remove' instead)",
                text_preview(text)
            )));
        }
        entries.push(note_entry.clone());
        Ok(())
    })
    .context("Failed to add note")?;

    let note_type = if sentiment < -0.3 {
        "warning"
    } else if sentiment > 0.3 {
        "pattern"
    } else {
        "observation"
    };
    Ok(NoteMutationCore {
        status: "added",
        text_preview: text_preview(text),
        note_type: Some(note_type),
        sentiment: Some(sentiment),
    })
}

/// Surface-agnostic core for `cqs notes update`: match by exact trimmed text,
/// apply the supplied new fields to the FIRST match, rewrite `docs/notes.toml`.
/// Requires at least one `new_*` field. No reindex (see [`notes_add_core`]).
pub(crate) fn notes_update_core(
    root: &std::path::Path,
    args: &NotesUpdateArgs,
) -> Result<NoteMutationCore> {
    let _span = tracing::info_span!(
        "notes_update_core",
        text_len = args.text.len(),
        new_text_len = args.new_text.as_deref().map(str::len),
    )
    .entered();
    if args.text.trim().is_empty() {
        bail!("Note text cannot be empty or whitespace-only");
    }
    if args.new_text.is_none()
        && args.new_sentiment.is_none()
        && args.new_mentions.is_none()
        && args.new_kind.is_none()
    {
        bail!(
            "At least one of new_text, new_sentiment, new_mentions, or new_kind \
             must be provided"
        );
    }
    let new_text = args
        .new_text
        .as_deref()
        .map(|t| validate_note_text(t, "new_text"))
        .transpose()?;

    let notes_path = root.join("docs/notes.toml");
    if !notes_path.exists() {
        bail!("No notes.toml found. Use 'cqs notes add' to create notes first.");
    }

    let text_trimmed = args.text.trim();
    let new_text_owned = new_text.map(|s| s.to_string());
    let new_sentiment_clamped = args.new_sentiment.map(cqs::note::snap_sentiment);
    let new_mentions_owned = args
        .new_mentions
        .as_deref()
        .map(|m| validate_mentions(m, "new_mentions"))
        .transpose()?;
    // `Some(None)` = clear the kind; `None` = leave existing.
    let new_kind_norm: Option<Option<String>> = args.new_kind.as_deref().map(|k| {
        let trimmed = k.trim().to_ascii_lowercase();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed)
        }
    });

    rewrite_notes_file(&notes_path, |entries| {
        let entry = entries
            .iter_mut()
            .find(|e| e.text.trim() == text_trimmed)
            .ok_or_else(|| {
                cqs::NoteError::NotFound(format!(
                    "No note with text: '{}'",
                    text_preview(text_trimmed)
                ))
            })?;
        if let Some(ref t) = new_text_owned {
            entry.text = t.clone();
        }
        if let Some(s) = new_sentiment_clamped {
            entry.sentiment = s;
        }
        if let Some(ref m) = new_mentions_owned {
            entry.mentions = m.clone();
        }
        if let Some(ref k) = new_kind_norm {
            entry.kind = k.clone();
        }
        Ok(())
    })
    .context("Failed to update note")?;

    let final_text = new_text.unwrap_or(args.text.as_str());
    Ok(NoteMutationCore {
        status: "updated",
        text_preview: text_preview(final_text),
        note_type: None,
        sentiment: None,
    })
}

/// Surface-agnostic core for `cqs notes remove`: match by exact trimmed text,
/// delete the FIRST match from `docs/notes.toml`. No reindex (see
/// [`notes_add_core`]).
pub(crate) fn notes_remove_core(
    root: &std::path::Path,
    args: &NotesRemoveArgs,
) -> Result<NoteMutationCore> {
    let _span = tracing::info_span!("notes_remove_core", text_len = args.text.len()).entered();
    if args.text.trim().is_empty() {
        bail!("Note text cannot be empty or whitespace-only");
    }

    let notes_path = root.join("docs/notes.toml");
    if !notes_path.exists() {
        bail!("No notes.toml found");
    }

    let text_trimmed = args.text.trim();
    let mut removed_text = String::new();
    rewrite_notes_file(&notes_path, |entries| {
        let pos = entries
            .iter()
            .position(|e| e.text.trim() == text_trimmed)
            .ok_or_else(|| {
                cqs::NoteError::NotFound(format!(
                    "No note with text: '{}'",
                    text_preview(text_trimmed)
                ))
            })?;
        removed_text = entries[pos].text.clone();
        entries.remove(pos);
        Ok(())
    })
    .context("Failed to remove note")?;

    Ok(NoteMutationCore {
        status: "removed",
        text_preview: text_preview(&removed_text),
        note_type: None,
        sentiment: None,
    })
}

/// Serialize a [`NoteMutationCore`] into the daemon-path JSON `data` payload.
///
/// Same envelope shape as the CLI's `--json` output (`status` / `type` /
/// `sentiment` / `text_preview` / `file`), but `indexed:false` and
/// `total_notes:0` because the daemon (MCP Phase 2a) does NOT reindex from the
/// handler — it wrote the `notes.toml` *file* and leaves the reindex to the
/// watch loop (the `Store<ReadOnly>` invariant). The reindex is driven by the
/// handler flipping the shared pending-notes signal the watch loop drains every
/// tick — NOT by an inotify event, which is unreliable for this path on the WSL
/// `/mnt/c` deployment. The `reindex_deferred` flag is the honest signal that
/// the index lags the file until the next watch tick.
pub(crate) fn notes_mutation_daemon_json(core: &NoteMutationCore) -> serde_json::Value {
    let result = NoteMutationOutput {
        status: core.status.into(),
        note_type: core.note_type.map(str::to_string),
        sentiment: core.sentiment,
        text_preview: core.text_preview.clone(),
        file: "docs/notes.toml".into(),
        indexed: false,
        total_notes: 0,
        index_error: None,
    };
    let mut value = serde_json::to_value(&result).unwrap_or_else(|e| {
        // to_value on a #[derive(Serialize)] struct of owned primitives cannot
        // fail in practice; degrade rather than panic on the unreachable path.
        tracing::warn!(error = %e, "notes_mutation_daemon_json serialization failed");
        serde_json::json!({ "status": core.status, "file": "docs/notes.toml" })
    });
    if let Some(obj) = value.as_object_mut() {
        obj.insert(
            "reindex_deferred".to_string(),
            serde_json::Value::Bool(true),
        );
    }
    value
}

/// Normalize a `--kind` value: trim, lowercase, empty → None. Shared by the
/// add core and the CLI/daemon paths so add/parse stay in sync.
fn normalize_kind(raw: Option<&str>) -> Option<String> {
    raw.and_then(|k| {
        let trimmed = k.trim().to_ascii_lowercase();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed)
        }
    })
}

// ─── Surface-agnostic core ────────────────────────────────────────────────────

/// Surface-agnostic JSON core for `cqs notes list`. Both the CLI
/// (`cmd_notes_list --json`) and the daemon (`dispatch_notes`) drive it, so the
/// wire shape is identical.
///
/// **Data source:** both surfaces parse `docs/notes.toml` directly (the CLI via
/// `parse_notes`, the daemon via its freshness-checked `ctx.notes()` cell which
/// also calls `parse_notes`). The file is the authoritative, always-fresh
/// source — the store-cached note rows are derived FROM it at index time and
/// can lag a hand edit, so neither surface reads the store for the list. The
/// core takes the already-parsed, already-filtered `notes` plus the staleness
/// map the adapter computed when `--check` was set.
pub(crate) fn notes_list_core(
    notes: &[&cqs::note::Note],
    staleness: &std::collections::HashMap<String, Vec<String>>,
    check: bool,
) -> serde_json::Value {
    let _span = tracing::info_span!("notes_list_core", count = notes.len(), check).entered();
    let entries: Vec<NoteListEntry> = notes
        .iter()
        .map(|n| {
            let note_type = if n.is_warning() {
                "warning"
            } else if n.is_pattern() {
                "pattern"
            } else {
                "neutral"
            };
            let stale_mentions = if check {
                Some(staleness.get(&n.text).cloned().unwrap_or_default())
            } else {
                None
            };
            NoteListEntry {
                id: n.id.clone(),
                sentiment: n.sentiment,
                note_type: note_type.into(),
                sentiment_label: n.sentiment_label(),
                kind: n.kind.clone(),
                text: n.text.clone(),
                mentions: n.mentions.clone(),
                stale_mentions,
            }
        })
        .collect();
    let output = NotesListOutput {
        count: entries.len(),
        notes: entries,
    };
    serde_json::to_value(&output).unwrap_or_else(|e| {
        // to_value on a #[derive(Serialize)] struct of owned primitives cannot
        // fail in practice; degrade to an empty envelope rather than panicking
        // on the unreachable path.
        tracing::warn!(error = %e, "notes_list_core serialization failed");
        serde_json::json!({ "notes": [], "count": 0 })
    })
}

/// Notes subcommands
#[derive(clap::Subcommand)]
pub(crate) enum NotesCommand {
    /// List all notes with sentiment and mentions
    ///
    /// Flattens the shared `NotesListArgs` so the CLI and daemon batch paths
    /// can't drift — same pattern as `Commands::Search { args: SearchArgs }`.
    List {
        #[command(flatten)]
        list: crate::cli::args::NotesListArgs,
        /// Shared `--json` arg.
        #[command(flatten)]
        output: TextJsonArgs,
    },
    /// Add a note to project memory
    Add {
        /// Note text
        text: String,
        /// Sentiment (-1, -0.5, 0, 0.5, 1)
        #[arg(
            long,
            default_value = "0",
            allow_negative_numbers = true,
            value_parser = crate::cli::definitions::parse_finite_f32,
        )]
        sentiment: f32,
        /// File paths or concepts this note relates to (comma-separated)
        #[arg(long, value_delimiter = ',')]
        mentions: Option<Vec<String>>,
        /// Optional structured kind tag — `todo`, `design-decision`,
        /// `deprecation`, `known-bug`, etc. Free-string (kebab-case
        /// lowercase by convention). When set, takes precedence over
        /// `--sentiment`'s implicit "Warning:"/"Pattern:" prefix in
        /// embedding text, and enables `cqs notes list --kind <kind>`
        /// filtering. Empty string is rejected as if absent.
        #[arg(long)]
        kind: Option<String>,
        /// Skip re-indexing after adding (useful for batch operations)
        #[arg(long)]
        no_reindex: bool,
        /// Shared `--json` arg so `cqs notes add ... --json` works at the
        /// subcommand level.
        #[command(flatten)]
        output: TextJsonArgs,
    },
    /// Update an existing note (find by exact text match)
    Update {
        /// Exact text of the note to update
        text: String,
        /// New text
        #[arg(long)]
        new_text: Option<String>,
        /// New sentiment (-1, -0.5, 0, 0.5, 1)
        #[arg(
            long,
            allow_negative_numbers = true,
            value_parser = crate::cli::definitions::parse_finite_f32,
        )]
        new_sentiment: Option<f32>,
        /// New mentions (replaces all, comma-separated)
        #[arg(long, value_delimiter = ',')]
        new_mentions: Option<Vec<String>>,
        /// New kind tag. Pass an empty string to clear the kind; the
        /// trim+lowercase normalization matches `notes add`. When unset, the
        /// existing kind is preserved.
        #[arg(long)]
        new_kind: Option<String>,
        /// Skip re-indexing after update
        #[arg(long)]
        no_reindex: bool,
        /// Shared `--json` arg — see `Add` above.
        #[command(flatten)]
        output: TextJsonArgs,
    },
    /// Remove a note by exact text match
    Remove {
        /// Exact text of the note to remove
        text: String,
        /// Skip re-indexing after removal
        #[arg(long)]
        no_reindex: bool,
        /// Shared `--json` arg — see `Add` above.
        #[command(flatten)]
        output: TextJsonArgs,
    },
}

impl NotesCommand {
    /// The effective output format for this notes subcommand, resolving
    /// `--json` over the default. Every arm carries a `TextJsonArgs` group, so
    /// this is always concrete. Consumed by `Commands::effective_output_format`
    /// so the daemon-forward text-mode gate can classify `notes list` (the
    /// only daemon-dispatchable notes arm) by its own `--json` flag.
    pub(crate) fn effective_output_format(&self) -> crate::cli::definitions::OutputFormat {
        match self {
            NotesCommand::List { output, .. }
            | NotesCommand::Add { output, .. }
            | NotesCommand::Update { output, .. }
            | NotesCommand::Remove { output, .. } => output.effective_format(),
        }
    }
}

/// Handle all `notes` subcommands.
///
/// Runs in the normal Group-B dispatch path. `ctx` is optional because
/// `notes add|update|remove` must work before any index exists (a fresh
/// clone lets a user capture notes without first running `cqs init && cqs
/// index`). Mutation arms use `ctx` when available — reusing the already-open
/// project root — and fall back to `find_project_root()` otherwise. `List`
/// needs the readonly store for staleness, so it requires `ctx`.
///
/// Mutation arms open a separate read-write `Store` lazily, only when the
/// mutation actually runs, to keep list-only workloads from paying for a
/// second connection (avoid double-connecting during pure reads).
pub(crate) fn cmd_notes(
    cli: &Cli,
    ctx: Option<&crate::cli::CommandContext<'_, cqs::store::ReadOnly>>,
    subcmd: &NotesCommand,
) -> Result<()> {
    let _span = tracing::info_span!("cmd_notes").entered();
    match subcmd {
        NotesCommand::List { list, output } => {
            let ctx = ctx.ok_or_else(|| {
                anyhow::anyhow!("Index not found. Run 'cqs init && cqs index' first to list notes.")
            })?;
            cmd_notes_list(
                ctx,
                list.warnings,
                list.patterns,
                list.kind.as_deref(),
                cli.json || output.json,
                list.check,
            )
        }
        NotesCommand::Add {
            text,
            sentiment,
            mentions,
            kind,
            no_reindex,
            output,
        } => cmd_notes_add(
            cli,
            ctx,
            text,
            *sentiment,
            mentions.as_deref(),
            kind.as_deref(),
            *no_reindex,
            output.json,
        ),
        NotesCommand::Update {
            text,
            new_text,
            new_sentiment,
            new_mentions,
            new_kind,
            no_reindex,
            output,
        } => cmd_notes_update(
            cli,
            ctx,
            text,
            new_text.as_deref(),
            *new_sentiment,
            new_mentions.as_deref(),
            new_kind.as_deref(),
            *no_reindex,
            output.json,
        ),
        NotesCommand::Remove {
            text,
            no_reindex,
            output,
        } => cmd_notes_remove(cli, ctx, text, *no_reindex, output.json),
    }
}

/// Resolve the project root: reuse the readonly ctx's root when available,
/// otherwise walk up from CWD. Centralizes the `ctx.root` vs `find_project_root()`
/// fallback so the three mutation helpers stay identical.
fn resolve_root(
    ctx: Option<&crate::cli::CommandContext<'_, cqs::store::ReadOnly>>,
) -> std::path::PathBuf {
    ctx.map(|c| c.root.clone())
        .unwrap_or_else(find_project_root)
}

/// Re-parse and re-index notes after a file mutation, reusing an existing store.
fn reindex_notes(root: &std::path::Path, store: &cqs::Store) -> (usize, Option<String>) {
    let notes_path = root.join("docs/notes.toml");
    match parse_notes(&notes_path) {
        Ok(notes) if !notes.is_empty() => match cqs::index_notes(&notes, &notes_path, store) {
            Ok(count) => (count, None),
            Err(e) => (0, Some(format!("Failed to index notes: {}", e))),
        },
        Ok(_) => (0, None),
        Err(e) => (0, Some(format!("Failed to parse notes: {}", e))),
    }
}

/// Open a read-write store for notes mutations that need to reindex.
fn open_rw_store(root: &std::path::Path) -> Result<cqs::Store> {
    let index_path = cqs::resolve_index_db(&cqs::resolve_index_dir(root));
    cqs::Store::open(&index_path)
        .map_err(|e| anyhow::anyhow!("Failed to open index at {}: {}", index_path.display(), e))
}

/// Validate a note text payload: trim, reject empty/whitespace-only,
/// enforce the byte cap. Returns the trimmed text — the trimmed form is
/// what gets stored, so the exact-match lookups in update/remove (which
/// also trim both sides) stay symmetric. Rejecting whitespace-only text
/// here matters beyond hygiene: a stored whitespace-only note would trim
/// to "" and match ANY whitespace-only query in update/remove — a
/// cross-note wildcard.
///
/// `what` names the offending argument in the error ("Note text",
/// "--new-text").
fn validate_note_text<'a>(raw: &'a str, what: &str) -> Result<&'a str> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        bail!("{what} cannot be empty or whitespace-only");
    }
    if trimmed.len() > MAX_NOTE_TEXT_BYTES {
        bail!(
            "{what} too long: {} bytes (max {})",
            trimmed.len(),
            MAX_NOTE_TEXT_BYTES
        );
    }
    Ok(trimmed)
}

/// Validate a mentions list: trim entries, drop empties, enforce the count
/// and per-mention byte caps. Without these caps a single `--mentions`
/// payload could push docs/notes.toml past the file-size limit in one shot.
/// (The serialized-output check in `rewrite_notes_file` is the backstop;
/// this gives a precise error before any file I/O.) `flag` names the
/// offending argument in the error.
fn validate_mentions(raw: &[String], flag: &str) -> Result<Vec<String>> {
    let mentions: Vec<String> = raw
        .iter()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect();
    if mentions.len() > MAX_NOTE_MENTIONS {
        bail!(
            "{flag}: {} mentions exceeds the per-note limit of {}",
            mentions.len(),
            MAX_NOTE_MENTIONS
        );
    }
    if let Some(long) = mentions.iter().find(|m| m.len() > MAX_NOTE_MENTION_BYTES) {
        bail!(
            "{flag}: mention '{}' is {} bytes (max {} per mention)",
            text_preview(long),
            long.len(),
            MAX_NOTE_MENTION_BYTES
        );
    }
    Ok(mentions)
}

/// Build a text preview (first 100 chars or full text).
fn text_preview(text: &str) -> String {
    text.char_indices()
        .nth(100)
        .map(|(i, _)| format!("{}...", &text[..i]))
        .unwrap_or_else(|| text.to_string())
}

/// Ensure docs/notes.toml exists, creating it with header if needed.
fn ensure_notes_file(root: &std::path::Path) -> Result<PathBuf> {
    let notes_path = root.join("docs/notes.toml");
    if let Some(parent) = notes_path.parent() {
        std::fs::create_dir_all(parent).context("Failed to create docs directory")?;
    }
    if !notes_path.exists() {
        std::fs::write(&notes_path, NOTES_HEADER).context("Failed to create notes.toml")?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let perms = std::fs::Permissions::from_mode(0o600);
            std::fs::set_permissions(&notes_path, perms)
                .context("Failed to set notes.toml permissions")?;
        }
    }
    Ok(notes_path)
}

/// Add a note: validate text/sentiment/mentions, append to notes.toml,
/// optionally reindex. Text is stored trimmed; whitespace-only text,
/// over-cap text/mentions, and exact (post-trim) duplicates of an existing
/// note are rejected.
///
/// `output_json` accepts the subcommand-level `--json`; combined with
/// `cli.json` to honour the top-level flag too. 8 args is on clippy's
/// `too_many_arguments` boundary — same rationale as `cmd_notes_update`'s
/// allow attribute below.
#[allow(clippy::too_many_arguments)]
fn cmd_notes_add(
    cli: &Cli,
    ctx: Option<&crate::cli::CommandContext<'_, cqs::store::ReadOnly>>,
    text: &str,
    sentiment: f32,
    mentions: Option<&[String]>,
    kind: Option<&str>,
    no_reindex: bool,
    output_json: bool,
) -> Result<()> {
    // Per-subhandler span so the shared "Note operation warning" line
    // carries enough context (op + sentiment for add, op + length
    // discriminator for the others) to disambiguate add/update/remove in
    // journalctl without pulling in arg payloads.
    let _span = tracing::info_span!(
        "cmd_notes_add",
        text_len = text.len(),
        sentiment,
        kind = kind.unwrap_or(""),
        no_reindex
    )
    .entered();

    let root = resolve_root(ctx);
    // The file-write + validation lives in the shared core (also the daemon's
    // gate-safe path); the CLI adds the reindex + rendering on top.
    let core = notes_add_core(
        &root,
        &NotesAddArgs {
            text: text.to_string(),
            sentiment,
            mentions: mentions.map(<[String]>::to_vec),
            kind: kind.map(str::to_string),
        },
    )?;

    let (indexed, index_error) = reindex_after_mutation(&root, no_reindex);

    if cli.json || output_json {
        let result = NoteMutationOutput {
            status: core.status.into(),
            note_type: core.note_type.map(str::to_string),
            sentiment: core.sentiment,
            text_preview: core.text_preview.clone(),
            file: "docs/notes.toml".into(),
            indexed: indexed > 0,
            total_notes: indexed,
            index_error,
        };
        crate::cli::json_envelope::emit_json(&result)?;
    } else {
        println!(
            "Added {} (sentiment: {:+.1}): {}",
            core.note_type.unwrap_or("observation"),
            core.sentiment.unwrap_or(0.0),
            core.text_preview
        );
        if indexed > 0 {
            println!("Indexed {} notes.", indexed);
        }
        if let Some(err) = index_error {
            // Surface reindex failure on stderr so an interactive user sees
            // the same signal a `--json` consumer gets via the `index_error`
            // field.
            eprintln!("Warning: note saved but reindex failed: {}", err);
            tracing::warn!(error = %err, "Note operation warning");
        }
    }

    Ok(())
}

/// CLI-only post-mutation reindex: open a read-write store lazily and re-parse +
/// re-index `docs/notes.toml`. Returns `(indexed_count, optional_error)`.
/// Skipped when `no_reindex` is set. The daemon path does NOT call this — it
/// leaves reindexing to the watch loop (the `Store<ReadOnly>` invariant).
fn reindex_after_mutation(root: &std::path::Path, no_reindex: bool) -> (usize, Option<String>) {
    if no_reindex {
        return (0, None);
    }
    // Open read-write store lazily *only* when a mutation actually runs, so
    // list-only invocations never pay for a second connection.
    match open_rw_store(root) {
        Ok(store) => reindex_notes(root, &store),
        Err(e) => (0, Some(format!("{e}"))),
    }
}

/// Update a note: match by text, apply new text/sentiment/mentions/kind, optionally reindex.
///
/// Matching is by exact trimmed text and applies to the FIRST match in file
/// order. The CLI rejects duplicate adds, but a hand-edited notes.toml can
/// still contain duplicates — in that case only the first entry is updated;
/// rerun the command to reach the next one.
///
/// 9 args, including `output_json`. Bundling into a struct would be
/// more shape than the call site warrants — the dispatcher at `cmd_notes`
/// already destructures the same fields, and a helper struct just round-
/// trips them through one extra hop.
#[allow(clippy::too_many_arguments)]
fn cmd_notes_update(
    cli: &Cli,
    ctx: Option<&crate::cli::CommandContext<'_, cqs::store::ReadOnly>>,
    text: &str,
    new_text: Option<&str>,
    new_sentiment: Option<f32>,
    new_mentions: Option<&[String]>,
    new_kind: Option<&str>,
    no_reindex: bool,
    output_json: bool,
) -> Result<()> {
    // Per-subhandler span — see `cmd_notes_add`.
    let _span = tracing::info_span!(
        "cmd_notes_update",
        text_len = text.len(),
        new_text_len = new_text.map(str::len),
        new_sentiment,
        new_kind,
        no_reindex
    )
    .entered();

    let root = resolve_root(ctx);
    // The match + file-rewrite + validation lives in the shared core (also the
    // daemon's gate-safe path); the CLI adds the reindex + rendering on top.
    let core = notes_update_core(
        &root,
        &NotesUpdateArgs {
            text: text.to_string(),
            new_text: new_text.map(str::to_string),
            new_sentiment,
            new_mentions: new_mentions.map(<[String]>::to_vec),
            new_kind: new_kind.map(str::to_string),
        },
    )?;

    let (indexed, index_error) = reindex_after_mutation(&root, no_reindex);

    if cli.json || output_json {
        let result = NoteMutationOutput {
            status: core.status.into(),
            note_type: None,
            sentiment: None,
            text_preview: core.text_preview.clone(),
            file: "docs/notes.toml".into(),
            indexed: indexed > 0,
            total_notes: indexed,
            index_error,
        };
        crate::cli::json_envelope::emit_json(&result)?;
    } else {
        println!("Updated: {}", core.text_preview);
        if indexed > 0 {
            println!("Indexed {} notes.", indexed);
        }
        if let Some(err) = index_error {
            // See cmd_notes_add — text users need an explicit signal
            // when reindex fails, not just a tracing::warn buried in logs.
            eprintln!("Warning: note saved but reindex failed: {}", err);
            tracing::warn!(error = %err, "Note operation warning");
        }
    }

    Ok(())
}

/// Remove a note by matching its text content, optionally reindex after.
///
/// Matching is by exact trimmed text and removes the FIRST match in file
/// order — see `cmd_notes_update` for the duplicate-entry caveat with
/// hand-edited files.
fn cmd_notes_remove(
    cli: &Cli,
    ctx: Option<&crate::cli::CommandContext<'_, cqs::store::ReadOnly>>,
    text: &str,
    no_reindex: bool,
    output_json: bool,
) -> Result<()> {
    // Per-subhandler span — see `cmd_notes_add`.
    let _span =
        tracing::info_span!("cmd_notes_remove", text_len = text.len(), no_reindex).entered();

    let root = resolve_root(ctx);
    // The match + file-rewrite lives in the shared core (also the daemon's
    // gate-safe path); the CLI adds the reindex + rendering on top.
    let core = notes_remove_core(
        &root,
        &NotesRemoveArgs {
            text: text.to_string(),
        },
    )?;

    let (indexed, index_error) = reindex_after_mutation(&root, no_reindex);

    if cli.json || output_json {
        let result = NoteMutationOutput {
            status: core.status.into(),
            note_type: None,
            sentiment: None,
            text_preview: core.text_preview.clone(),
            file: "docs/notes.toml".into(),
            indexed: indexed > 0,
            total_notes: indexed,
            index_error,
        };
        crate::cli::json_envelope::emit_json(&result)?;
    } else {
        println!("Removed: {}", core.text_preview);
        if indexed > 0 {
            println!("Indexed {} notes.", indexed);
        }
        if let Some(err) = index_error {
            // See cmd_notes_add — text users need an explicit signal
            // when reindex fails, not just a tracing::warn buried in logs.
            eprintln!("Warning: note saved but reindex failed: {}", err);
            tracing::warn!(error = %err, "Note operation warning");
        }
    }

    Ok(())
}

/// List notes from docs/notes.toml
fn cmd_notes_list(
    ctx: &crate::cli::CommandContext<'_, cqs::store::ReadOnly>,
    warnings_only: bool,
    patterns_only: bool,
    kind_filter: Option<&str>,
    json: bool,
    check: bool,
) -> Result<()> {
    let root = &ctx.root;
    let notes_path = root.join("docs/notes.toml");

    if !notes_path.exists() {
        bail!("No notes file found at docs/notes.toml. Run 'cqs init' or create it manually.");
    }

    let notes = parse_notes(&notes_path)?;

    if notes.is_empty() {
        println!("No notes found.");
        return Ok(());
    }

    // Staleness check (requires store)
    let staleness: std::collections::HashMap<String, Vec<String>> = if check {
        cqs::suggest::check_note_staleness(&ctx.store, root)?
            .into_iter()
            .collect()
    } else {
        std::collections::HashMap::new()
    };

    // Filter — kind ANDs with sentiment filter (warnings/patterns).
    let kind_norm = kind_filter.and_then(|k| {
        let trimmed = k.trim().to_lowercase();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed)
        }
    });
    let filtered: Vec<_> = notes
        .iter()
        .filter(|n| {
            let sentiment_ok = if warnings_only {
                n.is_warning()
            } else if patterns_only {
                n.is_pattern()
            } else {
                true
            };
            let kind_ok = match &kind_norm {
                Some(k) => n.kind.as_deref() == Some(k.as_str()),
                None => true,
            };
            sentiment_ok && kind_ok
        })
        .collect();

    if json || ctx.cli.json {
        // Route through the shared core so the CLI and daemon emit the
        // identical union object (`{notes, count}`) — was a bare array.
        let value = notes_list_core(&filtered, &staleness, check);
        crate::cli::json_envelope::emit_json(&value)?;
        return Ok(());
    }

    // Human-readable output
    let total = notes.len();
    let warn_count = notes.iter().filter(|n| n.is_warning()).count();
    let pat_count = notes.iter().filter(|n| n.is_pattern()).count();
    let neutral_count = total - warn_count - pat_count;

    println!(
        "{} notes ({} warnings, {} patterns, {} neutral)\n",
        total, warn_count, pat_count, neutral_count
    );

    for note in &filtered {
        let sentiment_marker = format!("[{:+.1}]", note.sentiment);
        let kind_marker = note
            .kind
            .as_deref()
            .map(|k| format!(" [{}]", k))
            .unwrap_or_default();

        // Truncate text for display (char-safe)
        let preview = if note.text.chars().count() > 120 {
            let end = note
                .text
                .char_indices()
                .nth(117)
                .map(|(i, _)| i)
                .unwrap_or(note.text.len());
            format!("{}...", &note.text[..end])
        } else {
            note.text.clone()
        };

        let mentions = if note.mentions.is_empty() {
            String::new()
        } else {
            format!("  mentions: {}", note.mentions.join(", "))
        };

        print!("  {}{} {}", sentiment_marker, kind_marker, preview);
        if check {
            if let Some(stale) = staleness.get(&note.text) {
                print!("  [STALE: {}]", stale.join(", "));
            }
        }
        println!();
        if !mentions.is_empty() {
            println!("  {}", mentions);
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn note_mutation_output_add() {
        let output = NoteMutationOutput {
            status: "added".into(),
            note_type: Some("warning".into()),
            sentiment: Some(-0.5),
            text_preview: "some note text".into(),
            file: "docs/notes.toml".into(),
            indexed: true,
            total_notes: 5,
            index_error: None,
        };
        let json = serde_json::to_value(&output).unwrap();
        assert_eq!(json["status"], "added");
        assert_eq!(json["type"], "warning");
        assert_eq!(json["sentiment"], -0.5);
        assert_eq!(json["text_preview"], "some note text");
        assert_eq!(json["indexed"], true);
        assert_eq!(json["total_notes"], 5);
        assert!(json.get("index_error").is_none());
    }

    #[test]
    fn note_mutation_output_remove_no_type() {
        let output = NoteMutationOutput {
            status: "removed".into(),
            note_type: None,
            sentiment: None,
            text_preview: "deleted note".into(),
            file: "docs/notes.toml".into(),
            indexed: false,
            total_notes: 0,
            index_error: Some("store not found".into()),
        };
        let json = serde_json::to_value(&output).unwrap();
        assert_eq!(json["status"], "removed");
        assert!(json.get("type").is_none());
        assert!(json.get("sentiment").is_none());
        assert_eq!(json["index_error"], "store not found");
    }

    #[test]
    fn note_list_entry_serialization() {
        let entry = NoteListEntry {
            id: "note:0".into(),
            sentiment: -1.0,
            note_type: "warning".into(),
            sentiment_label: "WARNING",
            kind: Some("known-bug".into()),
            text: "This is broken".into(),
            mentions: vec!["search.rs".into()],
            stale_mentions: Some(vec!["old_file.rs".into()]),
        };
        let json = serde_json::to_value(&entry).unwrap();
        assert_eq!(json["id"], "note:0");
        assert_eq!(json["type"], "warning");
        assert_eq!(json["sentiment_label"], "WARNING");
        assert_eq!(json["kind"], "known-bug");
        assert_eq!(json["sentiment"], -1.0);
        assert_eq!(json["mentions"][0], "search.rs");
        assert_eq!(json["stale_mentions"][0], "old_file.rs");
    }

    #[test]
    fn note_list_entry_no_stale() {
        let entry = NoteListEntry {
            id: "note:1".into(),
            sentiment: 0.0,
            note_type: "neutral".into(),
            sentiment_label: "NOTE",
            kind: None,
            text: "just an observation".into(),
            mentions: vec![],
            stale_mentions: None,
        };
        let json = serde_json::to_value(&entry).unwrap();
        assert!(json.get("stale_mentions").is_none());
        assert!(json.get("kind").is_none());
    }

    /// `notes_list_core` emits the union envelope: an object with `notes` +
    /// `count`, each note carrying the merged field set (`id`, `type`,
    /// `sentiment_label`). Pins the data-source-agnostic schema both surfaces
    /// share.
    #[test]
    fn notes_list_core_emits_union_envelope() {
        let notes = [
            cqs::note::Note {
                id: "note:0".into(),
                text: "a warning".into(),
                sentiment: -0.5,
                mentions: vec!["search.rs".into()],
                kind: None,
            },
            cqs::note::Note {
                id: "note:1".into(),
                text: "a pattern".into(),
                sentiment: 0.5,
                mentions: vec![],
                kind: Some("design-decision".into()),
            },
        ];
        let refs: Vec<&cqs::note::Note> = notes.iter().collect();
        let staleness = std::collections::HashMap::new();
        let value = notes_list_core(&refs, &staleness, false);

        assert_eq!(value["count"], 2, "count mirrors the notes array length");
        let arr = value["notes"].as_array().expect("notes is an array");
        assert_eq!(arr.len(), 2);
        // Union: id + type + sentiment_label all present.
        assert_eq!(arr[0]["id"], "note:0");
        assert_eq!(arr[0]["type"], "warning");
        assert_eq!(arr[0]["sentiment_label"], "WARNING");
        assert_eq!(arr[1]["type"], "pattern");
        assert_eq!(arr[1]["sentiment_label"], "PATTERN");
        assert_eq!(arr[1]["kind"], "design-decision");
        // stale_mentions absent without --check.
        assert!(arr[0].get("stale_mentions").is_none());
    }

    /// With `--check`, every note carries `stale_mentions` (present even when
    /// empty), matching the prior CLI/daemon contract.
    #[test]
    fn notes_list_core_check_emits_stale_mentions() {
        let notes = [cqs::note::Note {
            id: "note:0".into(),
            text: "a note".into(),
            sentiment: 0.0,
            mentions: vec![],
            kind: None,
        }];
        let refs: Vec<&cqs::note::Note> = notes.iter().collect();
        let mut staleness = std::collections::HashMap::new();
        staleness.insert("a note".to_string(), vec!["gone.rs".to_string()]);
        let value = notes_list_core(&refs, &staleness, true);
        let arr = value["notes"].as_array().unwrap();
        assert_eq!(arr[0]["stale_mentions"][0], "gone.rs");
    }
}
