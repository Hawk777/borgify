//! Actually performs a backup.

use super::{btrfs, config};
use nix::libc;
use std::ffi::{c_int, CStr, CString, OsStr};
use std::fmt::{Display, Formatter, LowerHex};
use std::fs::File;
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::fs::OpenOptionsExt as _;
use std::os::unix::io::{AsFd as _, AsRawFd as _};
use std::os::unix::prelude::*;
use std::process::Command;

/// The errors that can occur.
#[derive(Debug)]
pub enum Error {
	/// The archive root location cannot be opened.
	OpenArchiveRoot(std::io::Error),

	/// The parent directory of the archive root location cannot be opened.
	OpenArchiveRootParent(std::io::Error),

	/// The created snapshot cannot be opened.
	OpenSnapshot(std::io::Error),

	/// An error occurred creating a btrfs snapshot.
	SnapshotCreate(btrfs::Error),

	/// An error occurred deleting a btrfs snapshot.
	SnapshotDelete(btrfs::Error),

	/// There was an error spawning or communicating with the `borg` executable.
	Spawn(std::io::Error),

	/// The `borg` executable terminated with exit code 2, indicating an error.
	#[allow(clippy::enum_variant_names)] // Not the enum name, but the specific kind of exit.
	ErrorStatus,

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
			Self::OpenArchiveRoot(_) => "error opening archive root directory".fmt(f),
			Self::OpenArchiveRootParent(_) => {
				"error opening archive root’s parent directory".fmt(f)
			}
			Self::OpenSnapshot(_) => "error opening created btrfs snapshot".fmt(f),
			Self::SnapshotCreate(_) => "error creating btrfs snapshot".fmt(f),
			Self::SnapshotDelete(_) => "error deleting btrfs snapshot".fmt(f),
			Self::Spawn(_) => "failed to spawn Borg executable".fmt(f),
			Self::ErrorStatus => {
				"borg returned exit code 2 (error) without an error message".fmt(f)
			}
			Self::UnknownExitCode(code) => write!(f, "borg returned unknown exit code {code}"),
			Self::Signal(signal) => write!(f, "borg terminated due to signal {signal}"),
			Self::Unknown => write!(f, "borg terminated due to unknown reason"),
		}
	}
}

impl std::error::Error for Error {
	fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
		match self {
			Self::ErrorStatus | Self::UnknownExitCode(_) | Self::Signal(_) | Self::Unknown => None,
			Self::OpenArchiveRoot(e) => Some(e),
			Self::OpenArchiveRootParent(e) => Some(e),
			Self::OpenSnapshot(e) => Some(e),
			Self::SnapshotCreate(e) => Some(e),
			Self::SnapshotDelete(e) => Some(e),
			Self::Spawn(e) => Some(e),
		}
	}
}

/// A slice of bytes that can be formatted in hex.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
struct FormattableSlice<'a>(&'a [u8]);

impl LowerHex for FormattableSlice<'_> {
	fn fmt(&self, f: &mut Formatter) -> Result<(), std::fmt::Error> {
		for b in self.0 {
			write!(f, "{b:02x}")?;
		}
		Ok(())
	}
}

/// Performs an [`openat`](libc::openat) call safely.
fn openat(
	dirfd: impl AsFd,
	pathname: impl AsRef<CStr>,
	flags: c_int,
	mode: libc::mode_t,
) -> std::io::Result<File> {
	// SAFETY: The parameters to this wrapper are of data types which ensure proper memory safety.
	let ret = unsafe {
		libc::openat(
			dirfd.as_fd().as_raw_fd(),
			pathname.as_ref().as_ptr(),
			flags,
			mode,
		)
	};
	if ret < 0 {
		Err(std::io::Error::last_os_error())
	} else {
		// SAFETY: openat promises to return a brand new file descriptor.
		Ok(unsafe { File::from_raw_fd(ret) })
	}
}

