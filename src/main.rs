use carv::cli::CarveArgs;
use clap::Parser;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let _args = CarveArgs::parse();
    Ok(())
}
