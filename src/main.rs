use anyhow::{Result, anyhow};
use csv::ReaderBuilder;
use dotenv::dotenv;
use rig::{completion::Prompt, providers::deepseek::Client as DeepSeekClient};
use serde::{Deserialize, Serialize};
use std::{
    fs::{File, create_dir_all, read_to_string},
    io::{BufReader, Write},
    path::{Path, PathBuf},
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
| Indicator | Rust Implementation | Python/Cython Implementation | Functional Parity (🟢/🔴) | Test Coverage Parity (🟢/🔴) | Notes |

Use '(rust_link)' and '(python_link)' placeholders.
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

### Rust Implementation:
{}

### Python/Cython Implementation:
{}

Evaluate parity. Use '(rust_link)' and '(python_link)' placeholders. Output ONE Markdown row.
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
                    "| {} | (rust_link) | (python_link) | 🔴 | 🔴 | Agent error |",
                    ind.indicator_name
                )
            });

        let final_row = embed_links(&row, ind, &rust_filepath);
        all_rows.push(final_row.clone());

        let indicator_md =
            PathBuf::from("comparisons").join(format!("{}.md", sanitize(&ind.indicator_name)));
        let mut file = File::create(&indicator_md)?;
        writeln!(file, "# Comparison for {}\n", ind.indicator_name)?;
        writeln!(
            file,
            "| Indicator | Rust Implementation | Python/Cython Implementation | Functional Parity | Test Coverage Parity | Notes |"
        )?;
        writeln!(
            file,
            "|-----------|---------------------|-------------------------------|-------------------|----------------------|-------|"
        )?;
        writeln!(file, "{}", final_row)?;
    }

    let mut readme = File::create("README_parity.md")?;
    writeln!(readme, "# Indicator Parity Summary\n")?;
    writeln!(
        readme,
        "| Indicator | Rust Implementation | Python/Cython Implementation | Functional Parity | Test Coverage Parity | Notes |"
    )?;
    writeln!(
        readme,
        "|-----------|---------------------|-------------------------------|-------------------|----------------------|-------|"
    )?;
    for row in all_rows {
        writeln!(readme, "{}", row)?;
    }

    Ok(())
}

// --- Helpers ---

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

fn embed_links(row: &str, ind: &IndicatorRow, rust_path: &Option<PathBuf>) -> String {
    let rust_link = rust_path.as_ref().map_or("N/A".into(), |_| {
        format!("[Rust Implementation]({})", rust_github_link(rust_path))
    });
    let python_link = format!("[Python/Cython Implementation]({})", ind.gh_link);
    row.replace("(rust_link)", &rust_link)
        .replace("(python_link)", &python_link)
}

fn rust_github_link(path: &Option<PathBuf>) -> String {
    path.as_ref()
        .map(|p| {
            let p_str = p.display().to_string();
            format!(
                "https://github.com/nautechsystems/nautilus_trader/blob/develop/{}",
                p_str
            )
        })
        .unwrap_or_else(|| "N/A".into())
}

fn sanitize(name: &str) -> String {
    name.replace(['/', '\\', ' '], "_")
}

fn find_matching_rust(ind: &IndicatorRow) -> Result<Option<PathBuf>> {
    let candidate_path = format!(
        "nautilus_trader/crates/indicators/src/momentum/{}.rs",
        ind.indicator_name
    );
    if Path::new(&candidate_path).exists() {
        return Ok(Some(candidate_path.into()));
    }
    let alt_paths = [
        format!(
            "nautilus_trader/crates/indicators/src/volatility/{}.rs",
            ind.indicator_name
        ),
        format!(
            "nautilus_trader/crates/indicators/src/ratio/{}.rs",
            ind.indicator_name
        ),
        format!(
            "nautilus_trader/crates/indicators/src/book/{}.rs",
            ind.indicator_name
        ),
        format!(
            "nautilus_trader/crates/indicators/src/average/{}.rs",
            ind.indicator_name
        ),
        format!(
            "nautilus_trader/crates/indicators/src/python/momentum/{}.rs",
            ind.indicator_name
        ),
        format!(
            "nautilus_trader/crates/indicators/src/python/average/{}.rs",
            ind.indicator_name
        ),
    ];
    for p in alt_paths.iter() {
        if Path::new(p).exists() {
            return Ok(Some(p.into()));
        }
    }
    Ok(None)
}
