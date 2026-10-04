//! Shared implementation for the `hilo` CLI.

pub mod cli;
pub mod commands;

/// Shared sync-direction vocabulary: `hilo workspace sync` and
/// `hilo backend sync` speak the same --push/--pull/--both flags.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncDirectionArg {
    Push,
    Pull,
    Both,
}

impl SyncDirectionArg {
    /// The exact string printed on the run header / plan lines.
    pub(crate) fn label(self) -> &'static str {
        match self {
            SyncDirectionArg::Push => "push",
            SyncDirectionArg::Pull => "pull",
            SyncDirectionArg::Both => "two-way",
        }
    }
}

/// Resolve --push / --pull / --both (default two-way).
pub fn sync_direction(push: bool, pull: bool, _both: bool) -> SyncDirectionArg {
    if push {
        SyncDirectionArg::Push
    } else if pull {
        SyncDirectionArg::Pull
    } else {
        SyncDirectionArg::Both
    }
}
