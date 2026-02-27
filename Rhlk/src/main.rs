use clap::Parser;

fn main() {
    let mut argv = Vec::new();
    for arg in std::env::args_os() {
        if arg == "-an" {
            argv.push("--an".into());
            continue;
        }
        if arg == "-rn" {
            argv.push("--rn".into());
            continue;
        }
        if arg == "-0" {
            argv.push("--g2lk-off".into());
            continue;
        }
        if arg == "-1" {
            argv.push("--g2lk-on".into());
            continue;
        }
        if arg == "-l" {
            argv.push("--use-env-lib".into());
            continue;
        }
        if let Some(s) = arg.to_str() {
            if s.starts_with("-l") && s.len() > 2 && !s.starts_with("--") {
                argv.push("-l".into());
                argv.push(s[2..].into());
                continue;
            }
        }
        argv.push(arg);
    }
    let mut args = rhlk::cli::Args::parse_from(argv.clone());
    if args.g2lk_off && args.g2lk_on {
        eprintln!("--g2lk-off and --g2lk-on are mutually exclusive");
        std::process::exit(2);
    }
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
