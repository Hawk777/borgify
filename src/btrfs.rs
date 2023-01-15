//! Creation and deletion of btrfs snapshots.

use nix::libc;
use std::ffi::OsStr;
use std::fmt::{Display, Formatter};
use std::fs::File;
use std::mem::MaybeUninit;
use std::os::unix::prelude::*;

/// The raw btrfs ioctls.
mod ioctl {
	/// The ioctl type code for btrfs ioctls.
	const MAGIC: u8 = 0x94;

	/// The maximum length of the name of a subvolume.
	pub const SUBVOL_NAME_MAX: usize = 4039;

	/// The maximum length of the name of a subvolume used in another place.
	pub const VOL_NAME_MAX: usize = 255;

	/// The size of a UUID used with btrfs ioctls.
	pub const UUID_SIZE: usize = 16;

	/// A flag to [`snap_create_v2`](snap_create_v2) to make the new subvolume read-only.
	pub const SUBVOL_RDONLY: u64 = 1 << 1;

	/// A flag to [`snap_destroy_v2`](snap_destroy_v2) to find the subvolume to destroy by
	/// subvolume ID rather than by name.
	pub const SUBVOL_SPEC_BY_ID: u64 = 1 << 4;

	/// A representation of a timestamp in a btrfs ioctl.
	#[derive(Default)]
	#[repr(C)]
	pub struct Timespec {
		pub sec: u64,
		pub nsec: u32,
	}

	/// The identification of an object on which to operate, used as part of [`ArgsV2`](ArgsV2).
	#[repr(C)]
	pub union ArgsV2Identifier {
		pub name: [u8; SUBVOL_NAME_MAX + 1],
		pub devid: u64,
		pub subvolid: u64,
	}

	/// A parameter structure used by many btrfs ioctls.
	#[repr(C)]
	pub struct ArgsV2 {
		pub fd: i64,
		pub transid: u64,
		pub flags: u64,
		pub unused: [u64; 4],
		pub identifier: ArgsV2Identifier,
	}

	/// A parameter structure used by the subvolume-get-info ioctl.
	#[repr(C)]
	pub struct GetSubvolInfoArgs {
		pub treeid: u64,
		pub name: [u8; VOL_NAME_MAX + 1],
		pub parent_id: u64,
		pub dirid: u64,
		pub generation: u64,
		pub flags: u64,
		pub uuid: [u8; UUID_SIZE],
		pub parent_uuid: [u8; UUID_SIZE],
		pub received_uuid: [u8; UUID_SIZE],
		pub ctransid: u64,
		pub otransid: u64,
		pub stransid: u64,
		pub rtransid: u64,
		pub ctime: Timespec,
		pub otime: Timespec,
		pub stime: Timespec,
		pub rtime: Timespec,
		pub reserved: [u64; 8],
	}

	nix::ioctl_write_ptr!(snap_create_v2, MAGIC, 23, ArgsV2);
	nix::ioctl_read!(subvol_get_flags, MAGIC, 25, u64);
	nix::ioctl_write_ptr!(subvol_set_flags, MAGIC, 26, u64);
	nix::ioctl_read!(get_subvol_info, MAGIC, 60, GetSubvolInfoArgs);
	nix::ioctl_write_ptr!(snap_destroy_v2, MAGIC, 63, ArgsV2);
}

/// An error that can occur when operating on a btrfs filesystem.
#[derive(Debug)]
pub enum Error {
	/// A specified path is on a non-btrfs filesystem.
	NotBtrfs,

	/// A specified path is not the root directory of a subvolume.
	NotSubvolumeRoot,

	/// An error was returned by a syscall.
	Syscall(std::io::Error),
}

impl Display for Error {
	fn fmt(&self, f: &mut Formatter<'_>) -> std::result::Result<(), std::fmt::Error> {
		match self {
			Self::NotBtrfs => "not a btrfs filesystem",
			Self::NotSubvolumeRoot => "not the root of a subvolume",
			Self::Syscall(_) => "syscall failed",
		}
		.fmt(f)
	}
}

impl std::error::Error for Error {
	fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
		match self {
			Self::NotBtrfs | Self::NotSubvolumeRoot => None,
			Self::Syscall(e) => Some(e),
		}
	}
}

impl From<nix::errno::Errno> for Error {
	fn from(source: nix::errno::Errno) -> Self {
		Self::from(std::io::Error::from(source))
	}
}

impl From<std::io::Error> for Error {
	fn from(source: std::io::Error) -> Self {
		Self::Syscall(source)
	}
}

/// A result type whose error type is [`Error`](Error).
pub type Result<T> = std::result::Result<T, Error>;

