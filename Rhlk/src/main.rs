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
    let args = rhlk::cli::Args::parse_from(argv);
    if let Err(err) = rhlk::run(args) {
        eprintln!("{err}");
        std::process::exit(1);
    }
}
