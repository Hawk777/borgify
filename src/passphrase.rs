//! Support for reading a passphrase from the terminal with echoing disabled.

use nix::libc::{self, fcntl};
use std::ffi::{c_char, c_int, CString};
use std::io::Write as _;
use std::os::unix::io::{AsFd as _, AsRawFd as _};

/// Fail if there is no tty.
const RPP_REQUIRE_TTY: c_int = 0x02;

#[link(name = ":libbsd.so.0")]
extern "C" {
	fn readpassphrase(
		prompt: *const c_char,
		buf: *mut c_char,
		bufsiz: usize,
		flags: c_int,
	) -> *mut c_char;
}

/// Reads a passphrase from the terminal.
///
/// # Panics
/// This function panics of `prompt` contains an embedded NUL.
pub fn read(prompt: &str) -> std::io::Result<String> {
	let prompt = CString::new(prompt).expect("prompt contains embedded NUL");
	let mut buffer = vec![0_u8; 1024];
	// SAFETY: Prompt is a valid CString. Buffer and its length are passed properly.
	let ret = unsafe {
		readpassphrase(
			prompt.as_ptr(),
			buffer.as_mut_ptr() as *mut c_char,
			buffer.len(),
			RPP_REQUIRE_TTY,
		)
	};
	if ret.is_null() {
		Err(std::io::Error::last_os_error())
	} else {
		// Panic-soundness: readpassphrase(), on success, promises to write a NUL into the buffer.
		let nul_pos = buffer
			.iter()
			.position(|&b| b == 0)
			.expect("readpassphrase() did not write NUL into buffer");
		buffer.resize(nul_pos, 0_u8);
		String::from_utf8(buffer)
			.map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
	}
}

/// Creates an inheritable pipe with a passphrase inside it.
pub fn send_to_inheritable_pipe(passphrase: &str) -> std::io::Result<os_pipe::PipeReader> {
	// Create the pipe.
	let (reader, mut writer) = os_pipe::pipe()?;

	// Write the passphrase into the writer end.
	writer.write_all(passphrase.as_bytes())?;

	// Make the reader end inheritable.
	let fd = reader.as_fd().as_raw_fd();
	let flags = unsafe { fcntl(fd, libc::F_GETFD) };
	if flags < 0 {
		return Err(std::io::Error::last_os_error());
	}
	let flags = flags & !libc::FD_CLOEXEC;
	let ret = unsafe { fcntl(fd, libc::F_SETFD, flags) };
	if ret < 0 {
		return Err(std::io::Error::last_os_error());
	}

	Ok(reader)
}

/// Tests sending a passphrase to a pipe.
#[test]
fn test_send_to_inheritable_pipe() {
	use std::io::Read as _;
	const PASSPHRASE: &'static str = "hello world";
	let mut reader = send_to_inheritable_pipe(PASSPHRASE).expect("send_to_inheritable_pipe failed");
	let mut buffer = vec![];
	let actual = reader.read_to_end(&mut buffer).expect("read failed");
	assert_eq!(buffer, PASSPHRASE.as_bytes());
}
