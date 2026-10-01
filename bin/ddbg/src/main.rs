use ddbg_cli::{Args, Parser};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    if !args.tui {
        return ddbg_cli::run_with(args).await;
    }
    match ddbg_cli::start(&args).await? {
        Some(prepared) => ddbg_tui::run(prepared).await,
        None => Ok(()),
    }
}