/// Performs a backup, given a snapshot if applicable.
///
/// On success, returns whether any warnings were generated.
fn run_with_root(
	archive_name: &str,
	archive: &config::Archive,
	timestamp_utc: &str,
	timestamp_local: &str,
	passphrase: Option<&str>,
	root: impl AsFd,
	umask: u16,
) -> Result<bool, Error> {
	// Launch Borg.
	let mut child = Command::new("borg");
	let root = root.as_fd().as_raw_fd();
	// SAFETY: The lambda just calls fchdir, which is documented as signal-safe.
	unsafe {
		child.pre_exec(move || {
			// Allow SIGINT to reach the borg process.
			// SAFETY: The passed-in parameters are locally constructed properly.
			libc::signal(libc::SIGINT, libc::SIG_DFL);

			// SAFETY: The root parameter (of type impl AsFd) lives for the duration of
			// run_with_root, which, if it successfully spawns the child, has created a new process
			// in which the descriptor remains valid even if closed in the parent.
			let ret = libc::fchdir(root);
			if ret < 0 {
				Err(std::io::Error::last_os_error())
			} else {
				Ok(())
			}
		});
	}
	child
		.args([
			"--verbose",
			"--progress",
			"--iec",
			"--umask",
			&format!("0{umask:o}"),
			"create",
			"--stats",
			"--exclude-caches",
			"--timestamp",
			timestamp_utc,
			"--compression",
			&archive.compression,
		])
		.args(archive.patterns.iter().map(|i| format!("--pattern={i}")))
		.arg(format!("::{archive_name}-{timestamp_local}"))
		.arg(".")
		.env("BORG_REPO", OsStr::new(archive.repository.as_ref()))
		.env("BORG_FILES_CACHE_SUFFIX", archive_name);
	let passphrase_pipe_reader = if let Some(passphrase) = passphrase {
		let passphrase_pipe_reader =
			super::passphrase::send_to_inheritable_pipe(passphrase).map_err(Error::Spawn)?;
		child.env(
			"BORG_PASSPHRASE_FD",
			format!("{}", passphrase_pipe_reader.as_fd().as_raw_fd()),
		);
		Some(passphrase_pipe_reader)
	} else {
		None
	};
	let mut child = child.spawn().map_err(Error::Spawn)?;

	// Drop the pipe reader now that the child has a copy of it, ensuring we don’t keep open FDs
	// around longer than necessary.
	drop(passphrase_pipe_reader);

	// Wait and collect exit status.
	let status = child.wait().map_err(Error::Spawn)?;
	if let Some(code) = status.code() {
		// The process terminated normally.
		match code {
			0 => Ok(false),                         // Borg returned success.
			1 => Ok(true),                          // Borg returned success with a warning.
			2 => Err(Error::ErrorStatus),           // Borg returned error.
			_ => Err(Error::UnknownExitCode(code)), // Borg returned an exit code it is not documented as being able to return.
		}
	} else if let Some(signal) = status.signal() {
		// The process terminated with a signal.
		Err(Error::Signal(signal))
	} else {
		// The process terminated for an unknown reason.
		Err(Error::Unknown)
	}
}

/// Information about an existent snapshot.
struct Snapshot {
	/// Whether any warnings were generated while creating the snapshot.
	pub warnings: bool,

	/// The file descriptor of the parent directory containing the snapshot and its source.
	pub parent: File,

	/// The file descriptor of the snapshot itself.
	pub snapshot_fd: File,
}

