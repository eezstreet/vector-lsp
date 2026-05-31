use clap::Parser;

#[derive(Parser, Debug)]
pub struct CliArgs {
    #[arg(long, default_value = "config.json")]
    pub config_file: String,
}