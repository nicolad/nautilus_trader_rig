use anyhow::{anyhow, Result};
use dotenv::dotenv;
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
    fs::{self, File},
    io::{BufReader, Write},
    path::Path,
};
use tokio_rusqlite::Connection;
use tracing::info;

#[derive(Debug, Deserialize, Serialize, Clone)]
struct IndicatorRow {
    filename: String,
    indicator_name: String,
    extension: String,
    // This column might not exist in older CSVs, so we make it optional
    #[serde(default)]
    embedded: bool,
}

/// Represents a “chunk” of code to be embedded.
#[derive(Clone, Debug)]
pub struct CodeChunk {
    pub id: String,
    pub content: String,
    pub language: String,
    pub file_path: String,
}

impl CodeChunk {
    pub fn new(file_path: &str, content: &str, language: &str) -> Self {
        // For uniqueness, you might combine path + hash, or something else
        // Simple approach: path + language
        let id = format!("{}-{}", file_path, language);
        CodeChunk {
            id,
            content: content.to_string(),
            language: language.to_string(),
            file_path: file_path.to_string(),
        }
    }
}

/// Implement the `Embed` trait so `rig` can build embeddings from `CodeChunk`.
impl Embed for CodeChunk {
    fn embed(&self, embedder: &mut TextEmbedder) -> Result<(), EmbedError> {
        embedder.embed(self.content.clone());
        Ok(())
    }
}

/// Implement `SqliteVectorStoreTable` so we can store `CodeChunk` in a SQLite table.
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
    // ------------------------------------------------------------------------
    // 0. Initialize logging and .env
    // ------------------------------------------------------------------------
    tracing_subscriber::fmt()
        .with_target(false)
        .with_thread_names(true)
        .with_level(true)
        .init();

    dotenv().ok(); // Load .env if present
    info!("Starting the embedding process using indicators.csv as a tracker...");

    // ------------------------------------------------------------------------
    // 1. Load the CSV into memory, parse into a list of IndicatorRow
    // ------------------------------------------------------------------------
    let csv_path = "indicators.csv";
    let mut rows = load_indicators_csv(csv_path)?;

    // ------------------------------------------------------------------------
    // 2. Prepare OpenAI embedding model
    // ------------------------------------------------------------------------
    let openai_api_key =
        env::var("OPENAI_API_KEY").expect("Expected OPENAI_API_KEY in environment variables");
    let openai_client = Client::new(&openai_api_key);
    let model = openai_client.embedding_model(TEXT_EMBEDDING_ADA_002);

    // ------------------------------------------------------------------------
    // 3. Setup sqlite-vec for vector storage
    // ------------------------------------------------------------------------
    unsafe {
        sqlite3_auto_extension(Some(std::mem::transmute(sqlite3_vec_init as *const ())));
    }

    // ------------------------------------------------------------------------
    // 4. Create/open your SQLite DB
    // ------------------------------------------------------------------------
    let conn = Connection::open("code_chunks_vector_store.db").await?;
    let vector_store = SqliteVectorStore::new(conn.clone(), &model).await?;

    // ------------------------------------------------------------------------
    // 5. Gather all existing snippet IDs from DB, so we also skip duplicates
    // ------------------------------------------------------------------------
    let existing_ids = fetch_existing_ids(&conn).await?;
    info!(
        "Currently have {} code snippet(s) in the DB. Will not re-embed those.",
        existing_ids.len()
    );

    // ------------------------------------------------------------------------
    // 6. For each row in the CSV, if `embedded` is false, read the file,
    //    build a CodeChunk, and embed it. Then update that CSV row to `embedded = true`.
    // ------------------------------------------------------------------------
    let mut to_embed: Vec<CodeChunk> = Vec::new();
    let mut changed_any = false;

    // We collect everything to embed first, then do it in batches
    for row in rows.iter_mut() {
        // Already embedded per CSV? Skip
        if row.embedded {
            continue;
        }

        // Try reading the file for content
        let file_content = match read_file_contents(&row.filename) {
            Ok(c) => c,
            Err(e) => {
                eprintln!(
                    "Warning: Could not read file {} for indicator {}: {:#}",
                    row.filename, row.indicator_name, e
                );
                continue;
            }
        };

        // Build code snippet
        let language = row.extension.clone(); // "rust", "cython", "python", etc.
        let snippet = CodeChunk::new(&row.filename, &file_content, &language);

        // Check if snippet ID is already in DB
        if existing_ids.contains(&snippet.id) {
            info!(
                "Snippet with ID = {} is already in DB; marking embedded in CSV without re-embedding.",
                snippet.id
            );
            row.embedded = true;
            changed_any = true;
            continue;
        }

        // Otherwise, we'll embed
        to_embed.push(snippet);
    }

    // ------------------------------------------------------------------------
    // 7. Now embed everything in `to_embed` in batches
    // ------------------------------------------------------------------------
    const BATCH_SIZE: usize = 50;
    let total_new = to_embed.len();
    if total_new == 0 {
        info!("No new code snippets to embed.");
    } else {
        info!(
            "{} snippet(s) need embedding. Embedding in batches of {}.",
            total_new, BATCH_SIZE
        );
    }

    let mut total_embedded = 0;
    for (batch_index, chunk_group) in to_embed.chunks(BATCH_SIZE).enumerate() {
        info!(
            "Embedding batch #{} with {} documents...",
            batch_index + 1,
            chunk_group.len()
        );

        let mut builder = EmbeddingsBuilder::new(model.clone());
        for snippet in chunk_group {
            builder = builder.document(snippet.clone())?;
        }

        let embeddings = builder.build().await?;
        info!(
            "Batch #{} embedded successfully, storing in DB...",
            batch_index + 1
        );

        vector_store.add_rows(embeddings).await?;
        total_embedded += chunk_group.len();

        // Mark them embedded in memory
        for snippet in chunk_group {
            // Find the row in `rows` that matches snippet's file path
            if let Some(ind_row) = rows
                .iter_mut()
                .find(|r| r.filename == snippet.file_path && r.embedded == false)
            {
                ind_row.embedded = true;
                changed_any = true;
            }
        }
    }

    info!(
        "All batches completed. {} new documents stored. Updating CSV if needed...",
        total_embedded
    );

    // ------------------------------------------------------------------------
    // 8. If we set any `embedded=true`, write out the CSV again
    // ------------------------------------------------------------------------
    if changed_any {
        write_indicators_csv(csv_path, &rows)?;
        info!("Updated {csv_path} with embedded=true for newly embedded files.");
    } else {
        info!("No changes in CSV. No rewrite needed.");
    }

    // ------------------------------------------------------------------------
    // 9. (Optional) Build your vector store index
    // ------------------------------------------------------------------------
    let index = vector_store.index(model.clone());
    info!("Vector store indexed. Ready for RAG queries if needed.");

    let rag_agent = openai_client
    .agent("gpt-4")
    .preamble("
        You are an assistant that checks parity between Python/Cython indicators
        and their Rust counterparts. For each indicator, produce a single row in
        a Markdown table with columns:
        
          1) Indicator
          2) Rust Implementation
          3) Python/Cython Implementation
          4) Functional Parity (🟢 or 🔴)
          5) Test Coverage Parity (🟢 or 🔴)
          6) Notes

        Keep it concise but thorough. Use the vector store context to find relevant Rust code.
    ")
    .dynamic_context(1, index)
    .build();

    let indicators = load_indicators_csv("indicators.csv")?;


