use clap::Parser;

fn main() -> anyhow::Result<()> {
    let args = rhlk::cli::Args::parse();
    rhlk::run(args)
}
