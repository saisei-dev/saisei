//! `saisei run-with-pty` — run a command under a pseudo-terminal.
//! : joins its args with spaces and runs
//! `/bin/sh -c "<cmd>"` attached to a PTY (so the child sees an interactive tty),
//! pumping stdin -> pty master and pty master -> stdout, then exits with the
//! child's status (WEXITSTATUS on normal exit, 128 + signal if it was killed).

use std::ffi::CString;
use std::os::unix::io::RawFd;
use std::path::Path;
use std::process::exit;
use std::ptr;

const STDIN_FILENO: RawFd = 0;
const STDOUT_FILENO: RawFd = 1;

fn die(msg: &str) -> ! {
    eprintln!("run-with-pty: {msg}");
    exit(1)
}

/// Write all bytes to `fd`, retrying on partial writes / EINTR (like pty._writen).
unsafe fn write_all(fd: RawFd, buf: &[u8]) {
    let mut off = 0usize;
    while off < buf.len() {
        let n = libc::write(
            fd,
            buf[off..].as_ptr() as *const libc::c_void,
            buf.len() - off,
        );
        if n < 0 {
            let err = std::io::Error::last_os_error();
            if err.raw_os_error() == Some(libc::EINTR) {
                continue;
            }
            return; // matches pty.spawn: OSError aborts the copy loop
        }
        off += n as usize;
    }
}

/// Parent copy loop: pty master -> stdout, stdin -> pty master (pty._copy).
/// Returns when the master reaches EOF (child exited / closed the tty).
unsafe fn copy_loop(master_fd: RawFd) {
    let mut stdin_open = true;
    let mut buf = [0u8; 1024];
    loop {
        let mut rfds: libc::fd_set = std::mem::zeroed();
        libc::FD_ZERO(&mut rfds);
        libc::FD_SET(master_fd, &mut rfds);
        if stdin_open {
            libc::FD_SET(STDIN_FILENO, &mut rfds);
        }
        let nfds = master_fd + 1; // master_fd > STDIN_FILENO
        let r = libc::select(
            nfds,
            &mut rfds,
            ptr::null_mut(),
            ptr::null_mut(),
            ptr::null_mut(),
        );
        if r < 0 {
            if std::io::Error::last_os_error().raw_os_error() == Some(libc::EINTR) {
                continue;
            }
            return;
        }

        if libc::FD_ISSET(master_fd, &rfds) {
            let n = libc::read(master_fd, buf.as_mut_ptr() as *mut libc::c_void, buf.len());
            // Some OSes signal EOF with an empty read, some with an error; both
            // mean the child has exited and the master is unreachable.
            if n <= 0 {
                return;
            }
            write_all(STDOUT_FILENO, &buf[..n as usize]);
        }

        if stdin_open && libc::FD_ISSET(STDIN_FILENO, &rfds) {
            let n = libc::read(
                STDIN_FILENO,
                buf.as_mut_ptr() as *mut libc::c_void,
                buf.len(),
            );
            if n <= 0 {
                stdin_open = false; // stop forwarding stdin, keep draining master
            } else {
                write_all(master_fd, &buf[..n as usize]);
            }
        }
    }
}

pub fn main(_root: &Path, args: &[String]) -> ! {
    if args.is_empty() {
        eprintln!("Usage: the source CMD [ARGS...]");
        exit(1);
    }
    let cmd = args.join(" ");

    // Save the controlling terminal's mode and switch stdin to raw so keystrokes
    // pass straight through to the child (tty.setraw). Skip if stdin isn't a tty.
    let mut saved: libc::termios = unsafe { std::mem::zeroed() };
    let restore = unsafe {
        if libc::tcgetattr(STDIN_FILENO, &mut saved) == 0 {
            let mut raw = saved;
            libc::cfmakeraw(&mut raw);
            libc::tcsetattr(STDIN_FILENO, libc::TCSAFLUSH, &raw);
            true
        } else {
            false
        }
    };

    let mut master_fd: libc::c_int = -1;
    let pid = unsafe { libc::forkpty(&mut master_fd, ptr::null_mut(), ptr::null(), ptr::null()) };
    if pid < 0 {
        if restore {
            unsafe { libc::tcsetattr(STDIN_FILENO, libc::TCSAFLUSH, &saved) };
        }
        die("forkpty failed");
    }

    if pid == 0 {
        // Child: forkpty already made the slave our controlling tty and wired it
        // to stdin/stdout/stderr. Exec the shell command in its place.
        let sh = CString::new("/bin/sh").unwrap();
        let dash_c = CString::new("-c").unwrap();
        let c_cmd = CString::new(cmd).unwrap_or_else(|_| CString::new("").unwrap());
        unsafe {
            libc::execl(
                sh.as_ptr(),
                sh.as_ptr(),
                dash_c.as_ptr(),
                c_cmd.as_ptr(),
                ptr::null::<libc::c_char>(),
            );
            libc::_exit(127); // exec failed
        }
    }

    // Parent.
    unsafe { copy_loop(master_fd) };
    if restore {
        unsafe { libc::tcsetattr(STDIN_FILENO, libc::TCSAFLUSH, &saved) };
    }
    unsafe { libc::close(master_fd) };

    let mut status: libc::c_int = 0;
    loop {
        let r = unsafe { libc::waitpid(pid, &mut status, 0) };
        if r < 0 {
            if std::io::Error::last_os_error().raw_os_error() == Some(libc::EINTR) {
                continue;
            }
            die("waitpid failed");
        }
        break;
    }

    if libc::WIFEXITED(status) {
        exit(libc::WEXITSTATUS(status));
    }
    if libc::WIFSIGNALED(status) {
        exit(128 + libc::WTERMSIG(status));
    }
    exit(status);
}
