//! What can go wrong when asking git.

use std::time::Duration;

/// Why a git question got no answer.
///
/// The UI tells "no git" ([`GitError::GitNotFound`]) apart from "not a repo"
/// ([`GitError::NotARepository`]) and "git is slow" ([`GitError::Timeout`]),
/// explains it, and keeps going (plan §5.2).
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum GitError {
    /// The `git` program is not installed or not on `PATH`.
    #[error("git was not found; install git or add it to PATH")]
    GitNotFound,
    /// The directory is not inside a git work tree.
    #[error("not inside a git repository")]
    NotARepository,
    /// git did not finish in time and was killed.
    #[error("git did not answer within {} ms", .0.as_millis())]
    Timeout(Duration),
    /// git printed more than the byte limit and was stopped.
    #[error("git printed more than {0} bytes")]
    OutputTooLarge(usize),
    /// git ran and exited with an error.
    #[error("git failed ({}): {message}", exit_code_text(*.code))]
    Failed {
        /// The exit code, `None` when git was ended by a signal.
        code: Option<i32>,
        /// The first line of git's error output, capped.
        message: String,
    },
    /// git could not be started or waited for.
    #[error("could not run git: {0}")]
    Io(#[source] std::io::Error),
    /// git's output did not have the expected shape.
    #[error("unexpected git output: {0}")]
    Parse(#[from] ParseError),
}

fn exit_code_text(code: Option<i32>) -> String {
    code.map_or_else(|| "no exit code".to_owned(), |c| format!("exit code {c}"))
}

/// git printed something the parser does not understand.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum ParseError {
    /// A status record is malformed; holds the record kind and a reason.
    #[error("bad status record ({kind}): {reason}")]
    BadRecord {
        /// The record's leading tag, such as `1`, `2` or `# branch.ab`.
        kind: String,
        /// What was wrong with it.
        reason: &'static str,
    },
    /// `rev-parse` printed fewer lines than were asked for.
    #[error("rev-parse printed {got} lines, expected {expected}")]
    RevParseLines {
        /// Lines received.
        got: usize,
        /// Lines expected.
        expected: usize,
    },
}
