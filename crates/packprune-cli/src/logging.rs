//! The `tracing` subscriber: to stderr, results stay on stdout. `-v` shows each stage's
//! wall time, `-vv` more, `-q` only errors; `PACKPRUNE_LOG` (an `EnvFilter` directive) overrides both.

use std::io::{self, IsTerminal};

use tracing_subscriber::EnvFilter;

const ENV: &str = "PACKPRUNE_LOG";

/// The filter a command line gives when `PACKPRUNE_LOG` does not.
fn directive(verbose: u8, quiet: bool) -> &'static str {
    match (quiet, verbose) {
        (true, _) => "error",
        (false, 0) => "warn",
        (false, 1) => "info",
        (false, 2) => "debug",
        (false, _) => "trace",
    }
}

pub fn init(verbose: u8, quiet: bool) {
    install(directive(verbose, quiet));
}

/// The drop-in's stderr is Nexo's log: a line without PackSquash's mark would be dropped or misread there, so it
/// stays silent unless `PACKPRUNE_LOG` asks.
pub fn init_drop_in() {
    install("off");
}

fn install(default: &str) {
    let filter = EnvFilter::try_from_env(ENV).unwrap_or_else(|_| EnvFilter::new(default));
    // A second call (a test binary) keeps the first subscriber.
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(io::stderr)
        .with_ansi(io::stderr().is_terminal() && std::env::var_os("NO_COLOR").is_none())
        .with_target(false)
        .without_time()
        .try_init();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verbosity_picks_the_level() {
        assert_eq!(directive(0, false), "warn");
        assert_eq!(directive(1, false), "info");
        assert_eq!(directive(2, false), "debug");
        assert_eq!(directive(5, false), "trace");
        assert_eq!(directive(3, true), "error");
    }
}
