//! Fixed, redacted runner failures. No remote or child error text is retained.

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum RunnerError {
    #[error("runner configuration is invalid")]
    Config,
    #[error("runner state is busy")]
    Busy,
    #[error("runner permission denied")]
    Permission,
    #[error("runner contract is unavailable or invalid")]
    Contract,
    #[error("runner transport failed")]
    Transport,
    #[error("runner lease lost")]
    LeaseLost,
    #[error("runner budget exhausted")]
    Budget,
    #[error("runner evidence is invalid")]
    InvalidEvidence,
    #[error("runner proposal is invalid")]
    InvalidProposal,
    #[error("runner accounting is unknown")]
    UnknownAccounting,
    #[error("runner child process failed")]
    Child,
    #[error("runner journal failed")]
    Journal,
    #[error("runner cancelled")]
    Cancelled,
    #[error("runner I/O failed")]
    Io,
}

impl RunnerError {
    /// Preflight/configuration failures are 2, lock contention 3, runtime failures 4.
    pub fn exit_code(&self) -> u8 {
        match self {
            Self::Config | Self::Contract => 2,
            Self::Busy => 3,
            _ => 4,
        }
    }
}
