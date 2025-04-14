use anyhow::Result;
use dotenv::dotenv;
use git2::{Repository, TreeWalkMode, TreeWalkResult};
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
    io::Write,
};
use tokio_rusqlite::Connection;
use tracing::info;

// A code snippet
#[derive(Clone, Debug, Serialize, Deserialize, Eq, PartialEq)]
struct CodeChunk {
    id: String,
    content: String,
    language: String,
    file_path: String,
}

// Implement the Embed trait for rig 0.11 (or 0.1x) so rig can build embeddings
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

    // 3. Collect relevant code snippets
    let python_path_prefix = "nautilus_trader/indicators";
    let rust_path_prefix = "crates/indicators";

    let mut code_snippets = Vec::new();
    tree.walk(TreeWalkMode::PreOrder, |root, entry| {
        // Convert to a blob
        let Ok(obj) = entry.to_object(&repo) else {
            return TreeWalkResult::Ok;
        };
        let Some(blob) = obj.as_blob() else {
            return TreeWalkResult::Ok;
        };

        let file_name = entry.name().unwrap_or_default();
        let file_path = format!("{}{}", root, file_name);

        // We only care about these directories
        if file_path.starts_with(python_path_prefix) || file_path.starts_with(rust_path_prefix) {
            if let Ok(file_str) = std::str::from_utf8(blob.content()) {
                // Identify language from extension
                let extension = if file_path.ends_with(".py") {
                    "python"
                } else if file_path.ends_with(".rs") {
                    "rust"
                } else if file_path.ends_with(".pyx") || file_path.ends_with(".pxd") {
                    "cython"
                } else {
                    "unknown"
                };

                if extension != "unknown" {
                    code_snippets.push(CodeChunk {
                        id: format!("{}::{}", commit.id(), file_path),
                        content: file_str.to_string(),
                        language: extension.to_string(),
                        file_path,
                    });
                }
            }
        }

        TreeWalkResult::Ok
    })?;

    info!("Collected {} code snippets", code_snippets.len());

    // 4. Build embeddings with OpenAI
    let openai_api_key = env::var("OPENAI_API_KEY")
        .expect("Expected OPENAI_API_KEY in environment variables");
    let openai_client = Client::new(&openai_api_key);
    let model = openai_client.embedding_model(TEXT_EMBEDDING_ADA_002);

    info!("Building embeddings with OpenAI (text-embedding-ada-002)");

    let mut builder = EmbeddingsBuilder::new(model.clone());
    for snippet in &code_snippets {
        builder = builder.document(snippet.clone())?;
    }
    let embeddings = builder.build().await?;
    info!("Embeddings built successfully");

    // 5. Initialize `sqlite-vec`
    unsafe {
        sqlite3_auto_extension(Some(std::mem::transmute(sqlite3_vec_init as *const ())));
    }

    // 6. Create or open your SQLite DB
    let conn = Connection::open("code_chunks_vector_store.db").await?;
    let vector_store = SqliteVectorStore::new(conn.clone(), &model).await?;

    // 7. Before inserting everything, figure out what we already have
    //    Because older rig-sqlite doesn't provide `select_all()` or `query_rows()`,
    //    we do a *manual* query on the underlying rusqlite connection via `tokio_rusqlite::Connection`.
    //    We'll keep it very simple: SELECT only 'id' from table 'code_chunks'.
    //
    //    The code below calls `conn.call(...)` which runs synchronously on a blocking thread.
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

    // 8. If there are new snippets, embed them (already done in `embeddings`, but you can also do 2 passes)
    //    However, since we already built `embeddings` for everything, let's filter out those doc embeddings too:
    //    rig 0.11's EmbeddingsBuilder doesn't keep a 1-1 mapping if we skip after building. Instead, let's embed them
    //    in a single pass for all code, but only store new ones. We'll store all embeddings, but rig-sqlite will
    //    skip duplicates if the primary key conflicts. Alternatively, do 2 passes: (1) filter out existing first,
    //    (2) embed only new. Up to you.
    //
    //    For demonstration, we’ll store everything. The DB will throw an error if you try to re-insert the same
    //    primary key. If you want to gracefully skip duplicates, do the 2-pass approach.
    info!("Storing embeddings in the SQLite vector store");
    vector_store.add_rows(embeddings).await?;
    info!("Embedded code snippets stored in SQLite vector store");

    // 9. Create a vector index on our store
    let index = vector_store.index(model.clone());
    info!("Vector store indexed. Ready for queries.");

    let rag_agent = openai_client.agent("gpt-4")
    .preamble("
        You are a dictionary assistant here to assist the user in understanding the meaning of words.
        You will find additional non-standard word definitions that could be useful below.
    ")
    .dynamic_context(1, index)
    .build();

      // Prompt the agent and print the response
    let response = rag_agent.prompt("What does \"glarb-glarb\" mean?").await?;
    
    println!("Response: {}", response);


    // 11. Optional: Summaries
    {
        let mut file = File::create("collected_code_chunks.txt")?;
        writeln!(
            file,
            "Wrote a summary of code snippets stored in the DB:\n"
        )?;

        // Manually read them again from your DB if you’d like:
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



    info!("Embedding and storage process completed successfully");
    Ok(())
}
