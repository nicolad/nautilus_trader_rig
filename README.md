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

Below is a Mermaid flowchart illustrating the major steps in the process:

```mermaid
flowchart TB
    A((Start)) --> B[Initialize tracing/logging <br> & load environment (.env)]
    B --> C[Open local Git repository <br> (branch = develop)]
    C --> D[Get latest commit tree <br> from 'develop']
    D --> E[Walk all files in repo tree <br> (PreOrder)]
    E --> F{File extension <br> .py/.pyx/.pxd/.rs/.r?}
    F -->|Yes| G[Extract file content <br> & store as CodeChunk]
    G --> H[Check for indicator definitions]
    H --> I{Indicator match?}
    I -->|Yes| J[Collect indicator <br> name & path]
    I -->|No| K[Continue walking <br> other files]
    J --> K[Keep scanning <br> next lines]
    F -->|No| K

    K --> L[Write discovered indicators <br> to indicators.csv]
    L --> M[Generate embeddings <br> via OpenAI <br>(text-embedding-ada-002)]
    M --> N[Initialize & connect <br> to local SQLite DB]
    N --> O[Check existing snippet IDs <br> to avoid duplicates]
    O --> P[Store new snippet embeddings <br> & code metadata in DB]
    P --> Q[Build vector index <br> for retrieval (RAG)]
    Q --> R[Use LLM to compare <br> Rust vs. Python indicators]
    R --> S[Save generated comparison <br> table to README_comparison.md]
    S --> T[Write snippet summaries <br> to .txt and .md files]
    T --> U((Done))
