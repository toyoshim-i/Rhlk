use clap::Parser;

fn parse_u32_with_hex(input: &str) -> Result<u32, String> {
    let s = input.trim();
    if let Some(hex) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        u32::from_str_radix(hex, 16).map_err(|e| format!("invalid hex value '{input}': {e}"))
    } else {
        s.parse::<u32>()
            .map_err(|e| format!("invalid decimal value '{input}': {e}"))
    }
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
