use anyhow::Result;
use dotenv::dotenv;
use git2::{ObjectType, Repository, TreeWalkMode, TreeWalkResult};
use regex::Regex;
use rig::{
    embeddings::{Embed, EmbedError, EmbeddingsBuilder, TextEmbedder},
    providers::openai::{Client, TEXT_EMBEDDING_ADA_002},
};
use rig::completion::Prompt;
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
    let repo = Repository::open(repo_path)?;
    info!("Repository opened successfully: {:?}", repo_path);

    // 2. Get the latest commit from the local 'develop' branch
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

    // Walk the entire repo tree recursively
    let walker_callback = |root: &str, entry: &git2::TreeEntry| {
        let Ok(obj) = entry.to_object(&repo) else {
            return TreeWalkResult::Ok;
        };

        match obj.kind() {
            Some(ObjectType::Tree) => {
                // It's a sub-tree (directory). Keep walking in PreOrder.
                let dir_name = entry.name().unwrap_or_default();
                info!("Descending into subfolder: {}{}", root, dir_name);
                TreeWalkResult::Ok
            }
            Some(ObjectType::Blob) => {
                // It's a file. Let’s see if it's .py / .pyx / .pxd / .rs / .r
                let file_name = entry.name().unwrap_or_default();
                let file_path = format!("{}{}", root, file_name);

                // If you only need .rs, remove the .r check
                let extension = if file_path.ends_with(".py") {
                    "python"
                } else if file_path.ends_with(".pyx") || file_path.ends_with(".pxd") {
                    "cython"
                } else if file_path.ends_with(".rs") || file_path.ends_with(".r") {
                    "rust"
                } else {
                    "unknown"
                };

                if extension != "unknown" {
                    if let Some(blob) = obj.as_blob() {
                        if let Ok(file_str) = std::str::from_utf8(blob.content()) {
                            // We store the entire snippet for embedding
                            code_snippets.push(CodeChunk {
                                id: format!("{}::{}", commit.id(), file_path),
                                content: file_str.to_string(),
                                language: extension.to_string(),
                                file_path: file_path.clone(),
                            });

                            // Now search for indicator definitions
                            let lines: Vec<&str> = file_str.lines().collect();
                            info!(
                                "Processing file: {}, extension: {}, lines: {}",
                                file_path, extension, lines.len()
                            );
                            let mut found_any_match = false;

                            match extension {
                                "python" => {
                                    // Python: look for "class X(Indicator):"
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
                                                i,
                                                indicator_name
                                            );
                                            found_any_match = true;
                                        }
                                    }
                                }
                                "cython" => {
                                    // Cython: "cdef class X(Indicator):"
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
                                                i,
                                                indicator_name
                                            );
                                            found_any_match = true;
                                        }
                                    }
                                }
                                "rust" => {
                                    // Rust: "pub struct XIndicator {"
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
                                                i,
                                                indicator_name
                                            );
                                            found_any_match = true;
                                        }
                                    }
                                }
                                _ => {}
                            }

                            if !found_any_match {
                                info!("  No indicator matches found in {}", file_path);
                            }
                        }
                    }
                }
                TreeWalkResult::Ok
            }
            _ => TreeWalkResult::Ok,
        }
    };

    tree.walk(TreeWalkMode::PreOrder, walker_callback)?;

    info!("Collected {} code snippets", code_snippets.len());
    info!("Discovered a total of {} potential indicators", indicators.len());

    // Write discovered indicators to indicators.csv
    {
        let mut csv_file =
            BufWriter::new(File::create("indicators.csv").expect("create indicators.csv failed"));
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
                found.insert(row_result?);
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

    // ---- BATCHING LOGIC HERE ----
    const BATCH_SIZE: usize = 50;
    let mut total_embedded = 0;

    for chunk in new_snippets.chunks(BATCH_SIZE) {
        let mut builder = EmbeddingsBuilder::new(model.clone());
        for snippet in chunk {
            builder = builder.document(snippet.clone())?;
        }
        // Build embeddings for this subset
        let embeddings = builder.build().await?;
        info!("Batch of {} docs embedded successfully. Storing in DB...", chunk.len());

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
