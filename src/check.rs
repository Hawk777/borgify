//! A lightweight check for whether a repository is properly initialized and a proper passphrase
//! has been provided.

use serde::Deserialize;
use std::borrow::Cow;
use std::fmt::{Display, Formatter};
use std::io::{BufRead, BufReader};
use std::os::unix::io::{AsFd as _, AsRawFd as _};
use std::os::unix::process::ExitStatusExt as _;
use std::process::{Command, Stdio};

/// The possible errors from checking a repository.
#[derive(Debug)]
pub enum Error {
	/// A passphrase is needed and was not provided, or the provided passphrase was incorrect.
	Passphrase,

	/// The `borg` executable was invoked successfully and reported some other error regarding the
	/// repository.
	Repository(String),

	/// There was an error spawning or communicating with the `borg` executable.
	Spawn(std::io::Error),

	/// The `borg` executable produced a line of output that is not valid JSON.
	Json(serde_json::Error),

	/// The `borg` executable terminated with exit code 2, indicating an error, but did not print
	/// an error message.
	ErrorStatusWithoutMessage,

	/// The `borg` executable terminated with an exit code other than 0, 1, or 2, which is not
	/// documented as being possible, and did not print an error message.
	UnknownExitCode(i32),

	/// The `borg` executable terminated due to a fatal signal.
	Signal(i32),

	/// The `borg` executable terminated due to an unknown reason (neither normal termination nor a
	/// signal).
	Unknown,
}

impl Display for Error {
	fn fmt(&self, f: &mut Formatter<'_>) -> Result<(), std::fmt::Error> {
		match self {
			Self::Passphrase => write!(f, "incorrect passphrase"),
			Self::Repository(e) => write!(f, "{e}"),
			Self::Spawn(_) => write!(f, "failed to spawn Borg executable"),
			Self::Json(_) => write!(f, "Borg output is invalid JSON"),
			Self::ErrorStatusWithoutMessage => write!(
				f,
				"borg returned exit code 2 (error) without an error message"
			),
			Self::UnknownExitCode(code) => write!(f, "borg returned unknown exit code {code}"),
			Self::Signal(signal) => write!(f, "borg terminated due to signal {signal}"),
			Self::Unknown => write!(f, "borg terminated due to unknown reason"),
		}
	}
}

impl std::error::Error for Error {
	fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
		match self {
			Self::Passphrase
			| Self::Repository(_)
			| Self::ErrorStatusWithoutMessage
			| Self::UnknownExitCode(_)
			| Self::Signal(_)
			| Self::Unknown => None,
			Self::Spawn(e) => Some(e),
			Self::Json(e) => Some(e),
		}
	}
}

impl From<std::io::Error> for Error {
	fn from(e: std::io::Error) -> Self {
		Self::Spawn(e)
	}
}

impl From<serde_json::Error> for Error {
	fn from(e: serde_json::Error) -> Self {
		Self::Json(e)
	}
}

/// A line of output in JSON format that Borg sends to standard error.
#[derive(Clone, Debug, Deserialize, Eq, Hash, PartialEq)]
#[serde(tag = "type")]
enum StderrLine<'data> {
	#[serde(rename = "log_message")]
	LogMessage {
		/// The severity of the event.
		#[serde(rename = "levelname")]
		level: LogLevel,

		/// The formatted message text.
		#[serde(borrow)]
		message: Cow<'data, str>,

		/// The message ID.
		#[serde(rename = "msgid")]
		message_id: Option<MessageId>,
	},

	#[serde(other)]
	Unknown,
}

/// A severity level of a log event.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd)]
enum LogLevel {
	#[serde(rename = "DEBUG")]
	Debug,

	#[serde(rename = "INFO")]
	Info,

	#[serde(rename = "WARNING")]
	Warning,

	#[serde(rename = "ERROR")]
	Error,

	#[serde(rename = "CRITICAL")]
	Critical,

	#[serde(other)]
	Unknown,
}

/// A message ID.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq)]
enum MessageId {
	/// The repository is encrypted and the passphrase is incorrect.
	PassphraseWrong,

	/// Any other message.
	#[serde(other)]
	Unknown,
}

