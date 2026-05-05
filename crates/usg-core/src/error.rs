use std::fmt;
use std::io;

pub type Result<T> = std::result::Result<T, GeneratorError>;

#[derive(Debug)]
pub enum GeneratorError {
    Io { path: String, source: io::Error },
    Lockfile(LockfileError),
    MissingTemplate(String),
    NoSolutionFound(String),
    NoProjectsInSolution(String),
    DuplicateAsmDefName(String),
    NoTemplatesFound(String),
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
            GeneratorError::MissingTemplate(p) => write!(f, "Missing template file: {}", p),
            GeneratorError::NoSolutionFound(p) => write!(f, "No .sln file found in: {}", p),
            GeneratorError::NoProjectsInSolution(p) => {
                write!(f, "No C# projects found in solution: {}", p)
            }
            GeneratorError::DuplicateAsmDefName(n) => write!(f, "Duplicate asmdef name: '{}'", n),
            GeneratorError::NoTemplatesFound(p) => write!(
                f,
                "No templates found in: {}\nRun 'unity-solution-generator lock <unity-root>' to generate a lockfile instead.",
                p
            ),
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

pub(crate) fn io_err(path: impl Into<String>, source: io::Error) -> GeneratorError {
    GeneratorError::Io {
        path: path.into(),
        source,
    }
}
