#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppState {
    Idle,
    Recording,
    Processing,
    Error,
}

impl AppState {
    pub fn as_str(&self) -> &'static str {
        match self {
            AppState::Idle => "idle",
            AppState::Recording => "recording",
            AppState::Processing => "processing",
            AppState::Error => "error",
        }
    }
}
