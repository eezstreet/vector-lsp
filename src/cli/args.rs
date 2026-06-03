use clap::Parser;

#[derive(Parser, Debug)]
pub struct CliArgs {
    #[arg(long, default_value = "config.json")]
    pub config_file: String,
    /// Validate the workspace and exit (overrides the config `single_shot` setting).
    #[arg(long)]
    pub single_shot: bool,
}