use anyhow::Result;
use clap::Parser;

mod app;
mod config;
mod editor;
mod github;
mod ui;

#[derive(Parser, Debug)]
#[command(name = "hxpr")]
#[command(about = "TUI for GitHub PR review, designed for Helix editor users")]
#[command(version)]
struct Args {
    /// Repository name (e.g., "owner/repo")
    #[arg(short, long)]
    repo: String,

    /// Pull request number
    #[arg(short, long)]
    pr: u32,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let config = config::Config::load()?;
    let mut app = app::App::new(&args.repo, args.pr, config).await?;
    app.run().await
}
