use ddbg_cli::{Args, Parser, Subcommand};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    if let Some(Subcommand::Mcp) = args.command {
        ddbg_cli::init_logging(&args)?;
        return ddbg_mcp::serve_stdio().await;
    }
    if !args.tui {
        return ddbg_cli::run_with(args).await;
    }
    match ddbg_cli::start(&args).await? {
        Some(prepared) => ddbg_tui::run(prepared).await,
        None => Ok(()),
    }
}
