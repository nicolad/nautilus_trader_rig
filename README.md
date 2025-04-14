## Overview

1. **Open and Traverse the Git Repository**  
   - Clones/opens your target repository (default: `./nautilus_trader` on the `develop` branch).  
   - Identifies Python (`.py`, `.pyx`, `.pxd`) and Rust (`.rs`, optionally `.r`) files.

2. **Extract and Store Code Snippets**  
   - Reads entire file contents and creates code “snippets” as `CodeChunk` objects.  
   - Searches for indicator definitions:
     - Python/Cython: `class X(Indicator):` or `cdef class X(Indicator):`
     - Rust: `pub struct XIndicator {`
   - Writes discovered indicators to `indicators.csv`.

3. **Embed Using OpenAI**  
   - Uses `text-embedding-ada-002` to embed code snippets via the `rig` library.  
   - Stores vectors in a local SQLite database using `sqlite-vec`.

4. **Compare Python vs. Rust Indicators**  
   - Builds a retrieval-augmented generation (RAG) pipeline on top of the stored embeddings.  
   - Invokes GPT-4 to produce a Markdown table comparing Python and Rust indicators:
     1. Rust impl matches Python impl?
     2. Rust test coverage >= Python test coverage?

5. **Generate Reports**  
   - Saves a generated comparison table to `README_comparison.md`.  
   - Logs all snippet IDs in `collected_code_chunks.txt` and `collected_code_chunks.md`.

---

## Flow Diagram

```mermaid
flowchart TB
    A((Start)) --> B[Initialize tracing/logging and load environment (.env)]
    B --> C[Open local Git repository (branch = develop)]
    C --> D[Get latest commit tree from 'develop']
    D --> E[Walk all files in repo tree (PreOrder)]
    E --> F{File extension .py/.pyx/.pxd/.rs/.r?}
    F -->|Yes| G[Extract file content & store as CodeChunk]
    G --> H[Check for indicator definitions]
    H --> I{Indicator match?}
    I -->|Yes| J[Collect indicator name & path]
    I -->|No| K[Continue walking other files]
    J --> K[Keep scanning next lines]
    F -->|No| K

    K --> L[Write discovered indicators to indicators.csv]
    L --> M[Generate embeddings via OpenAI (text-embedding-ada-002)]
    M --> N[Initialize & connect to local SQLite DB]
    N --> O[Check existing snippet IDs to avoid duplicates]
    O --> P[Store new snippet embeddings & code metadata in DB]
    P --> Q[Build vector index for retrieval (RAG)]
    Q --> R[Use LLM to compare Rust vs. Python indicators]
    R --> S[Save generated comparison table to README_comparison.md]
    S --> T[Write snippet summaries to .txt and .md files]
    T --> U((Done))
```

## Code Explanation

### 1. Logging & Environment Setup
- Uses **tracing_subscriber** for logging.
- Loads **.env** variables via **dotenv**.

### 2. Repository Parsing
- Opens the Git repo, checks out `develop`, and walks the commit tree.
- Builds a `Vec<CodeChunk>` from any `.py`, `.pyx`, `.pxd`, `.rs`, or `.r` files.

### 3. Indicator Detection
- Uses regex to find definitions that match known patterns for “Indicator” classes/structs.
- Results stored in `indicators.csv`.

### 4. Embedding with OpenAI
- Uses `rig` with `text-embedding-ada-002` to generate embeddings for each code snippet.
- Stores embeddings in SQLite with `sqlite-vec`.

### 5. Comparison Table (LLM)
- Uses a retrieval-augmented generation (RAG) approach on GPT-4 to produce a Markdown table:
  - Does Rust implementation match Python?
  - Is Rust’s test coverage >= Python’s coverage?

### 6. Report Generation
- Writes:
  - **README_comparison.md** with the LLM-generated table.
  - **collected_code_chunks.txt** and **collected_code_chunks.md** listing snippet IDs stored in the SQLite DB.
