//! Init command for cqs
//!
//! Creates .cqs/ directory and downloads the embedding model.

use anyhow::{Context, Result};

use cqs::Embedder;

use crate::cli::{find_project_root, Cli};

/// Initialize cqs in a project directory
/// Creates `.cqs/` directory, downloads the embedding model, and warms up the embedder.
pub(crate) fn cmd_init(cli: &Cli, json: bool) -> Result<()> {
    let _span = tracing::info_span!("cmd_init").entered();
    let root = find_project_root();
    let cqs_dir = root.join(cqs::INDEX_DIR);

    // P2.12: when --json is set, suppress human progress prints to stderr-or-skip
    // and emit a single envelope summarizing the result on success. The global
    // `cli.json` and the local `--json` both honor it.
    let want_json = cli.json || json;
    let quiet = cli.quiet || want_json;

    if !quiet {
        println!("Initializing cqs...");
    }

    // Create .cqs directory
    std::fs::create_dir_all(&cqs_dir).context("Failed to create .cqs directory")?;

    // Set restrictive permissions on .cqs directory (Unix only)
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Err(e) = std::fs::set_permissions(&cqs_dir, std::fs::Permissions::from_mode(0o700)) {
            tracing::debug!(error = %e, "Failed to set .cqs directory permissions");
        }
    }

    // Create .gitignore
    // PB-V1.29-4: Windows git with `core.autocrlf=true` renders LF-only files
    // as modified. Use CRLF on Windows so the initial commit stays quiet.
    let gitignore = cqs_dir.join(".gitignore");
    #[cfg(windows)]
    let gitignore_contents = "index.db\r\nindex.db-wal\r\nindex.db-shm\r\nindex.lock\r\nindex.hnsw.graph\r\nindex.hnsw.data\r\nindex.hnsw.ids\r\nindex.hnsw.checksum\r\nindex.hnsw.lock\r\n*.tmp\r\n";
    #[cfg(not(windows))]
    let gitignore_contents = "index.db\nindex.db-wal\nindex.db-shm\nindex.lock\nindex.hnsw.graph\nindex.hnsw.data\nindex.hnsw.ids\nindex.hnsw.checksum\nindex.hnsw.lock\n*.tmp\n";
    std::fs::write(&gitignore, gitignore_contents).context("Failed to create .gitignore")?;

    // Download model
    if !quiet {
        // EX-V1.29-6: Read the exact preset-declared download size instead of
        // the old `dim >= 1024 ? "~1.3GB" : "~547MB"` heuristic. Custom models
        // (user-supplied repo) carry `None` and surface as "(size unknown)"
        // rather than silently misreporting a preset's number.
        let size = match cli.try_model_config()?.approx_download_bytes {
            Some(bytes) => format_download_size(bytes),
            None => "(size unknown)".to_string(),
        };
        println!("Downloading model ({size})...");
    }

    let embedder =
        Embedder::new(cli.try_model_config()?.clone()).context("Failed to initialize embedder")?;

    if !quiet {
        println!("Detecting hardware... {}", embedder.provider());
    }

    // Warm up
    embedder.warm()?;

    if !quiet {
        println!("Created .cqs/");
        println!();
        println!("Run 'cqs index' to index your codebase.");
    }

    if want_json {
        let model_name = cli
            .try_model_config()
            .map(|c| c.name.clone())
            .unwrap_or_default();
        let obj = serde_json::json!({
            "initialized": true,
            "cqs_dir": cqs_dir.display().to_string(),
            "model": model_name,
        });
        crate::cli::json_envelope::emit_json(&obj)?;
    }

    Ok(())
}

/// EX-V1.29-6: render bytes as GB or MB with one decimal, matching the
/// legacy heuristic output ("~1.3GB" / "~547MB"). GB kicks in at 1 GiB.
fn format_download_size(bytes: u64) -> String {
    const MIB: u64 = 1024 * 1024;
    const GIB: u64 = 1024 * MIB;
    if bytes >= GIB {
        format!("~{:.1}GB", bytes as f64 / GIB as f64)
    } else {
        format!("~{}MB", bytes.div_ceil(MIB))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_download_size_bge_large_renders_as_gb() {
        // BGE-large shipped value: 1300 MiB.
        let s = format_download_size(1_300 * 1024 * 1024);
        assert_eq!(s, "~1.3GB");
    }

    #[test]
    fn format_download_size_e5_base_renders_as_mb() {
        let s = format_download_size(547 * 1024 * 1024);
        assert_eq!(s, "~547MB");
    }

    #[test]
    fn format_download_size_sub_mib_rounds_up_not_zero() {
        // Small custom export: ensure the MB rendering never produces "~0MB".
        let s = format_download_size(1024);
        assert_eq!(s, "~1MB");
    }
}
