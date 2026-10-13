//! Bounded Host setup commands. Output stays in the Host; callers publish only
//! explicitly selected fields. Cancellation always checks kernel pid identity.

use std::io::Read;
use std::os::fd::AsRawFd;
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

pub(crate) struct Output {
    pub success: bool,
    pub stdout: Vec<u8>,
}

pub(crate) fn run(
    command: &mut Command,
    timeout: Duration,
    cancelled: &AtomicBool,
    mut progress: impl FnMut(&str),
) -> Result<Output, &'static str> {
    if cancelled.load(Ordering::Acquire) {
        return Err("Cancelled");
    }
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = command
        .spawn()
        .map_err(|_| "Could not start the command on this Host.")?;
    let started = crate::session_host::process_start_time_ms(child.id());
    let mut stdout = child.stdout.take().expect("piped stdout");
    let mut stderr = child.stderr.take().expect("piped stderr");
    for fd in [stdout.as_raw_fd(), stderr.as_raw_fd()] {
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
        if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
            stop(&mut child, started);
            return Err("Could not read the command on this Host.");
        }
    }
    let deadline = Instant::now() + timeout;
    let mut out = Vec::new();
    let mut err = Vec::new();
    loop {
        let status = child.try_wait();
        for (pipe, bytes) in [
            (&mut stdout as &mut dyn Read, &mut out),
            (&mut stderr as &mut dyn Read, &mut err),
        ] {
            let mut buf = [0; 1024];
            while let Ok(count) = pipe.read(&mut buf) {
                if count == 0 {
                    break;
                }
                bytes.extend_from_slice(&buf[..count]);
                if bytes.len() > 65536 {
                    stop(&mut child, started);
                    return Err("The command produced too much output.");
                }
            }
        }
        progress(&String::from_utf8_lossy(&err));
        match status {
            Ok(Some(status)) => {
                return Ok(Output {
                    success: status.success(),
                    stdout: out,
                })
            }
            Ok(None) => {}
            Err(_) => {
                stop(&mut child, started);
                return Err("Could not check the command on this Host.");
            }
        }
        if cancelled.load(Ordering::Acquire) || Instant::now() >= deadline {
            stop(&mut child, started);
            return Err(if cancelled.load(Ordering::Acquire) {
                "Cancelled"
            } else {
                "The command timed out. Try again."
            });
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

fn stop(child: &mut Child, started: Option<u64>) {
    let pid = child.id();
    if started.is_some() && crate::session_host::process_start_time_ms(pid) == started {
        unsafe {
            libc::kill(-(pid as i32), libc::SIGKILL);
        }
        let _ = child.wait();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounded_command_reports_progress_and_stops_its_own_process_on_timeout() {
        let mut command = Command::new("/bin/sh");
        command.args(["-c", "printf 'device challenge\\n' >&2; exec sleep 30"]);
        let mut progress_seen = false;
        let started = Instant::now();
        let result = run(
            &mut command,
            Duration::from_millis(150),
            &AtomicBool::new(false),
            |text| {
                progress_seen |= text.contains("device challenge");
            },
        );
        assert!(result.is_err());
        assert!(progress_seen);
        assert!(started.elapsed() < Duration::from_secs(3));
    }

    #[test]
    fn cancelled_commands_never_start() {
        let mut command = Command::new("/does/not/exist");
        let result = run(
            &mut command,
            Duration::from_secs(1),
            &AtomicBool::new(true),
            |_| {},
        );
        assert_eq!(result.err(), Some("Cancelled"));
    }
}