/// Handles output generated by a child process.
fn handle_output(mut stderr: impl BufRead) -> Result<(), Error> {
	let mut line_buffer = String::new();
	let mut first_non_passphrase_error: Option<String> = None;
	let mut seen_passphrase_wrong_error = false;
	loop {
		line_buffer.clear();
		if stderr.read_line(&mut line_buffer)? == 0 {
			break;
		}
		let line: StderrLine = serde_json::from_str(&line_buffer)?;
		match line {
			StderrLine::LogMessage {
				message_id: Some(MessageId::PassphraseWrong),
				..
			} => {
				seen_passphrase_wrong_error = true;
			}
			StderrLine::LogMessage { level, message, .. } if level >= LogLevel::Error => {
				first_non_passphrase_error.get_or_insert(message.into_owned());
			}
			_ => (),
		}
	}
	if let Some(e) = first_non_passphrase_error {
		Err(Error::Repository(e))
	} else if seen_passphrase_wrong_error {
		Err(Error::Passphrase)
	} else {
		Ok(())
	}
}

/// Tests `handle_output` with no lines.
#[test]
fn test_handle_output_empty() {
	const OUTPUT: &[u8] = b"";
	match handle_output(OUTPUT) {
		Ok(()) => (),
		Err(e) => panic!("unexpected error {e}"),
	}
}

/// Tests `handle_output` with a debug-level log message.
///
/// The message should not affect the result; the check should pass.
#[test]
fn test_handle_output_debug() {
	const OUTPUT: &[u8] = br#"{"message": "35 self tests completed in 0.08 seconds", "type": "log_message", "created": 1488278449.5575905, "levelname": "DEBUG", "name": "borg.archiver"}"#;
	match handle_output(OUTPUT) {
		Ok(()) => (),
		Err(e) => panic!("unexpected error {e}"),
	}
}

/// Tests `handle_output` with an invalid passphrase log message.
#[test]
fn test_handle_output_passphrase() {
	const OUTPUT: &[u8] = br#"{"type": "log_message", "time": 1673159674.6615226, "message": "passphrase supplied in BORG_PASSPHRASE, by BORG_PASSCOMMAND or via BORG_PASSPHRASE_FD is incorrect.", "levelname": "ERROR", "name": "borg.archiver", "msgid": "PassphraseWrong"}"#;
	match handle_output(OUTPUT) {
		Ok(()) => panic!("unexpected success"),
		Err(Error::Passphrase) => (),
		Err(e) => panic!("unexpected error {e}"),
	}
}

/// Tests `handle_output` with a different error.
#[test]
fn test_handle_output_error() {
	const OUTPUT: &[u8] = br#"{"type": "log_message", "time": 1673159749.4641619, "message": "Repository /some/path does not exist.", "levelname": "ERROR", "name": "borg.archiver", "msgid": "Repository.DoesNotExist"}"#;
	match handle_output(OUTPUT) {
		Ok(()) => panic!("unexpected success"),
		Err(Error::Repository(msg)) if msg == "Repository /some/path does not exist." => (),
		Err(e) => panic!("unexpected error {e}"),
	}
}

/// Tests `handle_output` with a debug message followed by an error.
#[test]
fn test_handle_output_debug_and_error() {
	const OUTPUT: &[u8] = br#"{"message": "35 self tests completed in 0.08 seconds", "type": "log_message", "created": 1488278449.5575905, "levelname": "DEBUG", "name": "borg.archiver"}
{"type": "log_message", "time": 1673159749.4641619, "message": "Repository /some/path does not exist.", "levelname": "ERROR", "name": "borg.archiver", "msgid": "Repository.DoesNotExist"}"#;
	match handle_output(OUTPUT) {
		Ok(()) => panic!("unexpected success"),
		Err(Error::Repository(msg)) if msg == "Repository /some/path does not exist." => (),
		Err(e) => panic!("unexpected error {e}"),
	}
}

/// Tests `handle_output` with two errors.
#[test]
fn test_handle_output_two_errors() {
	const OUTPUT: &[u8] = br#"{"type": "log_message", "time": 1673159749.4641619, "message": "The first message", "levelname": "ERROR", "name": "borg.archiver"}
{"type": "log_message", "time": 1673159749.4641619, "message": "The second message", "levelname": "ERROR", "name": "borg.archiver"}"#;
	match handle_output(OUTPUT) {
		Ok(()) => panic!("unexpected success"),
		Err(Error::Repository(msg)) if msg == "The first message" => (),
		Err(e) => panic!("unexpected error {e}"),
	}
}

