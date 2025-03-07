use crate::Result;
use crate::cli::Cli;
use clap::CommandFactory;

/// Generates a usage spec for the CLI
///
/// https://usage.jdx.dev
#[derive(Debug, clap::Args)]
#[clap(hide = true, verbatim_doc_comment)]
pub struct Usage {}

impl Usage {
    pub async fn run(&self) -> Result<()> {
        let mut cmd = Cli::command();
        clap_usage::generate(&mut cmd, "hk", &mut std::io::stdout());
        println!("{}", include_str!("../hk-extras.usage.kdl"));
        Ok(())
    }
}
