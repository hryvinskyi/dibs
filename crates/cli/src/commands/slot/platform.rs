//! What stops a command run under a slot from outside, on each system, and how its end is named.
//!
//! On Unix the wrapper catches `SIGTERM` and `SIGHUP` and sends the same signal to the command. It
//! catches `SIGINT` too, so that it outlives the command, but sends nothing: a terminal gives
//! Ctrl-C to its whole foreground process group, the command included, and a second `SIGINT`
//! makes some programs quit at once instead of finishing cleanly. On Windows it catches Ctrl-C,
//! Ctrl-Break and the closing of its console; the console hands the same event to the command,
//! which shares it, so there is nothing to send.

use std::process::ExitStatus;

/// An interruption the wrapper caught.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Interrupt {
    name: &'static str,
    passed_on: bool,
    #[cfg(unix)]
    signal: nix::sys::signal::Signal,
}

impl Interrupt {
    /// Its name as a release records it: `SIGTERM`, `SIGHUP`, `ctrl-c`, `ctrl-break`,
    /// `ctrl-close`.
    pub fn name(self) -> &'static str {
        self.name
    }

    /// Whether the command was given this interruption on the wrapper's behalf, so that the
    /// release records it as the command's end. An interruption the command already got on its
    /// own is not: the command's own outcome is.
    pub fn passed_on(self) -> bool {
        self.passed_on
    }
}

#[cfg(unix)]
mod system {
    use std::io;
    use std::os::unix::process::ExitStatusExt;
    use std::process::ExitStatus;

    use nix::sys::signal::{Signal, kill};
    use nix::unistd::Pid;
    use tokio::signal::unix::{SignalKind, signal};

    use super::Interrupt;

    /// The interruptions the wrapper listens for.
    pub struct Interrupts {
        interrupt: tokio::signal::unix::Signal,
        terminate: tokio::signal::unix::Signal,
        hangup: tokio::signal::unix::Signal,
    }

    impl Interrupts {
        /// Starts listening.
        pub fn listen() -> io::Result<Self> {
            Ok(Self {
                interrupt: signal(SignalKind::interrupt())?,
                terminate: signal(SignalKind::terminate())?,
                hangup: signal(SignalKind::hangup())?,
            })
        }

        /// The next interruption. `SIGINT` is not passed on: the terminal gave it to the command
        /// already.
        pub async fn next(&mut self) -> Interrupt {
            let (signal, passed_on) = tokio::select! {
                _ = self.interrupt.recv() => (Signal::SIGINT, false),
                _ = self.terminate.recv() => (Signal::SIGTERM, true),
                _ = self.hangup.recv() => (Signal::SIGHUP, true),
            };
            Interrupt {
                name: signal.as_str(),
                passed_on,
                signal,
            }
        }
    }

    /// Sends the interruption to the process `pid`, when it is one to pass on.
    pub fn forward(interrupt: Interrupt, pid: Option<u32>) {
        if !interrupt.passed_on {
            return;
        }
        let Some(pid) = pid.and_then(|pid| i32::try_from(pid).ok()) else {
            return;
        };
        if let Err(error) = kill(Pid::from_raw(pid), interrupt.signal) {
            tracing::debug!(error = %error, "the signal could not be passed to the command");
        }
    }

    /// The signal that ended a process, by name.
    pub fn ended_by(status: &ExitStatus) -> Option<&'static str> {
        status
            .signal()
            .and_then(|number| Signal::try_from(number).ok())
            .map(Signal::as_str)
    }

    /// The status a shell gives a process ended by the signal named.
    pub fn status_for_name(name: &str) -> u8 {
        name.parse::<Signal>()
            .ok()
            .and_then(|signal| u8::try_from(128 + signal as i32).ok())
            .unwrap_or(u8::MAX)
    }
}

#[cfg(windows)]
mod system {
    use std::io;
    use std::process::ExitStatus;

    use tokio::signal::windows::{ctrl_break, ctrl_c, ctrl_close};

    use super::Interrupt;

    /// The interruptions the wrapper listens for.
    pub struct Interrupts {
        ctrl_c: tokio::signal::windows::CtrlC,
        ctrl_break: tokio::signal::windows::CtrlBreak,
        ctrl_close: tokio::signal::windows::CtrlClose,
    }

    impl Interrupts {
        /// Starts listening.
        pub fn listen() -> io::Result<Self> {
            Ok(Self {
                ctrl_c: ctrl_c()?,
                ctrl_break: ctrl_break()?,
                ctrl_close: ctrl_close()?,
            })
        }

        /// The next interruption.
        pub async fn next(&mut self) -> Interrupt {
            let name = tokio::select! {
                _ = self.ctrl_c.recv() => "ctrl-c",
                _ = self.ctrl_break.recv() => "ctrl-break",
                _ = self.ctrl_close.recv() => "ctrl-close",
            };
            Interrupt {
                name,
                passed_on: true,
            }
        }
    }

    /// The console already gave the command the same event.
    pub fn forward(_interrupt: Interrupt, _pid: Option<u32>) {}

    /// Windows ends no process by a signal.
    pub fn ended_by(_status: &ExitStatus) -> Option<&'static str> {
        None
    }

    /// The status of a process ended by the event named.
    pub fn status_for_name(_name: &str) -> u8 {
        u8::MAX
    }
}

pub use system::{Interrupts, forward};

/// How a finished command ended: the signal that ended it, when one did.
pub fn ended_by(status: &ExitStatus) -> Option<&'static str> {
    system::ended_by(status)
}

/// The status the wrapper exits with for a command ended by the signal named.
pub fn status_for_name(name: &str) -> u8 {
    system::status_for_name(name)
}
