//! `ddbg adapter-test`: initialize an adapter and print its capabilities.

use std::time::Duration;

use anyhow::Context;
use ddbg_core::adapter::adapter_for_command;
use ddbg_dap::protocol::{DisconnectArguments, InitializeArguments};

pub async fn run(adapter: &[String]) -> anyhow::Result<()> {
    let (program, args) = adapter.split_first().context("missing adapter command")?;
    let adapter = adapter_for_command(program, args.to_vec());
    println!("adapter: {}", adapter.command()?.program);

    let mut conn = adapter.connect()?;
    let caps = tokio::time::timeout(
        Duration::from_secs(10),
        conn.client.request(InitializeArguments::new(adapter.id())),
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
        conn.client.request(DisconnectArguments::default()),
    )
    .await;
    if let Some(child) = conn.child.as_mut() {
        let _ = child.kill().await;
    }
    Ok(())
}
