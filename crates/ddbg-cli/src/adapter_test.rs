//! `ddbg adapter-test`: initialize an adapter and print its capabilities.

use std::time::Duration;

use anyhow::Context;
use ddbg_dap::protocol::{DisconnectArguments, InitializeArguments};
use ddbg_dap::{AdapterCommand, AdapterProcess};

pub async fn run(adapter: &[String]) -> anyhow::Result<()> {
    let (program, args) = adapter.split_first().context("missing adapter command")?;
    let mut cmd = AdapterCommand::new(program.as_str());
    cmd.args = args.to_vec();
    let cmd = cmd.resolve()?;
    println!("adapter: {}", cmd.program);

    let mut proc = AdapterProcess::spawn(&cmd)?;
    let caps = tokio::time::timeout(
        Duration::from_secs(10),
        proc.client.request(InitializeArguments::new("ddbg")),
    )
    .await
    .context("initialize timed out")??;

    let value = serde_json::to_value(&caps)?;
    let mut entries: Vec<_> = value.as_object().into_iter().flatten().collect();
    entries.sort_by(|a, b| a.0.cmp(b.0));
    println!("capabilities:");
    for (k, v) in entries {
        match v {
            serde_json::Value::Bool(b) => println!("  {k:<45} {}", if *b { "yes" } else { "no" }),
            other => {
                let s = other.to_string();
                let s: String = s.chars().take(60).collect();
                let ellipsis = if s.len() < other.to_string().len() {
                    "…"
                } else {
                    ""
                };
                println!("  {k:<45} {s}{ellipsis}")
            }
        }
    }

    let _ = tokio::time::timeout(
        Duration::from_secs(2),
        proc.client.request(DisconnectArguments::default()),
    )
    .await;
    let _ = proc.child.kill().await;
    Ok(())
}
