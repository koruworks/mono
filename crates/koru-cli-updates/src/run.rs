// SPDX-License-Identifier: MPL-2.0

//! Running another program with a deadline and a bounded memory of what it
//! printed.
//!
//! Two callers: the version probe, which wants the whole (short) output, and
//! the updater run, which wants each line as it arrives and the tail at the
//! end. One implementation serves both: reader threads turn stdout and stderr
//! into lines on a channel, the caller's thread polls the child against the
//! deadline, and a child that outlives it is killed rather than waited for.

use std::ffi::OsStr;
use std::io::{self, BufRead, BufReader, Read};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// How much of an updater's output is kept for the failure detail.
const TAIL_LINES: usize = 20;
/// Longer than that is a log, not a line; the card shows one line.
const LINE_CHARS: usize = 200;
const POLL: Duration = Duration::from_millis(20);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Finished {
    pub success: bool,
    pub exit_code: Option<i32>,
    pub timed_out: bool,
    /// The last [`TAIL_LINES`] lines of stdout and stderr, in arrival order.
    pub tail: Vec<String>,
}

impl Finished {
    /// The last line that said anything, for a card's second line.
    pub fn last_line(&self) -> Option<&str> {
        self.tail
            .iter()
            .rev()
            .map(|line| line.trim())
            .find(|line| !line.is_empty())
    }

    /// Every retained line, for a log or a bug report.
    pub fn detail(&self) -> String {
        self.tail.join("\n")
    }
}

/// Run `program args…` with no stdin, calling `on_line` for every line either
/// stream prints, until it exits or `timeout` passes.
pub fn run(
    program: &OsStr,
    args: &[&str],
    timeout: Duration,
    mut on_line: impl FnMut(&str),
) -> io::Result<Finished> {
    let mut command = Command::new(program);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        // A GUI process spawning a console program otherwise opens a console
        // window for it — a black box flashing over the desktop for every
        // daily check.
        use std::os::windows::process::CommandExt as _;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    let mut child = command.spawn()?;
    let (tx, rx) = mpsc::channel::<String>();
    let readers = [
        child
            .stdout
            .take()
            .map(|out| forward_lines(out, tx.clone())),
        child
            .stderr
            .take()
            .map(|err| forward_lines(err, tx.clone())),
    ];
    drop(tx);

    let deadline = Instant::now() + timeout;
    let mut tail = Vec::new();
    let mut status = None;
    let mut timed_out = false;
    loop {
        // Lines first, so a child that exited with output still pending is
        // read before its status closes the loop.
        while let Ok(line) = rx.try_recv() {
            on_line(&line);
            push_tail(&mut tail, line);
        }
        if status.is_some() {
            break;
        }
        match child.try_wait()? {
            Some(exit) => status = Some(exit),
            None if Instant::now() >= deadline => {
                timed_out = true;
                kill(&mut child);
                status = Some(child.wait()?);
            }
            None => std::thread::sleep(POLL),
        }
    }
    for reader in readers.into_iter().flatten() {
        let _ = reader.join();
    }
    while let Ok(line) = rx.try_recv() {
        on_line(&line);
        push_tail(&mut tail, line);
    }
    let status = status.expect("loop ends with a status");
    Ok(Finished {
        success: status.success() && !timed_out,
        exit_code: status.code(),
        timed_out,
        tail,
    })
}

fn forward_lines(
    stream: impl Read + Send + 'static,
    tx: mpsc::Sender<String>,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let reader = BufReader::new(stream);
        for line in reader.split(b'\n') {
            let Ok(bytes) = line else { break };
            let text = String::from_utf8_lossy(&bytes);
            let text = text.trim_end_matches('\r');
            // Progress bars redraw with carriage returns on one line; the
            // last redraw is the one that counts.
            let text = text.rsplit('\r').next().unwrap_or(text);
            let mut line: String = text.chars().take(LINE_CHARS).collect();
            if text.chars().count() > LINE_CHARS {
                line.push('…');
            }
            if tx.send(line).is_err() {
                break;
            }
        }
    })
}

fn push_tail(tail: &mut Vec<String>, line: String) {
    if tail.len() == TAIL_LINES {
        tail.remove(0);
    }
    tail.push(line);
}

fn kill(child: &mut Child) {
    let _ = child.kill();
}