/// Checks whether a given file handle refers to a something on a Btrfs filesystem.
fn is_btrfs(f: impl AsFd) -> Result<bool> {
	const BTRFS_SUPER_MAGIC: libc::__fsword_t = 0x9123683e;
	let f = f.as_fd();
	let mut stat_buf = std::mem::MaybeUninit::<libc::statfs>::uninit();
	// SAFETY:
	// - f.as_raw_fd() is a valid file descriptor, as proven by f being of type BorrowedFd.
	// - stat_buf.as_mut_ptr() is a valid pointer to memory of size to hold a statfs.
	if unsafe { libc::fstatfs(f.as_raw_fd(), stat_buf.as_mut_ptr()) } < 0 {
		Err(Error::Syscall(std::io::Error::last_os_error()))
	} else {
		// SAFETY: On success, fstatfs() promises to fill the buffer.
		let stat_buf = unsafe { stat_buf.assume_init() };
		Ok(stat_buf.f_type == BTRFS_SUPER_MAGIC)
	}
}

/// Given a file handle to a file on a Btrfs filesystem, checks whether it represents the root of a
/// subvolume.
fn is_subvolume(f: &File) -> Result<bool> {
	const BTRFS_FIRST_FREE_OBJECTID: u64 = 256;
	let metadata = f.metadata()?;
	Ok(metadata.is_dir() && metadata.ino() == BTRFS_FIRST_FREE_OBJECTID)
}

/// Creates a snapshot.
pub fn create_snapshot(
	source: &File,
	dest_parent: impl AsFd,
	dest_name: impl AsRef<OsStr>,
) -> Result<()> {
	let dest_name = dest_name.as_ref();

	// Sanity check the destination name length.
	if dest_name.len() > ioctl::SUBVOL_NAME_MAX {
		todo!("Find a better error code here");
	}

	// The source must be a subvolume root on a btrfs filesystem.
	if !is_btrfs(source)? {
		return Err(Error::NotBtrfs);
	}
	if !is_subvolume(source)? {
		return Err(Error::NotSubvolumeRoot);
	}

	// Perform the ioctl.
	let mut args = ioctl::ArgsV2 {
		fd: source.as_fd().as_raw_fd().into(),
		transid: 0,
		flags: ioctl::SUBVOL_RDONLY,
		unused: [0; 4],
		identifier: ioctl::ArgsV2Identifier {
			name: [0; ioctl::SUBVOL_NAME_MAX + 1],
		},
	};
	// SAFETY: name is the active union member.
	unsafe { &mut args.identifier.name[..dest_name.len()] }.copy_from_slice(dest_name.as_bytes());
	// SAFETY: The passed-in parameter is locally constructed properly.
	unsafe { ioctl::snap_create_v2(dest_parent.as_fd().as_raw_fd(), &args as *const _) }?;

	Ok(())
}

/// Deletes a subvolume.
pub fn delete_subvolume(parent: impl AsFd, subvolume: impl AsFd) -> Result<()> {
	let parent = parent.as_fd();
	let subvolume = subvolume.as_fd();

	// Make the subvolume writeable, which is a prerequisite for a non-root user to delete it.
	let mut flags = 0_u64;
	// SAFETY: This is a read-only ioctl.
	unsafe { ioctl::subvol_get_flags(subvolume.as_raw_fd(), &mut flags as *mut _) }?;
	flags &= !ioctl::SUBVOL_RDONLY;
	// SAFETY: The flags are exactly the old flags, minus the read-only flag.
	unsafe { ioctl::subvol_set_flags(subvolume.as_raw_fd(), &flags as *const _) }?;

	// Get subvolume info.
	let mut info = MaybeUninit::<ioctl::GetSubvolInfoArgs>::uninit();
	// SAFETY: This is a read-only ioctl and points at the right parameter type.
	unsafe { ioctl::get_subvol_info(subvolume.as_raw_fd(), info.as_mut_ptr()) }?;
	// SAFETY: The ioctl promises to fill the struct on success.
	let info = unsafe { info.assume_init() };

	// Delete subvolume.
	let args = ioctl::ArgsV2 {
		fd: 0,
		transid: 0,
		flags: ioctl::SUBVOL_SPEC_BY_ID,
		unused: [0_u64; 4],
		identifier: ioctl::ArgsV2Identifier {
			subvolid: info.treeid,
		},
	};
	// SAFETY: The parameter is of the proper type and properly populated.
	unsafe { ioctl::snap_destroy_v2(parent.as_raw_fd(), &args as *const _) }?;

	Ok(())
}
