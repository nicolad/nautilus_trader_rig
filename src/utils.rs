use serde::{Deserialize, Serialize};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use tracing::info;
use walkdir::WalkDir;

/// A simple data struct for storing code snippet chunks.
#[derive(Clone, Debug, Serialize, Deserialize, Eq, PartialEq)]
pub struct CodeChunk {
    pub id: String,
    pub content: String,
    pub language: String,
    pub file_path: String,
}

/// A helper function to walk a single directory and collect code snippets,
/// chunking larger files into multiple `CodeChunk`s.
pub fn collect_snippets_from_dir(
    base_path: &Path,
    extension_filter: &[&str],
    lang_label: &str,
) -> Vec<CodeChunk> {
    let mut code_snippets = Vec::new();

    // Walk the directory structure, following symlinks.
    for entry in WalkDir::new(base_path)
        .follow_links(true)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        let path = entry.path();

        // We only care about actual files, not directories, symlinks, etc.
        if !path.is_file() {
            continue;
        }

        let path_str = path.to_string_lossy().to_lowercase();
        let mut recognized = false;
        for ext in extension_filter {
            if path_str.ends_with(ext) {
                recognized = true;
                break;
            }
        }

        if !recognized {
            // Not one of our target extensions.
            continue;
        }

        // Attempt to read file contents
        match fs::read_to_string(path) {
            Ok(file_str) => {
                let file_path_string = path.to_string_lossy().to_string();
                info!(
                    "Processing file: {} for language: {}",
                    file_path_string, lang_label
                );

                // Chunk the file into smaller pieces to keep embeddings short.
                const MAX_LINES_PER_CHUNK: usize = 300;
                let lines: Vec<&str> = file_str.lines().collect();
                let total_lines = lines.len();
                let mut chunk_start = 0;

                while chunk_start < total_lines {
                    let chunk_end = std::cmp::min(chunk_start + MAX_LINES_PER_CHUNK, total_lines);
                    let chunk_slice = &lines[chunk_start..chunk_end];
                    let chunk_content = chunk_slice.join("\n");

                    let chunk_id = format!(
                        "FS::{}::chunk_{}_{}",
                        file_path_string, chunk_start, chunk_end
                    );

                    code_snippets.push(CodeChunk {
                        id: chunk_id,
                        content: chunk_content,
                        language: lang_label.to_string(),
                        file_path: file_path_string.clone(),
                    });

                    chunk_start = chunk_end;
                }
            }
            Err(e) => {
                info!("Skipping file due to read error: {} => {}", path.display(), e);
            }
        }
    }

    code_snippets
}

/// Collects snippets from both the Rust indicators directory and the
/// Python/Cython indicators directory.
pub fn collect_all_snippets() -> Vec<CodeChunk> {
    let mut all_snippets = Vec::new();


    // Python/Cython indicators (search for .py, .pyx, .pxd files)
    all_snippets.extend(collect_snippets_from_dir(
        Path::new("nautilus_trader/nautilus_trader/indicators"),
        &[".py", ".pyx", ".pxd"],
        "cython_python",
    ));

    // Rust indicators (search for .rs files)
    all_snippets.extend(collect_snippets_from_dir(
        Path::new("nautilus_trader/crates/indicators"),
        &[".rs"],
        "rust",
    ));


    all_snippets
}

/// A simple record for our CSV output.
#[derive(Debug, Serialize)]
struct IndicatorRecord {
    filename: String,
    indicator_name: String,
    extension: String,
}

/// Write the collected Rust/Cython indicators to `indicators.csv`.
///
/// This function:
/// 1) Gathers all code snippets (Rust + Python/Cython).
/// 2) Assigns the 'indicator_name' based on the file’s basename (sans extension).
/// 3) Distinguishes extension = "rust", "cython", or "python".
/// 4) Writes the CSV file with columns: filename, indicator_name, extension.
pub fn save_indicators_csv() -> io::Result<()> {
    // Collect the code snippets.
    let snippets = collect_all_snippets();

    // Prepare CSV writer.
    let mut wtr = csv::Writer::from_path("indicators.csv")?;

    // Write headers: filename,indicator_name,extension
    wtr.write_record(&["filename", "indicator_name", "extension"])?;

    // Convert each snippet into an IndicatorRecord
    for snippet in snippets {
        // Derive an extension label
        //   .rs     => "rust"
        //   .pyx,
        //   .pxd    => "cython"
        //   .py     => "python"
        let extension_label = if snippet.file_path.ends_with(".rs") {
            "rust"
        } else if snippet.file_path.ends_with(".pyx")
            || snippet.file_path.ends_with(".pxd")
        {
            "cython"
        } else if snippet.file_path.ends_with(".py") {
            "python"
        } else {
            // fallback (shouldn't happen if filters are correct)
            "unknown"
        };

        // Derive a “naive” indicator_name from the filename, minus extension
        let path = PathBuf::from(&snippet.file_path);
        let filename_only = path.file_name().unwrap_or_default().to_string_lossy();
        let indicator_name = filename_only
            .rsplit_once('.')
            .map(|(base, _)| base)
            .unwrap_or(&filename_only)
            .to_string();

        let record = IndicatorRecord {
            filename: snippet.file_path.clone(),
            indicator_name,
            extension: extension_label.to_string(),
        };

        wtr.serialize(record)?;
    }

    // Finish writing
    wtr.flush()?;
    Ok(())
}

