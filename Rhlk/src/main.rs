use clap::Parser;

fn main() {
    let argv = std::env::args_os()
        .map(|arg| {
            if arg == "-an" {
                "--an".into()
            } else if arg == "-rn" {
                "--rn".into()
            } else {
                arg
            }
        })
        .collect::<Vec<_>>();
    let mut args = rhlk::cli::Args::parse_from(argv.clone());
    let verbose_last = argv
        .iter()
        .rposition(|a| a == "-v" || a == "--verbose");
    let quiet_last = argv
        .iter()
        .rposition(|a| a == "-z" || a == "--quiet");
    args.verbose = match (verbose_last, quiet_last) {
        (Some(v), Some(z)) => v > z,
        (Some(_), None) => true,
        (None, Some(_)) => false,
        (None, None) => false,
    };
    if let Err(err) = rhlk::run(args) {
        eprintln!("{err}");
        std::process::exit(1);
    }
}
