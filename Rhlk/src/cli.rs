use clap::Parser;

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

    #[arg(value_name = "INPUT")]
    pub inputs: Vec<String>,
}
