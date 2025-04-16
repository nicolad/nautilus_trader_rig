use anyhow::{Result, anyhow};
use csv::ReaderBuilder;
use dotenv::dotenv;
use rig::{completion::Prompt, providers::deepseek};
use serde::{Deserialize, Serialize};
use std::{
    fs::{File, create_dir_all},
    io::{BufReader, Write},
    path::{Path, PathBuf},
};
use tracing::{debug, info};

#[derive(Debug, Deserialize, Serialize, Clone)]
struct IndicatorRow {
    filename: String,
    indicator_name: String,
    extension: String,
}

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize logging + .env
    tracing_subscriber::fmt()
        .with_target(false)
        .with_thread_names(true)
        .with_level(true)
        .init();
    dotenv().ok(); // Load .env if present

    info!("Starting the indicator comparison process...");

    // 1) Load indicators from CSV
    let csv_path = "indicators.csv";
    let indicators = load_indicators_csv(csv_path)?;
    info!("Loaded {} indicators from {}", indicators.len(), csv_path);

    // 2) Create the “comparisons” folder if it doesn’t exist
    create_dir_all("comparisons")?;
    info!("Ensured 'comparisons' folder is present.");

    // 3) Create a DeepSeek client
    let deepseek_client = deepseek::Client::from_env();

    // 4) Build an agent to generate Markdown table rows
    let comparison_agent = deepseek_client
        .agent(deepseek::DEEPSEEK_REASONER)
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

Keep it concise but thorough.
",
        )
        .build();
    info!("Comparison agent successfully built.");

    // We’ll accumulate rows for the final combined Markdown
    let mut all_rows = Vec::new();

    // 5) Iterate over each indicator
    for (idx, ind) in indicators.iter().enumerate() {
        info!(
            "Processing indicator #{}: {} (file: {}, ext: {})",
            idx + 1,
            ind.indicator_name,
            ind.filename,
            ind.extension
        );

        // Build the prompt for the agent
        let prompt_string = format!(
            "Indicator: {}\n\
             File path: {}\n\
             Extension: {}\n\
             Produce exactly one row in Markdown.\n\
             Use 🟢 for pass, 🔴 for fail.\n",
            ind.indicator_name, ind.filename, ind.extension
        );
        debug!("Prompt:\n{}", prompt_string);

        // Prompt the agent for a single row
        let row = match comparison_agent.prompt(prompt_string.as_str()).await {
            Ok(resp) => resp,
            Err(e) => {
                eprintln!(
                    "Warning: Could not generate parity row for {}: {}",
                    ind.indicator_name, e
                );
                format!(
                    "| {} | (error) | {} | 🔴 | 🔴 | Request failed |",
                    ind.indicator_name, ind.filename
                )
            }
        };

        // 5a) Write an individual Markdown file for this indicator
        //     e.g., "comparisons/IndicatorName.md"
        //     (You can sanitize the filename if indicator_name might have invalid chars)
        let indicator_md_path = PathBuf::from("comparisons")
            .join(format!("{}.md", sanitize_filename(&ind.indicator_name)));

        {
            // Write a small table with a header + single row
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
            writeln!(f, "{}", row)?;
        }
        info!("Wrote individual file: {}", indicator_md_path.display());

        // 5b) Save the row for final combined output
        all_rows.push(row);
    }

    // 6) Build the combined Markdown
    info!("All indicators processed. Building final README_parity.md ...");
    let mut md_output = Vec::new();
    md_output.push("# Indicator Parity Summary".to_string());
    md_output.push("".to_string());
    md_output.push("| **Indicator** | **Rust Implementation** | **Python/Cython** | **Functional Parity** | **Test Coverage Parity** | **Notes** |".to_string());
    md_output.push("|---------------|-------------------------|-------------------|-----------------------|--------------------------|-----------|".to_string());

    // Add the collected rows
    md_output.extend(all_rows);

    // Optional final notes
    md_output.push("".to_string());
    md_output.push("## Additional Observations".to_string());
    md_output.push("(Place any overarching notes or disclaimers here.)".to_string());

    // 7) Write everything to a big MD file
    let mut file = File::create("README_parity.md")?;
    for line in md_output {
        writeln!(file, "{}", line)?;
    }
    info!("Wrote README_parity.md with the parity comparison table.");

    Ok(())
}

/// Load the entire indicators CSV into a `Vec<IndicatorRow>`.
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

/// Rudimentary function to sanitize a string for use as a filename.
/// You could make this more robust, e.g., removing spaces or special characters.
fn sanitize_filename(name: &str) -> String {
    let mut clean = name.to_string();
    clean = clean.replace("/", "_");
    clean = clean.replace("\\", "_");
    clean = clean.replace(" ", "_");
    clean
}
