//! Logging setup. Logs always go to a file so they never corrupt the
//! interactive terminal.

use std::fs::File;
use std::sync::Mutex;

use tracing_subscriber::EnvFilter;

use crate::args::Args;

/// Enable logging when `--log-dap FILE` or `DDBG_LOG` is set.
///
/// `DDBG_LOG` is an `EnvFilter` directive (e.g. `dap=trace,info`). Without
/// `--log-dap` the log is written to `ddbg.log` in the current directory.
pub fn init(args: &Args) -> anyhow::Result<()> {
    let env = std::env::var("DDBG_LOG").ok();
    if args.log_dap.is_none() && env.is_none() {
        return Ok(());
    }
    let path = args.log_dap.clone().unwrap_or_else(|| "ddbg.log".into());
    let filter = EnvFilter::try_new(env.as_deref().unwrap_or("dap=trace,dap::stderr=debug,info"))?;
    let file = File::create(&path)?;
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_ansi(false)
        .with_writer(Mutex::new(file))
        .init();
    Ok(())
}
