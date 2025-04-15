use anyhow::Result;
use dotenv::dotenv;
use git2::{ObjectType, Repository, TreeWalkMode, TreeWalkResult};
use regex::Regex;
use rig::{
    completion::Prompt,
    embeddings::{Embed, EmbedError, EmbeddingsBuilder, TextEmbedder},
    providers::openai::{Client, TEXT_EMBEDDING_ADA_002},
};
use rig_sqlite::{Column, ColumnValue, SqliteVectorStore, SqliteVectorStoreTable};
use rusqlite::ffi::sqlite3_auto_extension;
use serde::{Deserialize, Serialize};
use sqlite_vec::sqlite3_vec_init;
use std::{
    collections::HashSet,
    env,
    fs::File,
    io::{BufWriter, Write},
};
use tokio_rusqlite::Connection;
use tracing::info;

#[derive(Clone, Debug, Serialize, Deserialize, Eq, PartialEq)]
struct CodeChunk {
    id: String,
    content: String,
    language: String,
    file_path: String,
}

// Implement the Embed trait so rig can build embeddings
impl Embed for CodeChunk {
    fn embed(&self, embedder: &mut TextEmbedder) -> std::result::Result<(), EmbedError> {
        embedder.embed(self.content.clone());
        Ok(())
    }
}

