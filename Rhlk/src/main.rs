use clap::Parser;

fn main() {
    let cli_argv = rhlk::cli::normalize_argv();
    let mut parsed_args = rhlk::cli::Args::parse_from(cli_argv.iter().cloned());
    if let Err(err) = rhlk::cli::finalize_compat_args(&mut parsed_args, &cli_argv) {
        eprintln!("{err}");
        std::process::exit(2);
    }
    if let Err(err) = rhlk::run(parsed_args) {
        eprintln!("{err}");
        std::process::exit(1);
    }
}
