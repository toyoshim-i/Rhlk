use clap::Parser;

#[derive(Debug, Parser)]
#[command(name = "rhlk")]
pub struct Args {
    #[arg(short = 'o', long = "output")]
    pub output: Option<String>,

    #[arg(short = 'r')]
    pub r_format: bool,

    #[arg(long = "rn")]
    pub r_no_check: bool,

    #[arg(long = "makemcs")]
    pub make_mcs: bool,

    #[arg(long = "omit-bss")]
    pub omit_bss: bool,

    #[arg(long = "verbose", short = 'v')]
    pub verbose: bool,

    #[arg(value_name = "INPUT")]
    pub inputs: Vec<String>,
}
