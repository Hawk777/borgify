mod backup;
mod btrfs;
mod check;
mod config;
mod passphrase;

use nix::libc;
use std::collections::hash_map::{Entry, HashMap};
use std::fmt::{Display, Formatter};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

/// The errors that can occur in the main application.
#[derive(Debug)]
enum Error {
	/// An error occurred loading the config file.
	ConfigLoad(std::io::Error),

	/// An error occurred parsing the config file.
	ConfigParse(serde_json::Error),

	/// An error occurred reading a passphrase from the terminal.
	ReadPassphrase(std::io::Error),

	/// An error occurred checking a repository.
	CheckRepository(String, check::Error),

	/// An error occurred examining an archive root.
	CheckArchiveRoot(PathBuf, std::io::Error),

	/// An error occurred performing a backup.
	Backup(String, backup::Error),
}

impl Display for Error {
	fn fmt(&self, f: &mut Formatter<'_>) -> Result<(), std::fmt::Error> {
		match self {
			Self::ConfigLoad(_) => "error loading config file".fmt(f),
			Self::ConfigParse(_) => "error parsing config file".fmt(f),
			Self::ReadPassphrase(_) => "error obtaining passphrase from terminal".fmt(f),
			Self::CheckRepository(url, _) => write!(f, "error checking repository {url}"),
			Self::CheckArchiveRoot(p, _) => {
				write!(f, "error checking archive root directory {}", p.display())
			}
			Self::Backup(a, _) => write!(f, "error backing up archive {a}"),
		}
	}
}

impl std::error::Error for Error {
	fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
		match self {
			Self::ConfigLoad(e) => Some(e),
			Self::ConfigParse(e) => Some(e),
			Self::ReadPassphrase(e) => Some(e),
			Self::CheckRepository(_, e) => Some(e),
			Self::CheckArchiveRoot(_, e) => Some(e),
			Self::Backup(_, e) => Some(e),
		}
	}
}

/// Tries to examine a repository. If a passphrase is needed, asks for the passphrase and
/// re-examines the repository to verify the passphrase.
fn check_repository_and_query_passphrase(repository: &str) -> Result<Option<String>, Error> {
	let mut pw: Option<String> = None;
	loop {
		match check::run(repository, pw.as_deref()) {
			Ok(()) => break Ok(pw),
			Err(check::Error::Passphrase) => {
				if pw.is_some() {
					eprintln!("Passphrase is incorrect.");
				}
				pw = Some(
					passphrase::read(&format!("Passphrase for repository {repository}:"))
						.map_err(Error::ReadPassphrase)?,
				)
			}
			Err(e) => break Err(Error::CheckRepository(repository.to_owned(), e)),
		}
	}
}

/// Checks that a specified archive root is a directory.
fn check_archive_root(root: &Path) -> std::io::Result<()> {
	let md = std::fs::metadata(root)?;
	if md.is_dir() {
		Ok(())
	} else {
		Err(std::io::Error::from_raw_os_error(libc::ENOTDIR))
	}
}

/// The top-level application logic.
fn run() -> Result<ExitCode, Error> {
	// Load the config file.
	let config = std::fs::read("/etc/borgify.json").map_err(Error::ConfigLoad)?;
	let config: config::Config = serde_json::from_slice(&config).map_err(Error::ConfigParse)?;

	// Check all the archives, collecting passwords for each one that needs one.
	let passphrases: HashMap<&str, Option<String>> = {
		let mut passphrases: HashMap<&str, Option<String>> = HashMap::new();
		for archive in config.archives.values() {
			if let Entry::Vacant(entry) = passphrases.entry(&archive.repository) {
				entry.insert(check_repository_and_query_passphrase(&archive.repository)?);
			}
		}
		passphrases
	};

	// Check that all the repository roots exist.
	for archive in config.archives.values() {
		check_archive_root(&archive.root)
			.map_err(|e| Error::CheckArchiveRoot(archive.root.clone().into_owned(), e))?;
	}

	// Run the backup processes.
	let timestamp_utc = chrono::Utc::now();
	let timestamp_local = timestamp_utc.with_timezone(&chrono::Local);
	let timestamp_utc = format!("{}", timestamp_utc.format("%FT%T"));
	let timestamp_local = format!("{}", timestamp_local.format("%FT%T"));
	let mut any_warnings = false;
	for (name, archive) in &config.archives {
		println!("===== Backing up archive {name} =====");
		any_warnings |= backup::run(
			name,
			archive,
			&timestamp_utc,
			&timestamp_local,
			passphrases
				.get(&*archive.repository)
				.expect("passphrase missing from map, but we already examined every repository")
				.as_deref(),
		)
		.map_err(|e| Error::Backup(name.clone().into_owned(), e))?;
		println!();
	}

	Ok(ExitCode::from(u8::from(any_warnings)))
}

fn main() -> ExitCode {
	match run() {
		Ok(code) => code,
		Err(e) => {
			fn show_error_stack(e: &(dyn std::error::Error + 'static), first: bool) {
				eprintln!("{}{e}", if first { "" } else { "caused by: " });
				if let Some(source) = e.source() {
					show_error_stack(source, false);
				}
			}
			show_error_stack(&e, true);
			2.into()
		}
	}
}
