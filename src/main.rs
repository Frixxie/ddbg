#[tokio::main]
async fn main() -> anyhow::Result<()> {
    ddbg_cli::run().await
}
