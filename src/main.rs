use anyhow::{Context, Result};
use git2::Repository;
use rig::{
    Embed, completion::Prompt, embeddings::EmbeddingsBuilder, providers::ollama::Client,
    vector_store::in_memory_store::InMemoryVectorStore,
};
use serde::{Deserialize, Serialize};
use tracing::{debug, error, info};

#[derive(Embed, Clone, Debug, Serialize, Deserialize, Eq, PartialEq)]
struct CodeChunk {
    id: String,
    #[embed]
    content: String,
    language: String,
    file_path: String,
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

    // 1. Open a local Git repository (already cloned manually)
    let repo_path = "./nautilus_trader";
    debug!("Opening repository at {}", repo_path);

    let repo = Repository::open(repo_path)
        .with_context(|| format!("Failed to open local repo at {repo_path}"))?;
    info!("Repository opened successfully");

    // 2. Get the latest commit from the local 'develop' branch
    debug!("Finding 'develop' branch");
    let branch = repo
        .find_branch("develop", git2::BranchType::Local)
        .context("Failed to find 'develop' branch locally")?;

    debug!("Peeling branch to commit");
    let commit = branch
        .get()
        .peel_to_commit()
        .context("Failed to peel 'develop' branch to a commit")?;

    let tree = commit.tree().context("Failed to read tree from commit")?;
    info!("Got tree from latest commit: {}", commit.id());

    let indicators_path_prefix = "nautilus_trader/indicators";
    let mut code_snippets = Vec::new();

    tree.walk(git2::TreeWalkMode::PreOrder, |root, entry| {
        if let Some(obj) = entry.to_object(&repo).ok() {
            if let Some(blob) = obj.as_blob() {
                let file_path = format!("{}{}", root, entry.name().unwrap_or_default());
                if file_path.starts_with(indicators_path_prefix) {
                    if let Ok(file_str) = std::str::from_utf8(blob.content()) {
                        let language = if file_path.ends_with(".py") {
                            "python"
                        } else if file_path.ends_with(".rs") {
                            "rust"
                        } else if file_path.ends_with(".pyx") || file_path.ends_with(".pxd") {
                            "cython"
                        } else {
                            "unknown"
                        };

                        if language != "unknown" {
                            let id = format!("{}::{}", commit.id(), file_path);
                            code_snippets.push(CodeChunk {
                                id,
                                content: file_str.to_string(),
                                language: language.to_string(),
                                file_path: file_path.clone(),
                            });
                            debug!("Included indicator: {}", file_path);
                        }
                    }
                } else {
                    debug!("Skipped file: {}", file_path);
                }
            }
        }
        git2::TreeWalkResult::Ok
    })?;

    info!("Collected {} code snippets", code_snippets.len());

    // 4. Create an Ollama-based client for embeddings
    info!("Creating Ollama client for embeddings");
    let client = Client::new();
    let model = client.embedding_model("nomic-embed-text");

    // 5. Build embeddings for all code snippets
    info!("Building embeddings");
    let mut builder = EmbeddingsBuilder::new(model.clone());
    for snippet in &code_snippets {
        debug!(
            "Adding snippet to embeddings builder: {}",
            snippet.file_path
        );
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
        .agent("qwen2.5:14b")
        .preamble(
            "
            You are an assistant that compares Python vs. Rust code in this repository. 
            Summarize differences in a markdown table if asked.
            ",
        )
        .dynamic_context(1, index)
        .build();

    info!("Prompting RAG agent");
    // Prompt the agent and print the response
    let response = rag_agent.prompt("What does \"glarb-glarb\" mean?").await?;

    info!("Received response from RAG agent");
    println!("{}", response);

    info!("RAG agent task completed successfully");
    Ok(())
}
