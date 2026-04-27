use carv::cli::CarvArgs;
use clap::Parser;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let _args = CarvArgs::parse();
    Ok(())
}
