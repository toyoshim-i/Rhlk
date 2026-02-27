use clap::Parser;
use std::ffi::OsString;

#[derive(Debug, Clone)]
pub struct DefineArg {
    pub name: String,
    pub value: u32,
}

fn parse_u32_with_hex(input: &str) -> Result<u32, String> {
    let s = input.trim();
    if let Some(hex) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        u32::from_str_radix(hex, 16).map_err(|e| format!("invalid hex value '{input}': {e}"))
    } else {
        s.parse::<u32>()
            .map_err(|e| format!("invalid decimal value '{input}': {e}"))
    }
}

fn parse_load_mode(input: &str) -> Result<u8, String> {
    let v = parse_u32_with_hex(input)?;
    if v > 2 {
        return Err(format!("load mode must be 0..2: {input}"));
    }
    Ok(v as u8)
}

fn parse_define_arg(input: &str) -> Result<DefineArg, String> {
    let (name_raw, value_raw) = input
        .split_once('=')
        .ok_or_else(|| format!("define format must be NAME=VALUE: {input}"))?;
    let name = name_raw.trim();
    if name.is_empty() {
        return Err(format!("define name is empty: {input}"));
    }
    let value = parse_u32_with_hex(value_raw.trim())?;
    Ok(DefineArg {
        name: name.to_string(),
        value,
    })
}

fn normalize_argv_from_iter<I>(args: I) -> Vec<OsString>
where
    I: IntoIterator<Item = OsString>,
{
    let mut argv = Vec::new();
    for arg in args {
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
    argv
}

pub fn normalize_argv() -> Vec<OsString> {
    normalize_argv_from_iter(std::env::args_os())
}

pub fn finalize_compat_args(args: &mut Args, argv: &[OsString]) -> Result<(), String> {
    if args.g2lk_off && args.g2lk_on {
        return Err("--g2lk-off and --g2lk-on are mutually exclusive".to_string());
    }
    let verbose_last = argv.iter().rposition(|a| a == "-v" || a == "--verbose");
    let quiet_last = argv.iter().rposition(|a| a == "-z" || a == "--quiet");
    args.verbose = match (verbose_last, quiet_last) {
        (Some(v), Some(z)) => v > z,
        (Some(_), None) => true,
        (None, Some(_)) => false,
        (None, None) => false,
    };
    Ok(())
}

#[derive(Debug, Parser)]
#[command(name = "rhlk", version)]
pub struct Args {
    #[arg(short = 'o', long = "output")]
    pub output: Option<String>,

    #[arg(short = 'r')]
    pub r_format: bool,

    #[arg(long = "rn")]
    pub r_no_check: bool,

    #[arg(short = 'a')]
    pub no_x_ext: bool,

    #[arg(long = "an")]
    pub opt_an: bool,

    #[arg(short = 'e')]
    pub align: Option<u32>,

    #[arg(short = 'b', value_parser = parse_u32_with_hex)]
    pub base_address: Option<u32>,

    #[arg(short = 'g', value_parser = parse_load_mode)]
    pub load_mode: Option<u8>,

    #[arg(short = 'd', value_parser = parse_define_arg)]
    pub defines: Vec<DefineArg>,

    #[arg(short = 'i')]
    pub indirect_files: Vec<String>,

    #[arg(short = 'L')]
    pub lib_paths: Vec<String>,

    #[arg(short = 'l')]
    pub libs: Vec<String>,

    #[arg(long = "use-env-lib")]
    pub use_env_lib: bool,

    #[arg(long = "g2lk-off")]
    pub g2lk_off: bool,

    #[arg(long = "g2lk-on")]
    pub g2lk_on: bool,

    #[arg(long = "makemcs")]
    pub make_mcs: bool,

    #[arg(long = "omit-bss")]
    pub omit_bss: bool,

    #[arg(short = 'x')]
    pub cut_symbols: bool,

    #[arg(short = 'p', long = "map", num_args = 0..=1, default_missing_value = "")]
    pub map: Option<String>,

    #[arg(long = "verbose", short = 'v')]
    pub verbose: bool,

    #[arg(long = "quiet", short = 'z')]
    pub quiet: bool,

    #[arg(short = 'w')]
    pub warn_off: bool,

    #[arg(short = 't')]
    pub title: bool,

    #[arg(short = 's')]
    pub section_info: bool,

    #[arg(value_name = "INPUT")]
    pub inputs: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::{Args, finalize_compat_args, normalize_argv_from_iter};
    use clap::Parser;
    use std::ffi::OsString;

    #[test]
    fn finalizes_verbose_by_last_switch() {
        let cli_argv = vec![
            OsString::from("rhlk"),
            OsString::from("-v"),
            OsString::from("-z"),
        ];
        let mut parsed_args = Args::parse_from(cli_argv.iter().cloned());
        finalize_compat_args(&mut parsed_args, &cli_argv).expect("finalize");
        assert!(!parsed_args.verbose);

        let cli_argv2 = vec![
            OsString::from("rhlk"),
            OsString::from("--quiet"),
            OsString::from("--verbose"),
        ];
        let mut parsed_args2 = Args::parse_from(cli_argv2.iter().cloned());
        finalize_compat_args(&mut parsed_args2, &cli_argv2).expect("finalize");
        assert!(parsed_args2.verbose);
    }

    #[test]
    fn rejects_mutually_exclusive_g2lk_switches() {
        let cli_argv = vec![
            OsString::from("rhlk"),
            OsString::from("--g2lk-off"),
            OsString::from("--g2lk-on"),
        ];
        let mut parsed_args = Args::parse_from(cli_argv.iter().cloned());
        let err = finalize_compat_args(&mut parsed_args, &cli_argv).expect_err("must fail");
        assert!(err.contains("mutually exclusive"));
    }

    #[test]
    fn normalizes_short_l_attached_form() {
        let argv = vec![
            OsString::from("rhlk"),
            OsString::from("-lfoo"),
            OsString::from("main.o"),
        ];
        let out = normalize_argv_from_iter(argv);
        assert_eq!(
            out,
            vec![
                OsString::from("rhlk"),
                OsString::from("-l"),
                OsString::from("foo"),
                OsString::from("main.o"),
            ]
        );
    }
}
