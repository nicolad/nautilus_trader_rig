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

#[derive(Debug, Deserialize, Serialize, Clone)]
struct IndicatorRow {
    filename: String,       // e.g. "momentum/amat.rs"
    indicator_name: String, // e.g. "AMAT"
    extension: String,      // e.g. "rs", "py", "pxd"
}

#[tokio::main]
async fn main() -> Result<()> {
    // -------------------------------------------------------------------------
    // 0) Init logging + .env
    // -------------------------------------------------------------------------
    tracing_subscriber::fmt()
        .with_target(false)
        .with_thread_names(true)
        .with_level(true)
        .init();
    dotenv().ok();
    info!("Starting the indicator comparison process...");

    // -------------------------------------------------------------------------
    // 1) Load the CSV
    // -------------------------------------------------------------------------
    let csv_path = "indicators.csv";
    let indicators = load_indicators_csv(csv_path)?;
    info!("Loaded {} indicators from {}", indicators.len(), csv_path);

    // -------------------------------------------------------------------------
    // 2) Create the `comparisons` folder
    // -------------------------------------------------------------------------
    create_dir_all("comparisons")?;
    info!("Ensured 'comparisons' folder is present.");

    // -------------------------------------------------------------------------
    // 3) Create a DeepSeek client + agent
    // -------------------------------------------------------------------------
    let deepseek_client = DeepSeekClient::from_env();

    // We ask the agent to produce ONE table row, using placeholders `(rust_link)` and `(python_link)`.
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

Please place `(rust_link)` and `(python_link)` in the row where you want me to
insert the GitHub URLs. For example:

| MyIndicator | (rust_link) | (python_link) | 🟢 | 🟢 | Looks good |

No double headers—only a single row of data is needed.
",
        )
        .build();
    info!("Comparison agent successfully built.");

    // Collect rows for final combined table
    let mut all_rows = Vec::new();

    // -------------------------------------------------------------------------
    // 4) For each indicator, generate individual + combined outputs
    // -------------------------------------------------------------------------
    for (idx, ind) in indicators.iter().enumerate() {
        info!(
            "Processing #{} => {} (file: {}, ext: {})",
            idx + 1,
            ind.indicator_name,
            ind.filename,
            ind.extension
        );

        // Build the full GitHub links
        let rust_link = format!(
            "https://github.com/nautechsystems/nautilus_trader/blob/develop/crates/indicators/src/{}",
            ind.filename
        );
        let python_link = format!(
            "https://github.com/nautechsystems/nautilus_trader/blob/develop/nautilus_trader/indicators/{}",
            ind.filename
        );
        debug!("Rust link: {}", rust_link);
        debug!("Python link: {}", python_link);

        // Decide how to treat each extension
        // If the file is .rs, we consider it Rust. If .py or .pxd, Python/Cython, etc.
        let (python_impl, rust_impl) = match ind.extension.as_str() {
            "rs" => (String::from("(none)"), rust_link),
            "py" | "pxd" => (python_link, String::from("(none)")),
            _ => (
                "(unknown extension)".to_string(),
                "(unknown extension)".to_string(),
            ),
        };

        // 4a) Build a prompt for the agent
        let prompt_string = format!(
            "Indicator: {}\n\
             Rust link (if any): {}\n\
             Python/Cython link (if any): {}\n\
             Produce exactly one row with placeholders '(rust_link)' and '(python_link)' \
             that I'll replace with actual GitHub URLs.\n\
             Use 🟢 for pass, 🔴 for fail.\n",
            ind.indicator_name, rust_impl, python_impl
        );
        debug!("Prompt:\n{}", prompt_string);

        // 4b) Prompt the agent for a single row
        let row = match comparison_agent.prompt(prompt_string.as_str()).await {
            Ok(resp) => resp,
            Err(e) => {
                eprintln!(
                    "Warning: Could not generate row for {}: {}",
                    ind.indicator_name, e
                );
                // fallback row
                format!(
                    "| {} | [Rust Implementation]({}) | [Python/Cython Implementation]({}) | 🔴 | 🔴 | Request failed |",
                    ind.indicator_name, rust_impl, python_impl
                )
            }
        };

        // 4c) Insert the clickable GitHub links in place of placeholders
        let final_row = embed_links_in_row(&row, &rust_impl, &python_impl);

        // 4d) Write an individual Markdown file for this indicator
        // WITHOUT duplicating the header row. We want one set of column headings,
        // then the data row. No second set of headers.
        let indicator_md_path = PathBuf::from("comparisons")
            .join(format!("{}.md", sanitize_filename(&ind.indicator_name)));
        {
            let mut f = File::create(&indicator_md_path)?;
            // A short heading
            writeln!(f, "# Comparison for {}", ind.indicator_name)?;
            writeln!(f)?;

            // Single table header
            writeln!(
                f,
                "| **Indicator** | **Rust Implementation** | **Python/Cython** | **Functional Parity** | **Test Coverage Parity** | **Notes** |"
            )?;
            writeln!(
                f,
                "|---------------|-------------------------|-------------------|-----------------------|--------------------------|-----------|"
            )?;

            // Single row for that indicator
            writeln!(f, "{}", final_row)?;
        }
        info!("Wrote individual file: {}", indicator_md_path.display());

        // 4e) Add that row to the final combined table
        all_rows.push(final_row);
    }

    // -------------------------------------------------------------------------
    // 5) Build the big README_parity.md
    // -------------------------------------------------------------------------
    info!("All indicators processed. Building README_parity.md ...");
    let mut md_output = Vec::new();
    md_output.push("# Indicator Parity Summary".to_string());
    md_output.push("".to_string());

    // One table header for the entire summary
    md_output.push("| **Indicator** | **Rust Implementation** | **Python/Cython** | **Functional Parity** | **Test Coverage Parity** | **Notes** |".to_string());
    md_output.push("|---------------|-------------------------|-------------------|-----------------------|--------------------------|-----------|".to_string());

    // Add all the rows we gathered
    md_output.extend(all_rows);

    // Optional final notes
    md_output.push("".to_string());
    md_output.push("## Additional Observations".to_string());
    md_output.push("(Place any overarching notes or disclaimers here.)".to_string());

    // Write out the combined file
    let mut file = File::create("README_parity.md")?;
    for line in md_output {
        writeln!(file, "{}", line)?;
    }
    info!("Wrote README_parity.md with the parity comparison table.");

    Ok(())
}

// =============================================================================
// Helpers
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

/// Replaces `(rust_link)` and `(python_link)` in `row` with clickable GitHub links.
fn embed_links_in_row(row: &str, rust_link: &str, python_link: &str) -> String {
    // Example row from the agent:
    // "| MyIndicator | (rust_link) | (python_link) | 🟢 | 🟢 | All good |"
    row.replace(
        "(rust_link)",
        &format!("[Rust Implementation]({})", rust_link),
    )
    .replace(
        "(python_link)",
        &format!("[Python/Cython Implementation]({})", python_link),
    )
}

/// Rudimentary function to sanitize a string for use as a filename.
fn sanitize_filename(name: &str) -> String {
    let mut clean = name.to_string();
    clean = clean.replace("/", "_");
    clean = clean.replace("\\", "_");
    clean = clean.replace(" ", "_");
    clean
}