/// Tests `handle_output` with a passphrase error and some other error.
///
/// The other error is considered more important.
#[test]
fn test_handle_output_passphrase_and_other_error() {
	const OUTPUT: &[u8] = br#"{"type": "log_message", "time": 1673159674.6615226, "message": "passphrase supplied in BORG_PASSPHRASE, by BORG_PASSCOMMAND or via BORG_PASSPHRASE_FD is incorrect.", "levelname": "ERROR", "name": "borg.archiver", "msgid": "PassphraseWrong"}
{"type": "log_message", "time": 1673159749.4641619, "message": "The second message", "levelname": "ERROR", "name": "borg.archiver"}"#;
	match handle_output(OUTPUT) {
		Ok(()) => panic!("unexpected success"),
		Err(Error::Repository(msg)) if msg == "The second message" => (),
		Err(e) => panic!("unexpected error {e}"),
	}
}

/// Tests `handle_output` with a line of invalid JSON.
#[test]
fn test_handle_output_invalid_json() {
	const OUTPUT: &[u8] = b"{";
	match handle_output(OUTPUT) {
		Ok(()) => panic!("unexpected success"),
		Err(Error::Json(_)) => (),
		Err(e) => panic!("unexpected error {e}"),
	}
}

/// Tries to examine a repository and verify that it exists and is accessible with a given
/// passphrase.
pub fn run(repository: &str, passphrase: Option<&str>, umask: u16) -> Result<(), Error> {
	// If no passphrase is provided, then use an arbitrary passphrase. If it fails, it will fail
	// with an “incorrect passphrase” error, which is exactly what we want when a passphrase is
	// required and was not given. If the repository is unencrypted, then it will succeed because
	// the passphrase is entirely ignored. This is weird, but is actually the Borg-recommended way
	// to check whether a repository is encrypted or not.
	let passphrase = passphrase.unwrap_or("f1ba7f94-7bb5-4a55-8877-7afe3b280f4b");
	let passphrase_pipe_reader = super::passphrase::send_to_inheritable_pipe(passphrase)?;

	// Spawn the process.
	let mut child = Command::new("borg")
		.arg("--log-json")
		.arg("--umask")
		.arg(format!("0{umask:o}"))
		.arg("info")
		.env(
			"BORG_PASSPHRASE_FD",
			format!("{}", passphrase_pipe_reader.as_fd().as_raw_fd()),
		)
		.env("BORG_REPO", repository)
		.stdin(Stdio::null())
		.stdout(Stdio::null())
		.stderr(Stdio::piped())
		.spawn()?;

	// Drop the pipe reader now that the child has a copy of it, ensuring we don’t keep open FDs
	// around longer than necessary.
	drop(passphrase_pipe_reader);

	// Deal with the output.
	let ret = handle_output(BufReader::new(child.stderr.take().unwrap()));

	// If the result was an I/O error or invalid JSON, the child process may not have finished yet,
	// so try to clean up by killing it.
	match ret {
		Err(Error::Spawn(_)) | Err(Error::Json(_)) => {
			// Best effort attempt at cleaning up; if the kill attempt fails, there’s not much
			// useful we can do (and it might have failed because the child died anyway, in which
			// case no problem).
			let _ = child.kill();
		}
		_ => (),
	}

	// Wait and collect exit status.
	let status = child.wait()?;

	// If handle_output reported an error, that is the most detailed information we can provide. If
	// it did not, consider the exit status.
	ret?;

	if let Some(code) = status.code() {
		// The process terminated normally.
		match code {
			0 | 1 => {
				// Borg returned success or warning, but not error or above.
				Ok(())
			}
			2 => {
				// Borg returned an error. We shouldn’t really get here; Borg should have printed
				// an ERROR-level log message and so we should have reported that instead.
				Err(Error::ErrorStatusWithoutMessage)
			}
			_ => {
				// Borg returned an exit code it is not documented as being able to return.
				Err(Error::UnknownExitCode(code))
			}
		}
	} else if let Some(signal) = status.signal() {
		// The process terminated with a signal.
		Err(Error::Signal(signal))
	} else {
		// The process terminated for an unknown reason.
		Err(Error::Unknown)
	}
}
