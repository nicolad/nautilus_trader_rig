use anyhow::Result;
use git2::{Repository, TreeWalkMode, TreeWalkResult};
use rig::{
    completion::Prompt,
    embeddings::{Embed, EmbedError, EmbeddingsBuilder, TextEmbedder},
    providers::ollama::Client,
    vector_store::in_memory_store::InMemoryVectorStore,
};
use serde::{Deserialize, Serialize};
use tracing::info;
use std::fs::File;
use std::io::Write;

#[derive(Clone, Debug, Serialize, Deserialize, Eq, PartialEq)]
struct CodeChunk {
    id: String,
    content: String,
    language: String,
    file_path: String,
}

// Implement the Embed trait for rig 0.11+
//
// We must define fn embed(&self, embedder: &mut TextEmbedder) -> Result<(), EmbedError>.
// Note that embedder.embed(...) accepts a String, not a &str, and returns nothing,
// so we call it and then return Ok(()) ourselves.
impl Embed for CodeChunk {
    fn embed(&self, embedder: &mut TextEmbedder) -> Result<(), EmbedError> {
        // Pass a clone of self.content as a String
        embedder.embed(self.content.clone());
        // Satisfy the Result<(), EmbedError> requirement
        Ok(())
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize tracing subscriber for logging
    tracing_subscriber::fmt()
        .with_target(false)
        .with_thread_ids(true)
        .with_thread_names(true)
        .with_level(true)
        .init();

    info!("Starting RAG agent setup");

    // 1. Open local Git repository (assumes you have it cloned)
    let repo_path = "./nautilus_trader"; 
    let repo = Repository::open(repo_path)?;
    info!("Repository opened successfully");

    // 2. Get the latest commit from the local 'develop' branch
    let branch = repo.find_branch("develop", git2::BranchType::Local)?;
    let commit = branch.get().peel_to_commit()?;
    let tree = commit.tree()?;
    info!("Got tree from latest commit: {}", commit.id());

    // 3. Collect relevant code snippets
    let python_path_prefix = "nautilus_trader/indicators";
    let rust_path_prefix   = "crates/indicators";

    let mut code_snippets = Vec::new();

    tree.walk(TreeWalkMode::PreOrder, |root, entry| {
        let Ok(obj) = entry.to_object(&repo) else {
            return TreeWalkResult::Ok;
        };
        let Some(blob) = obj.as_blob() else {
            return TreeWalkResult::Ok;
        };

        let file_name = entry.name().unwrap_or_default();
        let file_path = format!("{}{}", root, file_name);

        // Only process files in these two indicator directories
        if file_path.starts_with(python_path_prefix) || file_path.starts_with(rust_path_prefix) {
            if let Ok(file_str) = std::str::from_utf8(blob.content()) {
                // Identify language
                let extension = if file_path.ends_with(".py") {
                    "python"
                } else if file_path.ends_with(".rs") {
                    "rust"
                } else if file_path.ends_with(".pyx") || file_path.ends_with(".pxd") {
                    "cython"
                } else {
                    "unknown"
                };

                // Only store recognized files
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

    // 4. Create an Ollama-based client for embeddings
    info!("Creating Ollama client for embeddings");
    let client = Client::new();
    // If your Ollama model name is different, adjust here
    let model = client.embedding_model("nomic-embed-text");

    // 5. Build embeddings for all code snippets
    info!("Building embeddings");
    let mut builder = EmbeddingsBuilder::new(model.clone());
    for snippet in &code_snippets {
        builder = builder.document(snippet.clone())?;
    }
    let embeddings = builder.build().await?;
    info!("Embeddings built successfully");

    // 6. Create an in-memory vector store and index
    info!("Creating in-memory vector store");
    let vector_store = InMemoryVectorStore::from_documents(embeddings);
    let index = vector_store.index(model.clone());
    info!("Vector store created and indexed");

    // 7. Build a RAG agent
    info!("Building RAG agent");
    let rag_agent = client
        // If you have a different large language model, adjust here
        .agent("qwen2.5:14b")
        .preamble(
            "
            You are an assistant that compares Python indicators (source of truth)
            in 'nautilus_trader/indicators' to corresponding Rust indicators in 'crates/indicators'.
            Summarize differences in a markdown table if asked.
            ",
        )
        // Adjust how many docs are fed to the prompt
        .dynamic_context(3, index)
        .build();

    // 8. Prompt the agent and print the response
    info!("Prompting RAG agent");
    let response = rag_agent
        .prompt("
            Compare Rust indicator implementations from 'crates/indicators' 
            against Python indicators in 'nautilus_trader/indicators'.
            Provide a markdown table structured as follows:

            | Indicator Name | Logic Match? | Test Coverage Match or Superior? | Discrepancies (if any) |
            |--------------- |------------- |--------------------------------- |------------------------ |
        ")
        .await?;

    info!("Received response from RAG agent");
    println!("\n## Indicators Parity Report\n{}", response);

    // 9. Save the response to a markdown file
    let mut file = File::create("indicators_parity_report.md")?;
    writeln!(file, "# Indicators Parity Report\n\n{}", response)?;

    info!("RAG agent task completed successfully");
    Ok(())
}
