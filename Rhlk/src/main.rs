use clap::Parser;

fn main() {
    let args = rhlk::cli::Args::parse();
    if let Err(err) = rhlk::run(args) {
        eprintln!("{err}");
        std::process::exit(1);
    }
}