// 5) Prepare a buffer for our final Markdown output
    let mut md_output = Vec::new();

        // Write the table header
        md_output.push("# Indicator Parity Summary".to_string());
        md_output.push("".to_string());
        md_output.push("| **Indicator** | **Rust Implementation** | **Python/Cython** | **Functional Parity** | **Test Coverage Parity** | **Notes** |".to_string());
        md_output.push("|---------------|-------------------------|-------------------|-----------------------|--------------------------|-----------|".to_string());
    
        // 6) For each indicator, ask GPT to produce a single table row
        for ind in &indicators {
            let user_query = format!(
                "Indicator name: {}\n\
                 Python/Cython path: {}\n\
                 Compare with Rust code, if any, and produce **exactly one** Markdown row.\n\
                 Use 🟢 for pass, 🔴 for fail.\n",
                ind.indicator_name,
                ind.filename
            );

    
            // RAG query
            let response = match rag_agent.prompt(user_query.as_str()).await {
                Ok(resp) => resp,
                Err(e) => {
                    eprintln!("Warning: Could not get a response for {}: {:#}", ind.indicator_name, e);
                    // Fallback row with an error message
                    format!(
                        "| {} | (error) | {} | 🔴 | 🔴 | Failed to retrieve info |",
                        ind.indicator_name, ind.filename
                    )
                }
            };
    
            md_output.push(response);
        }
    
        // 7) Optionally add an Additional Observations section
        md_output.push("".to_string());
        md_output.push("## Additional Observations".to_string());
        md_output.push("(Place any overarching notes or disclaimers here.)".to_string());
    
        // 8) Write everything to a markdown file
        let mut file = File::create("README_parity.md")?;
        for line in md_output {
            writeln!(file, "{}", line)?;
        }
    
        println!("Wrote README_parity.md with the parity comparison table.");
    

    // Everything done
    Ok(())
}

/// Load the entire indicators CSV into a `Vec<IndicatorRow>`.
/// If the CSV doesn’t have an `embedded` column yet, it will default to `false`.
fn load_indicators_csv<P: AsRef<Path>>(path: P) -> Result<Vec<IndicatorRow>> {
    let file = File::open(path.as_ref())
        .map_err(|e| anyhow!("Failed to open CSV {}: {}", path.as_ref().display(), e))?;
    let mut rdr = csv::ReaderBuilder::new()
        .has_headers(true)
        .flexible(true)        // allow extra/less columns
        .trim(csv::Trim::All)
        .from_reader(file);

    let mut rows: Vec<IndicatorRow> = Vec::new();
    for result in rdr.deserialize() {
        let record: IndicatorRow = result?;
        rows.push(record);
    }
    Ok(rows)
}

/// Write out the updated CSV, including the `embedded` column.
fn write_indicators_csv<P: AsRef<Path>>(path: P, rows: &[IndicatorRow]) -> Result<()> {
    let mut wtr = csv::WriterBuilder::new()
        .has_headers(true)
        .from_path(path.as_ref())?;

    // Write header row manually
    wtr.write_record(&["filename", "indicator_name", "extension", "embedded"])?;

    for row in rows {
        wtr.serialize(row)?;
    }
    wtr.flush()?;
    Ok(())
}

/// Fetch all snippet IDs already in the database.
async fn fetch_existing_ids(conn: &Connection) -> Result<HashSet<String>> {
    use rusqlite::params;
    let ids = conn
        .call(move |inner_conn| {
            let mut stmt = inner_conn.prepare("SELECT id FROM code_chunks")?;
            let rows = stmt.query_map(params![], |row| row.get::<_, String>(0))?;

            let mut found = HashSet::new();
            for row_result in rows {
                found.insert(row_result?);
            }
            Ok(found)
        })
        .await?;
    Ok(ids)
}

/// Read file contents as a String, naive version.
/// Adapt path-building logic as needed for your directory layout.
fn read_file_contents<P: AsRef<Path>>(path: P) -> Result<String> {
    let data = fs::read_to_string(path.as_ref())?;
    Ok(data)
}
