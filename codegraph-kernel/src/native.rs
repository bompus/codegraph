//! Owned data and errors for the runtime-independent kernel build.
pub type Buffer = Vec<u8>;
pub type Result<T> = std::result::Result<T, Error>;
#[derive(Debug)]
pub struct Error(String);
impl Error {
    pub fn from_reason(reason: impl Into<String>) -> Self {
        Self(reason.into())
    }
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}
impl std::error::Error for Error {}
