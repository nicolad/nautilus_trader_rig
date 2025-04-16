use anyhow::{Result, anyhow};
use csv::ReaderBuilder;
use dotenv::dotenv;
use rig::{completion::Prompt, providers::deepseek::Client as DeepSeekClient};
use serde::{Deserialize, Serialize};
use std::{
    fs::{File, create_dir_all},
    io::{BufReader, Write},
    path::{Path, PathBuf},
};
use tracing::{debug, info};

/// Matches the CSV columns exactly:
/// filename,indicator_name,extension,embedded,gh_link
#[derive(Debug, Deserialize, Serialize, Clone)]
struct IndicatorRow {
    filename: String,
    indicator_name: String,
    extension: String,
    embedded: bool,
    gh_link: String,
}

#[tokio::main]
async fn main() -> Result<()> {
    // -------------------------------------------------------------------------
    // 0) Initialize logging + .env
    // -------------------------------------------------------------------------
    tracing_subscriber::fmt()
        .with_target(false)
        .with_thread_names(true)
        .with_level(true)
        .init();
    dotenv().ok();
    info!("Starting the indicator comparison process...");

    // -------------------------------------------------------------------------
    // 1) Load indicators from CSV
    // -------------------------------------------------------------------------
    let csv_path = "indicators.csv";
    let indicators = load_indicators_csv(csv_path)?;
    info!("Loaded {} indicators from {}", indicators.len(), csv_path);

    // -------------------------------------------------------------------------
    // 2) Create the “comparisons” folder
    // -------------------------------------------------------------------------
    create_dir_all("comparisons")?;
    info!("Ensured 'comparisons' folder is present.");

    // -------------------------------------------------------------------------
    // 3) Create a DeepSeek client + agent
    // -------------------------------------------------------------------------
    let deepseek_client = DeepSeekClient::from_env();

    // Preamble: instruct the agent to produce exactly ONE table row with placeholders
    let comparison_agent = deepseek_client
        .agent("deepseek-chat")
        .preamble(
            "
You are an assistant that checks parity between Python/Cython indicators
and their Rust counterparts. For each indicator, produce a single row in
a Markdown table with columns:
  1) Indicator
  2) Rust Implementation
  3) Python/Cython Implementation
  4) Functional Parity (🟢 or 🔴)
  5) Test Coverage Parity (🟢 or 🔴)
  6) Notes

Use '(rust_link)' and '(python_link)' placeholders to show where the GitHub
links go (or 'N/A' if there is no recognized extension).
Example row:

| MyIndicator | (rust_link) | (python_link) | 🟢 | 🟢 | Some notes |

