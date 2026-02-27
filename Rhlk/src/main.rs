use clap::Parser;

fn main() {
    let argv = rhlk::cli::normalize_argv();
    let mut args = rhlk::cli::Args::parse_from(argv.iter().cloned());
    if let Err(err) = rhlk::cli::finalize_compat_args(&mut args, &argv) {
        eprintln!("{err}");
        std::process::exit(2);
    }
    if let Err(err) = rhlk::run(args) {
        eprintln!("{err}");
        std::process::exit(1);
    }
}