// Implement SqliteVectorStoreTable so we can store CodeChunk easily
impl SqliteVectorStoreTable for CodeChunk {
    fn name() -> &'static str {
        "code_chunks"
    }

    fn schema() -> Vec<rig_sqlite::Column> {
        vec![
            Column::new("id", "TEXT PRIMARY KEY"),
            Column::new("content", "TEXT"),
            Column::new("language", "TEXT"),
            Column::new("file_path", "TEXT"),
        ]
    }

    fn id(&self) -> String {
        self.id.clone()
    }

    fn column_values(&self) -> Vec<(&'static str, Box<dyn ColumnValue>)> {
        vec![
            ("id", Box::new(self.id.clone())),
            ("content", Box::new(self.content.clone())),
            ("language", Box::new(self.language.clone())),
            ("file_path", Box::new(self.file_path.clone())),
        ]
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize logging
    tracing_subscriber::fmt()
        .with_target(false)
        .with_thread_ids(true)
        .with_thread_names(true)
        .with_level(true)
        .init();

    dotenv().ok();
    info!("Starting code snippet collection and embedding with OpenAI");

    // 1. Open local Git repo
    let repo_path = "./nautilus_trader";
    info!("Attempting to open repository at: {}", repo_path);
    let repo = Repository::open(repo_path)?;
    info!("Repository opened successfully: {:?}", repo_path);

    // 2. Get the latest commit from the local 'develop' branch
    info!("Looking for branch: 'develop'");
    let branch = repo.find_branch("develop", git2::BranchType::Local)?;
    let commit = branch.get().peel_to_commit()?;
    let tree = commit.tree()?;
    info!("Got tree from latest commit: {}", commit.id());

    // We'll store code snippets from any .py/.pyx/.pxd/.rs/.r file in the entire repo
    let mut code_snippets = Vec::new();
    // We'll also store discovered indicator definitions in a CSV
    let mut indicators = Vec::new();

    // Regex for Python-based indicators, e.g. "class Foo(Indicator):"
    let re_python_indicator =
        Regex::new(r"(?i)^\s*class\s+([A-Za-z_]\w*)\s*\(\s*Indicator\s*\)\s*:").unwrap();

    // Regex for Cython-based indicators, e.g. "cdef class Foo(Indicator):"
    let re_cython_indicator =
        Regex::new(r"(?i)^\s*cdef\s+class\s+([A-Za-z_]\w*)\s*\(\s*Indicator\s*\)\s*:").unwrap();

    // Regex for Rust-based indicators, e.g. "pub struct FooIndicator {"
    let re_rust_indicator =
        Regex::new(r"(?i)pub\s+struct\s+([A-Za-z_]\w*Indicator)\s*\{").unwrap();

    // We'll define a closure that processes entries in the git tree
    // and builds up the code_snippets + indicators vectors.
    let walker_callback = |root: &str, entry: &git2::TreeEntry| {
        info!(
            "Examining tree entry: root=\"{}\", name=\"{:?}\"",
            root,
            entry.name().unwrap_or_default()
        );

        let Ok(obj) = entry.to_object(&repo) else {
            info!("Skipping entry: couldn't convert to object => {:?}", entry.name());
            return TreeWalkResult::Ok;
        };

        match obj.kind() {
            Some(ObjectType::Tree) => {
                // Sub-tree => descend
                let dir_name = entry.name().unwrap_or_default();
                let full_dir_path = format!("{}{}", root, dir_name);
                info!("Descending into subfolder: {full_dir_path}");
                TreeWalkResult::Ok
            }
            Some(ObjectType::Blob) => {
                let file_name = entry.name().unwrap_or_default();
                let file_path = format!("{}{}", root, file_name);
                info!("Found file: {file_path}");

                let file_path_lower = file_path.to_lowercase();
                let extension = if file_path_lower.ends_with(".py") {
                    "python"
                } else if file_path_lower.ends_with(".pyx") || file_path_lower.ends_with(".pxd") {
                    "cython"
                } else if file_path_lower.ends_with(".rs") || file_path_lower.ends_with(".r") {
                    "rust"
                } else {
                    "unknown"
                };

                info!("File \"{}\" extension detection => \"{}\"", file_path, extension);

                if extension == "unknown" {
                    info!("Skipping unknown extension: {file_path}");
                    return TreeWalkResult::Ok;
                }

                // Attempt to retrieve the blob content
                if let Some(blob) = obj.as_blob() {
                    match std::str::from_utf8(blob.content()) {
                        Ok(file_str) => {
                            // ---- INDICATOR DETECTION ----
                            // We'll search the entire file for indicator definitions
                            let lines: Vec<&str> = file_str.lines().collect();
                            let mut found_any_match = false;

                            match extension {
                                "python" => {
                                    for (i, line) in lines.iter().enumerate() {
                                        if let Some(cap) = re_python_indicator.captures(line) {
                                            let indicator_name = cap.get(1).unwrap().as_str();
                                            indicators.push((
                                                file_path.clone(),
                                                indicator_name.to_string(),
                                                extension.to_string(),
                                            ));
                                            info!(
                                                "  [MATCH] line {} => Found Python indicator: {}",
                                                i, indicator_name
                                            );
                                            found_any_match = true;
                                        }
                                    }
                                }
                                "cython" => {
                                    for (i, line) in lines.iter().enumerate() {
                                        if let Some(cap) = re_cython_indicator.captures(line) {
                                            let indicator_name = cap.get(1).unwrap().as_str();
                                            indicators.push((
                                                file_path.clone(),
                                                indicator_name.to_string(),
                                                extension.to_string(),
                                            ));
                                            info!(
                                                "  [MATCH] line {} => Found Cython indicator: {}",
                                                i, indicator_name
                                            );
                                            found_any_match = true;
                                        }
                                    }
                                }
                                "rust" => {
                                    info!("Scanning lines for Rust indicators in: {}", file_path);
                                    for (i, line) in lines.iter().enumerate() {
                                        if let Some(cap) = re_rust_indicator.captures(line) {
                                            let indicator_name = cap.get(1).unwrap().as_str();
                                            indicators.push((
                                                file_path.clone(),
                                                indicator_name.to_string(),
                                                extension.to_string(),
                                            ));
                                            info!(
                                                "  [MATCH] line {} => Found Rust indicator: {}",
                                                i, indicator_name
                                            );
                                            found_any_match = true;
                                        }
                                    }
                                }
                                _ => {
                                    // Shouldn't happen, but just in case
                                }
                            }

                            if !found_any_match {
                                info!("  No indicator matches found in {}", file_path);
                            }

                            // ---- CHUNKING FOR EMBEDDING ----
                            // We'll chunk the file text in smaller pieces to avoid
                            // the 8k token limit in text-embedding-ada-002.
                            const MAX_LINES_PER_CHUNK: usize = 300;
                            let total_lines = lines.len();
                            let mut chunk_start = 0;

                            while chunk_start < total_lines {
                                let chunk_end =
                                    std::cmp::min(chunk_start + MAX_LINES_PER_CHUNK, total_lines);

                                let chunk_slice = &lines[chunk_start..chunk_end];
                                let chunk_content = chunk_slice.join("\n");

                                let chunk_id = format!(
                                    "{}::{}::chunk_{}_{}",
                                    commit.id(),
                                    file_path,
                                    chunk_start,
                                    chunk_end
                                );

                                code_snippets.push(CodeChunk {
                                    id: chunk_id,
                                    content: chunk_content,
                                    language: extension.to_string(),
                                    file_path: file_path.clone(),
                                });

                                chunk_start = chunk_end;
                            }

                            info!("Processed file: {}, lines: {}, total chunks: {}",
                                  file_path,
                                  total_lines,
                                  (total_lines as f64 / MAX_LINES_PER_CHUNK as f64).ceil()
                            );
                        }
                        Err(e) => {
                            info!("Skipping file (UTF-8 error): {} => {}", file_path, e);
                        }
                    }
                } else {
                    info!("Object is not a blob: {file_path}");
                }
                TreeWalkResult::Ok
            }
            other => {
                // Could be a commit, tag, etc. We'll skip
                info!("Skipping object of type {:?} at entry: {:?}", other, entry.name());
                TreeWalkResult::Ok
            }
        }
    };

    // Now perform the actual recursive walk of the tree
    info!("Starting recursive tree walk...");
    tree.walk(TreeWalkMode::PreOrder, walker_callback)?;

    info!("Collected {} code snippets (including chunked).", code_snippets.len());
    info!(
        "Discovered a total of {} potential indicators.",
        indicators.len()
    );

    // Write discovered indicators to indicators.csv
    {
        let mut csv_file = BufWriter::new(File::create("indicators.csv")?);
        writeln!(csv_file, "filename,indicator_name,extension")?;
        for (path, name, ext) in &indicators {
            writeln!(csv_file, "{},{},{}", path, name, ext)?;
        }
        info!("Wrote {} indicators to indicators.csv", indicators.len());
    }

    // 4. We'll embed in **smaller batches** to avoid invalid request size errors.
    let openai_api_key =
        env::var("OPENAI_API_KEY").expect("Expected OPENAI_API_KEY in environment variables");
    let openai_client = Client::new(&openai_api_key);
    let model = openai_client.embedding_model(TEXT_EMBEDDING_ADA_002);

    info!("Building embeddings in smaller batches with OpenAI (text-embedding-ada-002)");

    // 5. Initialize `sqlite-vec`
    unsafe {
        sqlite3_auto_extension(Some(std::mem::transmute(sqlite3_vec_init as *const ())));
    }

    // 6. Create or open your SQLite DB
    let conn = Connection::open("code_chunks_vector_store.db").await?;
    let vector_store = SqliteVectorStore::new(conn.clone(), &model).await?;

    // 7. Gather existing IDs in the table
    use rusqlite::params;
    let existing_ids: HashSet<String> = conn
        .call(|inner_conn| {
            let mut stmt = inner_conn.prepare("SELECT id FROM code_chunks")?;
            let rows = stmt.query_map(params![], |row| row.get::<_, String>(0))?;
            let mut found = HashSet::new();
            for row_result in rows {
                let the_id = row_result?;
                found.insert(the_id);
            }
            Ok(found)
        })
        .await?;

    info!("Found {} snippets already in the DB", existing_ids.len());

    // Filter out duplicates
    let new_snippets: Vec<CodeChunk> = code_snippets
        .into_iter()
        .filter(|s| !existing_ids.contains(&s.id))
        .collect();

    info!("{} new code snippets to store", new_snippets.len());

    // ---- BATCHING LOGIC ----
    const BATCH_SIZE: usize = 50;
    let mut total_embedded = 0;
    if new_snippets.is_empty() {
        info!("No new snippets to embed. Skipping embedding step.");
    } else {
        info!("Beginning embedding in batches of {} documents each.", BATCH_SIZE);
    }

    for (batch_index, chunk) in new_snippets.chunks(BATCH_SIZE).enumerate() {
        info!(
            "Embedding batch #{} with {} documents...",
            batch_index + 1,
            chunk.len()
        );

        let mut builder = EmbeddingsBuilder::new(model.clone());
        for snippet in chunk {
            builder = builder.document(snippet.clone())?;
        }

        let embeddings = builder.build().await?;
        info!(
            "Batch #{} embedded successfully, storing in DB...",
            batch_index + 1
        );

        vector_store.add_rows(embeddings).await?;
        total_embedded += chunk.len();
    }

    info!("All batches completed. {} new documents stored.", total_embedded);

    // 9. Create a vector index on our store
    let index = vector_store.index(model.clone());
    info!("Vector store indexed. Ready for queries.");

    // 10. Use RAG to generate a table comparing Python vs. Rust indicators
    let rag_agent = openai_client
        .agent("gpt-4")
        .preamble("
            You are an assistant that compares the Rust implementation of indicators
            against the Python/Cython implementation. We want a single Markdown table
            with three columns:
              1) 'Indicator'
              2) 'Rust Matches Python?'
              3) 'Rust Test Coverage >= Python?'
            For each relevant indicator, put '✅' if true, '❌' if false.
            If there is incomplete information to decide, you may guess based on partial data.
            Format it as valid Markdown.
        ")
        .dynamic_context(1, index)
        .build();

    let comparison_table = rag_agent
        .prompt("Produce a table comparing all discovered Python vs. Rust indicators for parity and test coverage.")
        .await?;

    println!("Comparison table:\n{}", comparison_table);

    // 11. Write the model's table to a README_comparison.md
    {
        let mut comparison_md = File::create("README_comparison.md")?;
        writeln!(comparison_md, "{}", comparison_table)?;
        info!("Wrote the model's comparison table to README_comparison.md");
    }

    // 12. Write out a .txt file summarizing snippet IDs
    {
        let mut file = File::create("collected_code_chunks.txt")?;
        writeln!(file, "Wrote a summary of code snippets stored in the DB:\n")?;

        let all_ids: Vec<String> = conn
            .call(|inner_conn| {
                let mut stmt = inner_conn.prepare("SELECT id FROM code_chunks")?;
                let rows = stmt.query_map(params![], |row| row.get::<_, String>(0))?;
                let mut v = Vec::new();
                for row_result in rows {
                    v.push(row_result?);
                }
                Ok(v)
            })
            .await?;

        for doc_id in all_ids {
            writeln!(file, "{}", doc_id)?;
        }
        info!("Wrote a basic summary to collected_code_chunks.txt");
    }

    // 13. Optionally keep the “collected_code_chunks.md”
    {
        let mut md_file = File::create("collected_code_chunks.md")?;
        writeln!(md_file, "# Collected Code Chunks\n")?;
        writeln!(
            md_file,
            "Below is a list of code snippet IDs stored in the database:\n"
        )?;

        let all_ids: Vec<String> = conn
            .call(|inner_conn| {
                let mut stmt = inner_conn.prepare("SELECT id FROM code_chunks")?;
                let rows = stmt.query_map(params![], |row| row.get::<_, String>(0))?;
                let mut v = Vec::new();
                for row_result in rows {
                    v.push(row_result?);
                }
                Ok(v)
            })
            .await?;

        for doc_id in all_ids {
            writeln!(md_file, "- `{}`", doc_id)?;
        }
        info!("Wrote a basic summary to collected_code_chunks.md");
    }

    info!("Embedding, comparison, and storage process completed successfully");
    Ok(())
}
