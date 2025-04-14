use anyhow::{Context, Result};
use git2::{ObjectType, Repository};
use rig::{
    Embed, embeddings::EmbeddingsBuilder, providers::openai,
    vector_store::in_memory_store::InMemoryVectorStore,
};
use serde::{Deserialize, Serialize};

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
    // 1. Open a local Git repository
    let remote_url = "https://github.com/nautechsystems/nautilus_trader";
    let tmp_path = std::env::temp_dir().join("nautilus_trader");
    let repo = Repository::clone(remote_url, &tmp_path)
        .with_context(|| format!("Failed to clone repo from {remote_url}"))?;

    // 2. Read HEAD commit & tree
    let head = repo.head()?.peel(ObjectType::Commit)?;
    let commit = head.as_commit().expect("HEAD is not a commit");
    let tree = commit.tree()?;

    // 3. Walk the files in the tree, collect them as CodeChunk structs
    let mut code_snippets = Vec::new();
    tree.walk(git2::TreeWalkMode::PreOrder, |_, entry| {
        if let Some(obj) = entry.to_object(&repo).ok() {
            if obj.as_tree().is_none() {
                // It's a file (blob)
                if let Some(blob) = obj.as_blob() {
                    // Convert file contents to string
                    if let Ok(file_str) = std::str::from_utf8(blob.content()) {
                        // Basic heuristic: check if file is .py or .rs
                        if let Some(name) = entry.name() {
                            let language = if name.ends_with(".py") {
                                "python"
                            } else if name.ends_with(".rs") {
                                "rust"
                            } else {
                                "unknown"
                            };
                            let id = format!("{}::{}", commit.id(), name);

                            code_snippets.push(CodeChunk {
                                id,
                                content: file_str.to_string(),
                                language: language.to_string(),
                                file_path: name.to_string(),
                            });
                        }
                    }
                }
            }
        }
        git2::TreeWalkResult::Ok
    })?;

    // 4. Create a DeepSeek client (reads config from environment)
    let client = openai::Client::from_url("ollama", "http://localhost:11434/v1");

    // https://github.com/0xPlaygrounds/rig/blob/main/rig-core/examples/rag_ollama.rs#L29
    // https://ollama.com/library/nomic-embed-text
    let model = client.embedding_model("nomic-embed-text");

    // 5. Build embeddings for all code snippets
    let mut builder = EmbeddingsBuilder::new(model.clone());
    for snippet in code_snippets {
        builder = builder.document(snippet)?;
    }

    // 6. Actually build the embeddings (await directly in async context)
    let embeddings = builder.build().await?;

    // 7. Create vector store and an index
    let vector_store = InMemoryVectorStore::from_documents(embeddings);
    let index = vector_store.index(model);

    // 8. Build a RAG agent
    let rag_agent = client
        .agent("indicator_parity_checker")
        .preamble(
            "You are an assistant that compares Python vs. Rust code in this repository. \
             Summarize differences in a markdown table if asked.",
        )
        // auto-retrieve top 3 relevant code snippets for each prompt
        .dynamic_context(3, index)
        .build();

    // 9. Prompt the agent
    let query = r#"
Compare the Python vs. Rust indicators in this repository. 
Give me a summary in a Markdown table with columns: 
(Implementation | Inputs | Outputs | Differences).
"#;

    // If you have a CLI chatbot utility, you can invoke it here:
    if let Err(e) = rig::cli_chatbot::cli_chatbot(rag_agent).await {
        eprintln!("Error: {}", e);
    }

    Ok(())
}
