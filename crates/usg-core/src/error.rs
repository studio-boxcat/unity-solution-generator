use std::fmt;
use std::io;

pub type Result<T> = std::result::Result<T, GeneratorError>;

#[derive(Debug)]
pub enum GeneratorError {
    Io { path: String, source: io::Error },
    Lockfile(LockfileError),
    DuplicateAsmDefName(String),
    /// Watchman daemon unreachable. Carries the original "install watchman"
    /// hint so the FFI host (BoxcatBridge) can surface it verbatim instead of
    /// string-matching on `Other`.
    ScanUnavailable(String),
    Other(String),
}

#[derive(Debug)]
pub enum LockfileError {
    NoProjectVersion(String),
    UnityNotFound(String),
    InvalidLockfile(String),
}

impl fmt::Display for GeneratorError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            GeneratorError::Io { path, source } => write!(f, "{}: {}", source, path),
            GeneratorError::Lockfile(e) => write!(f, "{}", e),
            GeneratorError::DuplicateAsmDefName(n) => write!(f, "Duplicate asmdef name: '{}'", n),
            GeneratorError::ScanUnavailable(msg) => write!(f, "{}", msg),
            GeneratorError::Other(msg) => write!(f, "{}", msg),
        }
    }
}

impl fmt::Display for LockfileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LockfileError::NoProjectVersion(p) => {
                write!(f, "Cannot find ProjectSettings/ProjectVersion.txt in: {}", p)
            }
            LockfileError::UnityNotFound(p) => write!(f, "Unity installation not found at: {}", p),
            LockfileError::InvalidLockfile(r) => write!(f, "Invalid lockfile: {}", r),
        }
    }
}

impl std::error::Error for GeneratorError {}
impl std::error::Error for LockfileError {}

impl From<LockfileError> for GeneratorError {
    fn from(e: LockfileError) -> Self {
        GeneratorError::Lockfile(e)
    }
}

pub fn io_err(path: impl Into<String>, source: io::Error) -> GeneratorError {
    GeneratorError::Io {
        path: path.into(),
        source,
    }
}
