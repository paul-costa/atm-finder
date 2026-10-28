//! Progress messages on stderr.
//!
//! Everything the tool says while working ("Resolving 'Graz' ...", "150 ATMs in Graz") goes to
//! stderr, because stdout may carry the exported CSV when the user writes to `-`. `--quiet`
//! switches the messages off; errors are printed by `main` regardless.

use std::sync::atomic::{AtomicBool, Ordering};

static QUIET: AtomicBool = AtomicBool::new(false);

/// Silence (or re-enable) progress messages for the rest of the run.
pub fn set_quiet(quiet: bool) {
    QUIET.store(quiet, Ordering::Relaxed);
}

/// Print one progress line to stderr unless `--quiet` was given.
pub fn log(msg: &str) {
    if !QUIET.load(Ordering::Relaxed) {
        eprintln!("{msg}");
    }
}