impl Snapshot {
	/// Creates a btrfs snapshot at a sibling location to the source path, with a generated name.
	///
	/// On success, returns whether any warnings were generated, and the path to the snapshot.
	fn create(source: &File, hash_seed: &[u8]) -> Result<Self, Error> {
		// Open the parent directory of the archive root.
		let parent =
			openat(source, c"..", libc::O_DIRECTORY, 0).map_err(Error::OpenArchiveRootParent)?;

		// Try to create a “randomly” (actually an SHA256 of a seed value and a counter) named
		// subvolume, repeatedly, until we don’t collide with an existing name.
		let mut any_warnings = false;
		let mut hash_base = hmac_sha256::Hash::new();
		hash_base.update(hash_seed);
		let hash_base = hash_base;
		for i in u64::MIN..=u64::MAX {
			let mut hash = hash_base;
			hash.update(i.to_le_bytes());
			let hash = hash.finalize();
			let snapshot_name = format!("{:x}", FormattableSlice(&hash));
			match btrfs::create_snapshot(source, &parent, &snapshot_name) {
				Ok(()) => {
					let snapshot_fd = openat(
						&parent,
						CString::new(snapshot_name)
							.expect("hex-encoded hash contains embedded NUL"),
						libc::O_DIRECTORY | libc::O_NOFOLLOW,
						0,
					)
					.map_err(Error::OpenSnapshot)?;
					return Ok(Self {
						warnings: any_warnings,
						parent,
						snapshot_fd,
					});
				}
				Err(btrfs::Error::Syscall(e)) if e.kind() == std::io::ErrorKind::AlreadyExists => {
					// A subvolume with this name already exists. Given how we generate snapshot
					// subvolume paths, that’s unlikely to be something the user legitimately
					// created (more likely something created by a previous invocation of this tool
					// that failed to delete it), so we should probably warn about it, but we
					// shouldn’t do anything else to it; instead, just increment “i” and try
					// generating a new name.
					eprintln!(
						"WARNING: Snapshot {} already exists; trying another name",
						snapshot_name
					);
					any_warnings = true;
				}
				Err(e) => return Err(Error::SnapshotCreate(e)),
			}
		}
		panic!("tried 2⁶⁴ filenames without finding a nonexistent one, which is impossible");
	}

	/// Deletes a snapshot.
	fn delete(self) -> Result<(), Error> {
		btrfs::delete_subvolume(self.parent, self.snapshot_fd).map_err(Error::SnapshotDelete)
	}
}

/// Creates a btrfs snapshot, performs the backup, and deletes the snapshot.
///
/// On success, returns whether any warnings were generated.
fn do_snapshot(
	archive_name: &str,
	archive: &config::Archive,
	timestamp_utc: &str,
	timestamp_local: &str,
	passphrase: Option<&str>,
	archive_root: &File,
	umask: u16,
) -> Result<bool, Error> {
	// Create a snapshot at a unique path which is a sibling to the root.
	let snapshot = Snapshot::create(archive_root, archive.root.as_os_str().as_bytes())?;
	let snapshot_warnings = snapshot.warnings;

	// Run the backup using the snapshot as the archive root.
	let backup_result = run_with_root(
		archive_name,
		archive,
		timestamp_utc,
		timestamp_local,
		passphrase,
		&snapshot.snapshot_fd,
		umask,
	);

	// Delete the snapshot.
	let delete_snapshot_result = snapshot.delete();

	match (backup_result, delete_snapshot_result) {
		(Ok(any_warnings_running_backup), Ok(())) => {
			Ok(snapshot_warnings || any_warnings_running_backup)
		}
		(Ok(_), Err(e)) => Err(e),
		(Err(e), Ok(())) => Err(e),
		// If both failed, the error from doing the backup is more important.
		(Err(backup_error), Err(_)) => Err(backup_error),
	}
}

/// Performs a backup.
///
/// On success, returns whether any warnings were generated.
pub fn run(
	archive_name: &str,
	archive: &config::Archive,
	timestamp_utc: &str,
	timestamp_local: &str,
	passphrase: Option<&str>,
	umask: u16,
) -> Result<bool, Error> {
	let archive_root = File::options()
		.read(true)
		.custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW)
		.open(&archive.root)
		.map_err(Error::OpenArchiveRoot)?;
	if archive.btrfs_snapshot {
		do_snapshot(
			archive_name,
			archive,
			timestamp_utc,
			timestamp_local,
			passphrase,
			&archive_root,
			umask,
		)
	} else {
		run_with_root(
			archive_name,
			archive,
			timestamp_utc,
			timestamp_local,
			passphrase,
			archive_root,
			umask,
		)
	}
}
