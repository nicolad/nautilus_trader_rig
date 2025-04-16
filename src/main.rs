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
    // e.g. "momentum/amat.rs" or "amat.pxd"
    filename: String,
    // e.g. "AMAT"
    indicator_name: String,
    // e.g. "rs" or "pxd" or "py"
    extension: String,
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
    // 2) Create the “comparisons” folder
    // -------------------------------------------------------------------------
    create_dir_all("comparisons")?;
    info!("Ensured 'comparisons' folder is present.");

    // -------------------------------------------------------------------------
    // 3) Create a DeepSeek client + agent
    // -------------------------------------------------------------------------
    let deepseek_client = DeepSeekClient::from_env();
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

Use '(rust_link)' wherever you want the Rust URL inserted,
and '(python_link)' for the Python/Cython URL. For example:
| MyIndicator | (rust_link) | (python_link) | 🟢 | 🟢 | Looks good |

Make sure to provide exactly one table row, using 🟢 or 🔴.
",
        )
        .build();
    info!("Comparison agent successfully built.");

    // We'll accumulate rows for our final combined Markdown
    let mut all_rows = Vec::new();

    // -------------------------------------------------------------------------
    // 4) For each indicator, generate and store individual + combined outputs
    // -------------------------------------------------------------------------
    for (idx, ind) in indicators.iter().enumerate() {
        info!(
            "Processing #{} => {} (file: {}, ext: {})",
            idx + 1,
            ind.indicator_name,
            ind.filename,
            ind.extension
        );

        // Build the full GitHub links for each side:
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

        // Decide how to treat each extension (rs => Rust, py/pxd => Python, etc.)
        let (python_impl, rust_impl) = match ind.extension.as_str() {
            "rs" => (String::from("(none)"), rust_link),
            "py" | "pxd" => (python_link, String::from("(none)")),
            _ => (
                "(unknown extension)".to_string(),
                "(unknown extension)".to_string(),
            ),
        };

        // 4a) Build a direct prompt for the agent, letting the LLM know
        //     we have placeholders (rust_link / python_link) to replace
        let prompt_string = format!(
            "Indicator: {}\n\
             Rust link (if any): {}\n\
             Python/Cython link (if any): {}\n\
             Provide exactly one Markdown row. Use '(rust_link)' and '(python_link)' placeholders \
             for me to insert final GitHub links.\n\
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
                // fallback row, using the link directly or placeholders
                format!(
                    "| {} | [Rust Implementation]({}) | [Python/Cython Implementation]({}) | 🔴 | 🔴 | Request failed |",
                    ind.indicator_name, rust_impl, python_impl
                )
            }
        };

        // 4c) Insert clickable GitHub links in place of placeholders (if present).
        let final_row = embed_links_in_row(&row, &rust_impl, &python_impl);

        // 4d) Write an individual Markdown file for this indicator
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

        // 4e) Add to final combined
        all_rows.push(final_row);
    }

    // -------------------------------------------------------------------------
    // 5) Now build the big README_parity.md
    // -------------------------------------------------------------------------
    info!("All indicators processed. Building README_parity.md ...");
    let mut md_output = Vec::new();
    md_output.push("# Indicator Parity Summary".to_string());
    md_output.push("".to_string());
    md_output.push("| **Indicator** | **Rust Implementation** | **Python/Cython** | **Functional Parity** | **Test Coverage Parity** | **Notes** |".to_string());
    md_output.push("|---------------|-------------------------|-------------------|-----------------------|--------------------------|-----------|".to_string());

    // Add all accumulated rows
    md_output.extend(all_rows);

    // Optional final notes
    md_output.push("".to_string());
    md_output.push("## Additional Observations".to_string());
    md_output.push("(Place any overarching notes or disclaimers here.)".to_string());

    // 5b) Write it out
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

/// Minimal “filename sanitizer” to remove or replace certain chars.
fn sanitize_filename(name: &str) -> String {
    let mut clean = name.to_string();
    clean = clean.replace("/", "_");
    clean = clean.replace("\\", "_");
    clean = clean.replace(" ", "_");
    clean
}

/// Replaces `(rust_link)` and `(python_link)` in `row` with clickable GitHub links.
fn embed_links_in_row(row: &str, rust_link: &str, python_link: &str) -> String {
    // Example row from the agent might be:
    // "| AMAT | (rust_link) | (python_link) | 🟢 | 🟢 | Looks good |"
    row.replace(
        "(rust_link)",
        &format!("[Rust Implementation]({})", rust_link),
    )
    .replace(
        "(python_link)",
        &format!("[Python/Cython Implementation]({})", python_link),
    )
}
