use anyhow::{Result, anyhow};
use csv::ReaderBuilder;
use dotenv::dotenv;
use rig::{completion::Prompt, providers::deepseek::Client as DeepSeekClient};
use serde::{Deserialize, Serialize};
use std::{
    fs::{File, create_dir_all, read_to_string},
    io::{BufReader, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
};
use tracing::{debug, error, info};

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
    tracing_subscriber::fmt()
        .with_target(false)
        .with_thread_names(true)
        .with_level(true)
        .init();
    dotenv().ok();
    info!("Starting indicator comparison...");

    let csv_path = "indicators.csv";
    let indicators = load_indicators_csv(csv_path)?;

    create_dir_all("comparisons")?;

    let deepseek_client = DeepSeekClient::from_env();
    let comparison_agent = deepseek_client
        .agent("deepseek-chat")
        .preamble(
            "
You're checking parity between Python/Cython indicators and Rust counterparts.

Output exactly ONE Markdown table row:
| Indicator | Functional Parity (🟢/🔴) | Test Coverage Parity (🟢/🔴) | Notes |
",
        )
        .build();

    let mut all_rows = Vec::new();

    for ind in indicators.iter() {
        info!("Processing indicator: {}", ind.indicator_name);

        let rust_filepath = find_matching_rust(ind)?;
        let rust_content = rust_filepath.as_ref().map_or("N/A".into(), |p| {
            read_file_contents(p).unwrap_or_else(|_| "Rust file unavailable".into())
        });

        let python_content = read_file_contents(&ind.filename)
            .unwrap_or_else(|_| "Python/Cython file unavailable".into());

        let prompt = format!(
            "
Indicator: {}

{}

### Python/Cython Implementation:
{}

Evaluate parity. Output ONE Markdown row.
",
            ind.indicator_name, rust_content, python_content
        );

        debug!("Sending prompt to agent...");
        let row = comparison_agent
            .prompt(prompt.as_str())
            .await
            .unwrap_or_else(|e| {
                error!("Agent error: {}", e);
                format!(
                    "| {} | N/A | N/A | 🔴 | 🔴 | Agent error |",
                    ind.indicator_name
                )
            });

        let clean_row = row
            .replace("(rust_link)", "Rust Implementation")
            .replace("(python_link)", "Python/Cython Implementation");

        all_rows.push(clean_row.clone());

        let indicator_md =
            PathBuf::from("comparisons").join(format!("{}.md", sanitize(&ind.indicator_name)));

        let md_content = format!(
            "# Comparison for {}\n\n\
             | Indicator | Functional Parity (🟢/🔴) | Test Coverage Parity (🟢/🔴) | Notes |\n\
             |-----------|---------------------------|-----------------------------|-------|\n\
             {}\n",
            ind.indicator_name, clean_row
        );

        let formatted_md = beautify_markdown(&md_content)?;

        let mut file = File::create(&indicator_md)?;
        file.write_all(formatted_md.as_bytes())?;
    }

    let summary_md_header = "# Indicator Parity Summary\n\n\
    | Indicator | Functional Parity (🟢/🔴) | Test Coverage Parity (🟢/🔴) | Notes |\n\
    |-----------|---------------------------|-----------------------------|-------|\n";

    let summary_md_content = all_rows.join("\n");
    let full_readme_md = format!("{}{}\n", summary_md_header, summary_md_content);

    let formatted_summary_md = beautify_markdown(&full_readme_md)?;

    let mut readme = File::create("README_parity.md")?;
    readme.write_all(formatted_summary_md.as_bytes())?;

    Ok(())
}

// --- Helper Functions ---

fn beautify_markdown(input: &str) -> Result<String> {
    let mut child = Command::new("prettier")
        .args(["--parser", "markdown"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;

    child
        .stdin
        .as_mut()
        .ok_or(anyhow!("Failed to open stdin"))?
        .write_all(input.as_bytes())?;

    let output = child.wait_with_output()?;

    if output.status.success() {
        Ok(String::from_utf8(output.stdout)?)
    } else {
        Err(anyhow!(
            "Prettier failed: {}",
            String::from_utf8_lossy(&output.stderr)
        ))
    }
}

fn load_indicators_csv<P: AsRef<Path>>(path: P) -> Result<Vec<IndicatorRow>> {
    let file = File::open(path)?;
    let mut rdr = ReaderBuilder::new()
        .has_headers(true)
        .from_reader(BufReader::new(file));
    rdr.deserialize()
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| anyhow!(e))
}

fn read_file_contents<P: AsRef<Path>>(path: P) -> Result<String> {
    read_to_string(&path).map_err(|e| anyhow!("Error reading {}: {}", path.as_ref().display(), e))
}

fn sanitize(name: &str) -> String {
    name.replace(['/', '\\', ' '], "_")
}

fn find_matching_rust(ind: &IndicatorRow) -> Result<Option<PathBuf>> {
    let paths = [
        "momentum",
        "volatility",
        "ratio",
        "book",
        "average",
        "python/momentum",
        "python/average",
    ];

    for p in paths.iter() {
        let candidate_path = format!(
            "nautilus_trader/crates/indicators/src/{}/{}.rs",
            p, ind.indicator_name
        );
        if Path::new(&candidate_path).exists() {
            return Ok(Some(candidate_path.into()));
        }
    }

    Ok(None)
}
