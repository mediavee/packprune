//! Print the generated configuration schema.

use std::io::{self, Write};

use crate::ExitClass;

#[derive(Debug, clap::Args)]
pub struct Args {}

pub fn run(_args: &Args) -> ExitClass {
    match writeln!(
        io::stdout().lock(),
        "{:#}",
        packprune_config::schema().as_value()
    ) {
        Ok(()) => ExitClass::Clean,
        Err(error) => {
            eprintln!("packprune schema: {error}");
            ExitClass::Error
        }
    }
}