No double headers—only a single row of data is needed.
",
        )
        .build();
    info!("Comparison agent successfully built.");

    // Collect final rows for the big README
    let mut all_rows = Vec::new();

    // -------------------------------------------------------------------------
    // 4) Iterate over each CSV row, prompt the agent, and write results
    // -------------------------------------------------------------------------
    for (idx, ind) in indicators.iter().enumerate() {
        info!(
            "Processing #{} => {} (file: {}, ext: {}, embedded: {}, link: {})",
            idx + 1,
            ind.indicator_name,
            ind.filename,
            ind.extension,
            ind.embedded,
            ind.gh_link,
        );

        // Decide if the link belongs to Rust or Python/Cython
        let (rust_link, python_link) = match ind.extension.as_str() {
            "rust" => (ind.gh_link.clone(), "N/A".to_string()),
            "python" | "py" | "cython" | "pyx" | "pxd" => ("N/A".to_string(), ind.gh_link.clone()),
            _ => ("N/A".into(), "N/A".into()),
        };

        // Build the prompt for the LLM
        let prompt_string = format!(
            "
Indicator: {}
Rust link (if any): {}
Python/Cython link (if any): {}
Use '(rust_link)' and '(python_link)' placeholders for me to replace.
Use 🟢 for pass, 🔴 for fail.
Make only ONE row of Markdown.
",
            ind.indicator_name, rust_link, python_link
        );
        debug!("Prompt:\n{}", prompt_string);

        // Ask the agent
        let row = match comparison_agent.prompt(prompt_string.as_str()).await {
            Ok(resp) => resp,
            Err(e) => {
                eprintln!(
                    "Warning: Could not generate row for {}: {}",
                    ind.indicator_name, e
                );
                // Fallback row
                format!(
                    "| {} | [Rust Implementation]({}) | [Python/Cython Implementation]({}) | 🔴 | 🔴 | Request failed |",
                    ind.indicator_name, rust_link, python_link
                )
            }
        };

        // Insert the actual GitHub links (or N/A)
        let final_row = embed_links_in_row(&row, &rust_link, &python_link);

        // Write an individual .md file for this indicator
        let indicator_md_path = PathBuf::from("comparisons")
            .join(format!("{}.md", sanitize_filename(&ind.indicator_name)));
        {
            let mut f = File::create(&indicator_md_path)?;
            writeln!(f, "# Comparison for {}", ind.indicator_name)?;
            writeln!(f)?;
            writeln!(
                f,
                "| **Indicator** | **Rust Implementation** | **Python/Cython** | **Functional Parity** | **Test Coverage Parity** | **Notes** |"
            )?;
            writeln!(
                f,
                "|---------------|-------------------------|-------------------|-----------------------|--------------------------|-----------|"
            )?;
            writeln!(f, "{}", final_row)?;
        }
        info!("Wrote individual file: {}", indicator_md_path.display());

        // Accumulate for the final combined table
        all_rows.push(final_row);
    }

    // -------------------------------------------------------------------------
    // 5) Create a combined README_parity.md
    // -------------------------------------------------------------------------
    info!("All indicators processed. Building README_parity.md ...");
    let mut md_output = Vec::new();
    md_output.push("# Indicator Parity Summary".to_string());
    md_output.push("".to_string());

    // Single header row
    md_output.push("| **Indicator** | **Rust Implementation** | **Python/Cython** | **Functional Parity** | **Test Coverage Parity** | **Notes** |".to_string());
    md_output.push("|---------------|-------------------------|-------------------|-----------------------|--------------------------|-----------|".to_string());

    // Append each row
    md_output.extend(all_rows);

    // Optional concluding note
    md_output.push("".to_string());
    md_output.push("## Additional Observations".to_string());
    md_output.push("(Place any overarching notes or disclaimers here.)".to_string());

    // Write the final file
    let mut file = File::create("README_parity.md")?;
    for line in md_output {
        writeln!(file, "{}", line)?;
    }
    info!("Wrote README_parity.md with the parity comparison table.");

    Ok(())
}

// =============================================================================
// Helper Functions
// =============================================================================

fn load_indicators_csv<P: AsRef<Path>>(path: P) -> Result<Vec<IndicatorRow>> {
    info!("Attempting to open CSV from: {}", path.as_ref().display());
    let file = File::open(path.as_ref())
        .map_err(|e| anyhow!("Failed to open CSV {}: {}", path.as_ref().display(), e))?;

    let mut rdr = ReaderBuilder::new()
        .has_headers(true)
        .trim(csv::Trim::All)
        .from_reader(BufReader::new(file));

    let mut rows = Vec::new();
    for result in rdr.deserialize() {
        let record: IndicatorRow = result?;
        rows.push(record);
    }
    info!("Finished reading CSV with {} rows of data.", rows.len());
    Ok(rows)
}

/// Replaces `(rust_link)` or `(python_link)` in `row` with either:
/// `[Rust Implementation](URL)` or `[Python/Cython Implementation](URL)`
/// or "N/A" if extension not recognized.
fn embed_links_in_row(row: &str, rust_link: &str, python_link: &str) -> String {
    let rust_md = if rust_link == "N/A" {
        "N/A".to_string()
    } else {
        format!("[Rust Implementation]({})", rust_link)
    };
    let python_md = if python_link == "N/A" {
        "N/A".to_string()
    } else {
        format!("[Python/Cython Implementation]({})", python_link)
    };

    row.replace("(rust_link)", &rust_md)
        .replace("(python_link)", &python_md)
}

/// A simple function to replace forbidden chars (/, \, space) in filenames.
fn sanitize_filename(name: &str) -> String {
    let mut clean = name.to_string();
    clean = clean.replace("/", "_");
    clean = clean.replace("\\", "_");
    clean = clean.replace(" ", "_");
    clean
}
