//! Read-only request handler behind the local IPC transports.
//!
//! The worker is transport-free: it maps one request body to one serialized
//! response body, bounded by `Limits`. It exposes only `status`, `processes`
//! and `scan`; every other operation is `unsupported_operation`. It never
//! spawns a shell and always calls the shared core functions.

use crate::ScanOptions;
use crate::ipc::{
    self, ErrorCode, Event, Handler, IpcError, Limits, Outcome, PROTOCOL_VERSION, Phase, Request,
    Response,
};
use serde_json::{Value, json};
use std::collections::{HashSet, VecDeque};
use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Remembered request ids (oldest evicted first).
const SEEN_IDS: usize = 4096;
/// Maximum process rows ever returned.
const MAX_PROCESS_ROWS: usize = 5000;
const MAX_SCAN_ROOTS: usize = 64;
const MAX_SCAN_DEPTH: usize = 128;
const MAX_SCAN_ENTRIES: usize = 1_000_000;
const SCHEMA_VERSION: u32 = 1;

/// Internal, read-only entry point: `cockpit worker exec-op` reads ONE
/// serialized `Request` body from stdin (bounded by `--max-request-bytes`,
/// default `ipc::MAX_REQUEST_BYTES`), executes it with a leaf in-process
/// `Worker`, and writes ONE serialized `Response` body to stdout (bounded
/// by `--max-response-bytes`, default `ipc::MAX_RESPONSE_BYTES`).
/// Exit 0 whenever a response was written, 2 on internal I/O failure.
/// The leaf never spawns children, so execution cannot recurse.
pub const EXEC_OP_SUBCOMMAND: &str = "exec-op";

/// How a worker runs supported operations.
enum Execution {
    /// In-process. Used by `worker exec-op` children and by tests.
    Leaf,
    /// In a `worker exec-op` child of the given Cockpit executable.
    /// Deadline expiry triggers kill plus a bounded reap attempt; inability
    /// to confirm termination disables further operations on this worker.
    Subprocess { exe: PathBuf },
}

pub struct Worker {
    limits: Limits,
    sink: Option<Box<dyn FnMut(&Event) + Send>>,
    seen: HashSet<String>,
    order: VecDeque<String>,
    execution: Execution,
    execution_poisoned: bool,
}

impl Worker {
    /// Leaf worker: operations run in-process. This is what `exec-op`
    /// children and tests use; it offers no forced cancellation.
    pub fn new(limits: Limits) -> Self {
        Self {
            limits,
            sink: None,
            seen: HashSet::new(),
            order: VecDeque::new(),
            execution: Execution::Leaf,
            execution_poisoned: false,
        }
    }

    /// Leaf worker with lifecycle events.
    pub fn with_events(limits: Limits, sink: Box<dyn FnMut(&Event) + Send>) -> Self {
        let mut worker = Self::new(limits);
        worker.sink = Some(sink);
        worker
    }

    /// Bounded worker: every supported operation runs in a fresh
    /// `worker exec-op` child of `exe` (production passes
    /// `std::env::current_exe()`; tests pass `CARGO_BIN_EXE_cockpit`).
    /// Deadline expiry or oversize output triggers termination. Reap is
    /// bounded separately; unconfirmed termination is a typed failure &
    /// prevents this worker from launching further operations.
    pub fn bounded(limits: Limits, sink: Box<dyn FnMut(&Event) + Send>, exe: PathBuf) -> Self {
        let mut worker = Self::new(limits);
        worker.sink = Some(sink);
        worker.execution = Execution::Subprocess { exe };
        worker
    }

    /// Limits in force, including the transport wait and idle-exit bounds.
    pub fn limits(&self) -> Limits {
        self.limits
    }

    fn emit(&mut self, id: Option<&str>, op: Option<&str>, phase: Phase, error: Option<ErrorCode>) {
        if let Some(sink) = self.sink.as_mut() {
            sink(&Event {
                id: id.map(str::to_owned),
                op: op.map(str::to_owned),
                phase,
                error,
            });
        }
    }

    /// Records `id`; returns false when it was already seen.
    fn remember(&mut self, id: &str) -> bool {
        if self.seen.contains(id) {
            return false;
        }
        if self.order.len() >= SEEN_IDS
            && let Some(old) = self.order.pop_front()
        {
            self.seen.remove(&old);
        }
        self.seen.insert(id.to_owned());
        self.order.push_back(id.to_owned());
        true
    }

    fn parse(&self, bytes: &[u8]) -> Result<Request, (Option<String>, IpcError)> {
        if bytes.len() > self.limits.max_request_bytes {
            return Err((
                None,
                IpcError::new(
                    ErrorCode::OversizedFrame,
                    format!(
                        "request of {} bytes exceeds {}",
                        bytes.len(),
                        self.limits.max_request_bytes
                    ),
                ),
            ));
        }
        let malformed = |id: Option<String>, message: String| {
            (id, IpcError::new(ErrorCode::MalformedRequest, message))
        };
        let value: Value = serde_json::from_slice(bytes)
            .map_err(|e| malformed(None, format!("invalid JSON: {e}")))?;
        let readable_id = value
            .get("id")
            .and_then(Value::as_str)
            .filter(|id| ipc::valid_request_id(id))
            .map(str::to_owned);
        let request: Request = serde_json::from_value(value)
            .map_err(|e| malformed(readable_id.clone(), format!("invalid request: {e}")))?;
        if !ipc::valid_request_id(&request.id) {
            return Err(malformed(None, "invalid request id".into()));
        }
        Ok(request)
    }

    fn run(&self, request: &Request) -> Result<(Value, bool), IpcError> {
        match request.op.as_str() {
            "status" => {
                match &request.args {
                    Value::Null => {}
                    Value::Object(map) if map.is_empty() => {}
                    _ => {
                        return Err(invalid("status takes no arguments"));
                    }
                }
                let status = serde_json::to_value(crate::system_status())
                    .map_err(|e| internal(format!("serialize status: {e}")))?;
                let data = json!({"schema_version": SCHEMA_VERSION, "system": status});
                self.fit_plain(&request.id, data)
            }
            "processes" => self.run_processes(request),
            "scan" => self.run_scan(request),
            other => Err(IpcError::new(
                ErrorCode::UnsupportedOperation,
                format!("operation {other:?} is not supported; worker is read-only"),
            )),
        }
    }

    fn fits(&self, id: &str, data: &Value, truncated: bool) -> Option<Vec<u8>> {
        let bytes = serde_json::to_vec(&Response {
            version: PROTOCOL_VERSION,
            id: Some(id.to_owned()),
            outcome: Outcome::Ok {
                data: data.clone(),
                truncated,
            },
        })
        .ok()?;
        (bytes.len() <= self.limits.max_response_bytes).then_some(bytes)
    }

    /// Non-list payloads cannot be truncated: fit or fail.
    fn fit_plain(&self, id: &str, data: Value) -> Result<(Value, bool), IpcError> {
        if self.fits(id, &data, false).is_some() {
            Ok((data, false))
        } else {
            Err(internal("response exceeds max_response_bytes".into()))
        }
    }

    fn run_processes(&self, request: &Request) -> Result<(Value, bool), IpcError> {
        let args: ipc::ProcessArgs = match &request.args {
            Value::Null => ipc::ProcessArgs::default(),
            other => serde_json::from_value(other.clone())
                .map_err(|e| invalid(format!("processes arguments: {e}")))?,
        };
        let mut rows = crate::procs();
        let total = rows.len();
        rows.truncate(MAX_PROCESS_ROWS);
        loop {
            let omitted = total - rows.len();
            let mut data = json!({
                "schema_version": SCHEMA_VERSION,
                "processes": serde_json::to_value(&rows)
                    .map_err(|e| internal(format!("serialize processes: {e}")))?,
                "omitted": omitted,
            });
            if args.grouped {
                // Grouped from the retained rows so groups never describe
                // rows the response does not carry.
                data["groups"] = serde_json::to_value(crate::processes::group(&rows))
                    .map_err(|e| internal(format!("serialize groups: {e}")))?;
            }
            let truncated = omitted > 0;
            if self.fits(&request.id, &data, truncated).is_some() {
                return Ok((data, truncated));
            }
            if rows.is_empty() {
                return Err(internal("response exceeds max_response_bytes".into()));
            }
            rows.truncate(rows.len() / 2);
        }
    }

    fn run_scan(&self, request: &Request) -> Result<(Value, bool), IpcError> {
        let args: ipc::ScanArgs = serde_json::from_value(request.args.clone())
            .map_err(|e| invalid(format!("scan arguments: {e}")))?;
        if args.roots.is_empty() || args.roots.len() > MAX_SCAN_ROOTS {
            return Err(invalid(format!(
                "roots must contain 1..={MAX_SCAN_ROOTS} paths"
            )));
        }
        if let Some(bad) = args.roots.iter().find(|r| !r.is_absolute()) {
            return Err(invalid(format!("root {} is not absolute", bad.display())));
        }
        if args.max_depth > MAX_SCAN_DEPTH {
            return Err(invalid(format!("max_depth exceeds {MAX_SCAN_DEPTH}")));
        }
        if args.max_entries == 0 || args.max_entries > MAX_SCAN_ENTRIES {
            return Err(invalid(format!(
                "max_entries must be 1..={MAX_SCAN_ENTRIES}"
            )));
        }
        let mut report = crate::scan_paths(
            &args.roots,
            &ScanOptions {
                max_depth: args.max_depth,
                max_entries: args.max_entries,
                ..Default::default()
            },
        );
        let total = report.entries.len();
        loop {
            let omitted = total - report.entries.len();
            let truncated = omitted > 0;
            if truncated {
                // Output truncation is NOT scan incompleteness: the scan
                // may itself be incomplete. Report the omission at the
                // response level (`truncated`, `entries_omitted`, this
                // reason) while `accounting.incomplete` keeps describing
                // the scan's own accounting only.
                let note = format!(
                    "output truncated to fit max_response_bytes: {omitted} of {total} scanned entries omitted"
                );
                report
                    .incomplete_reasons
                    .retain(|r| !r.starts_with("output truncated"));
                report.incomplete_reasons.push(note);
            }
            let data = json!({
                "report": serde_json::to_value(&report)
                    .map_err(|e| internal(format!("serialize report: {e}")))?,
                "entries_omitted": omitted,
            });
            if self.fits(&request.id, &data, truncated).is_some() {
                return Ok((data, truncated));
            }
            if report.entries.is_empty() {
                return Err(internal("response exceeds max_response_bytes".into()));
            }
            report.entries.truncate(report.entries.len() / 2);
        }
    }

    /// Serialize an error within `max_response_bytes`. Falls back to a
    /// code-only message; when even the minimal response cannot fit
    /// (non-viable limits) returns EMPTY — transports must close rather
    /// than write an oversized frame.
    fn error_bytes(&self, id: Option<String>, error: IpcError) -> Vec<u8> {
        let bytes = ipc::error_response(id.clone(), error.clone());
        if bytes.len() <= self.limits.max_response_bytes {
            return bytes;
        }
        let minimal = ipc::error_response(id, IpcError::new(error.code, ""));
        if minimal.len() <= self.limits.max_response_bytes {
            minimal
        } else {
            Vec::new()
        }
    }

    /// Run one parsed, id-remembered, version-checked request and return
    /// its serialized response. In `Subprocess` mode the operation runs in
    /// a killable `worker exec-op` child; in `Leaf` mode it runs here.
    fn execute(&mut self, request: &Request, body: &[u8]) -> Result<Vec<u8>, IpcError> {
        match &self.execution {
            Execution::Leaf => match self.run(request) {
                Ok((data, truncated)) => self
                    .fits(&request.id, &data, truncated)
                    .ok_or_else(|| internal("response exceeds max_response_bytes".into())),
                Err(error) => Err(error),
            },
            Execution::Subprocess { exe } => self.execute_in_child(exe, body, &request.id),
        }
    }

    /// Spawn `exe worker exec-op`, then drive stdin/stdout until
    /// `limits.op_deadline`. Termination confirmation may add REAP_BOUND
    /// & pipe cleanup may add DRAIN_BOUND; neither wait is unbounded. No shell, no arbitrary executable, no
    /// recursion: the child is a leaf `Worker`. Every path either reaps
    /// the child or reports that kill/reap could not be confirmed (the
    /// process may still be running; this is never claimed otherwise).
    fn execute_in_child(
        &self,
        exe: &PathBuf,
        body: &[u8],
        request_id: &str,
    ) -> Result<Vec<u8>, IpcError> {
        let mut command = Command::new(exe);
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x08000000); // CREATE_NO_WINDOW
        }
        let mut child = command
            .args(["worker", EXEC_OP_SUBCOMMAND])
            .arg("--max-request-bytes")
            .arg(self.limits.max_request_bytes.to_string())
            .arg("--max-response-bytes")
            .arg(self.limits.max_response_bytes.to_string())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            // Child lifecycle is reported by the parent's events; stderr
            // is discarded so a verbose child can never deadlock a pipe.
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| internal(format!("exec-op spawn failed: {e}")))?;

        let deadline = Instant::now() + self.limits.op_deadline;
        let cap = self.limits.max_response_bytes;
        let bytes = drive_child(&mut child, body, cap, deadline)?;

        // The child is the same Cockpit binary, but a corrupt or
        // mismatched reply must not be forwarded: verify version and id
        // echo before trusting the body.
        let valid = serde_json::from_slice::<Response>(&bytes)
            .ok()
            .is_some_and(|r| r.version == PROTOCOL_VERSION && r.id.as_deref() == Some(request_id));
        if valid {
            Ok(bytes)
        } else {
            Err(internal(
                "exec-op child returned a malformed or mismatched response".into(),
            ))
        }
    }
}

/// Polling tick for the child-driving loops.
const CHILD_IO_TICK: Duration = Duration::from_millis(20);
/// After a kill, bound the reap attempt; an unreaped child is reported as
/// unconfirmed rather than waited on forever.
const REAP_BOUND: Duration = Duration::from_secs(5);
/// After the child exits, bound the drain of any remaining stdout bytes.
const DRAIN_BOUND: Duration = Duration::from_secs(5);

/// Whether a killed child is provably reaped.
enum Reap {
    /// Exit status observed; the child cannot hold resources.
    Reaped,
    /// kill() failed, or the child did not report exit within REAP_BOUND.
    /// The process may still be running — callers surface this, never a
    /// false cancellation claim.
    Unconfirmed(String),
}

impl Reap {
    fn note(&self) -> String {
        match self {
            Reap::Reaped => "child killed and reaped".into(),
            Reap::Unconfirmed(why) => format!("child termination unconfirmed: {why}"),
        }
    }
}

/// Kill `child` (recording a kill failure), then poll `try_wait` until the
/// child is reaped or REAP_BOUND expires. Never calls blocking `wait()`.
fn kill_and_reap(child: &mut std::process::Child) -> Reap {
    let kill_error = child.kill().err().map(|e| e.to_string());
    let bound = Instant::now() + REAP_BOUND;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return Reap::Reaped,
            Ok(None) if Instant::now() >= bound => {
                return Reap::Unconfirmed(match kill_error {
                    Some(e) => format!("kill failed ({e}) and exit unconfirmed"),
                    None => "kill issued but exit unconfirmed".into(),
                });
            }
            Ok(None) => std::thread::sleep(CHILD_IO_TICK),
            Err(e) => {
                return Reap::Unconfirmed(format!("status check failed after kill: {e}"));
            }
        }
    }
}

/// Unix: child stdio fds are set O_NONBLOCK and driven by `poll` inside the
/// deadline, so no helper threads exist at all — a child that never reads
/// stdin or floods stdout cannot block the parent on a full pipe.
#[cfg(unix)]
fn drive_child(
    child: &mut std::process::Child,
    body: &[u8],
    cap: usize,
    deadline: Instant,
) -> Result<Vec<u8>, IpcError> {
    use std::os::unix::io::AsRawFd;
    let mut stdin = child.stdin.take();
    let Some(stdout) = child.stdout.take() else {
        let reap = kill_and_reap(child);
        return Err(internal(format!(
            "exec-op stdout unavailable; {}",
            reap.note()
        )));
    };
    let nonblocking = |fd: libc::c_int| -> Result<(), IpcError> {
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
        if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
            return Err(IpcError::from_io(&std::io::Error::last_os_error()));
        }
        Ok(())
    };
    if let Some(pipe) = &stdin {
        if let Err(e) = nonblocking(pipe.as_raw_fd()) {
            let reap = kill_and_reap(child);
            return Err(internal(format!(
                "exec-op stdin setup failed: {e}; {}",
                reap.note()
            )));
        }
    }
    if let Err(e) = nonblocking(stdout.as_raw_fd()) {
        let reap = kill_and_reap(child);
        return Err(internal(format!(
            "exec-op stdout setup failed: {e}; {}",
            reap.note()
        )));
    }
    let out_fd = stdout.as_raw_fd();
    let mut written = 0usize;
    // Nothing (or nothing possible) left to write: close stdin so the
    // child sees EOF on an empty request body too.
    let mut input_closed = true;
    if stdin.is_some() && written < body.len() {
        input_closed = false;
    }
    if input_closed {
        // Nothing to send: close stdin immediately so the child sees EOF.
        drop(stdin.take());
    }
    let mut output = Vec::new();
    let mut overflow = false;
    let mut eof = false;
    let mut exited: Option<std::process::ExitStatus> = None;
    // Hard stop for the post-exit drain of buffered stdout bytes.
    let mut drain_deadline: Option<Instant> = None;
    loop {
        if exited.is_none() {
            match child.try_wait() {
                Ok(Some(status)) => {
                    exited = Some(status);
                    drain_deadline = Some(Instant::now() + DRAIN_BOUND);
                }
                Ok(None) => {}
                Err(e) => {
                    // Never leave a live child on a status failure.
                    let reap = kill_and_reap(child);
                    return Err(internal(format!(
                        "exec-op status check failed: {e}; {}",
                        reap.note()
                    )));
                }
            }
        }
        if eof && exited.is_some() {
            break;
        }
        if Instant::now() >= deadline {
            let reap = kill_and_reap(child);
            return Err(match reap {
                Reap::Reaped => IpcError::new(
                    ErrorCode::Timeout,
                    "operation exceeded exec-op deadline; child killed and reaped",
                ),
                Reap::Unconfirmed(_) => internal(format!(
                    "operation exceeded exec-op deadline; {}",
                    reap.note()
                )),
            });
        }
        if overflow {
            // Response already exceeds the cap: stop the child now rather
            // than letting it fill pipes until the deadline.
            let reap = kill_and_reap(child);
            return Err(internal(format!(
                "exec-op response exceeded max_response_bytes; {}",
                reap.note()
            )));
        }
        if let Some(drain) = drain_deadline
            && Instant::now() >= drain
        {
            return Err(internal(
                "exec-op exited but stdout did not reach EOF within the drain bound".into(),
            ));
        }
        if !input_closed && written >= body.len() {
            // Fully written: close stdin so the child sees EOF.
            drop(stdin.take());
            input_closed = true;
        }
        let mut fds = Vec::with_capacity(2);
        if !input_closed && let Some(pipe) = &stdin {
            fds.push(libc::pollfd {
                fd: pipe.as_raw_fd(),
                events: libc::POLLOUT,
                revents: 0,
            });
        }
        if !eof {
            fds.push(libc::pollfd {
                fd: out_fd,
                events: libc::POLLIN,
                revents: 0,
            });
        }
        if fds.is_empty() {
            // Nothing to do but wait for exit.
            if exited.is_some() {
                break;
            }
            std::thread::sleep(CHILD_IO_TICK);
            continue;
        }
        let wait = match drain_deadline {
            Some(d) => deadline.min(d),
            None => deadline,
        };
        let left = wait
            .checked_duration_since(Instant::now())
            .unwrap_or(Duration::ZERO)
            .min(CHILD_IO_TICK);
        let ms = left.as_millis().clamp(0, libc::c_int::MAX as u128) as libc::c_int;
        let rc = unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, ms) };
        if rc < 0 {
            let e = std::io::Error::last_os_error();
            if e.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            let reap = kill_and_reap(child);
            return Err(internal(format!(
                "exec-op poll failed: {e}; {}",
                reap.note()
            )));
        }
        for fd in &fds {
            if fd.revents == 0 {
                continue;
            }
            if fd.fd == out_fd {
                let mut chunk = [0u8; 8192];
                loop {
                    let n = unsafe {
                        libc::read(
                            out_fd,
                            chunk.as_mut_ptr().cast::<libc::c_void>(),
                            chunk.len(),
                        )
                    };
                    if n > 0 {
                        let n = n as usize;
                        if output.len() + n > cap {
                            overflow = true;
                            break;
                        }
                        output.extend_from_slice(&chunk[..n]);
                    } else if n == 0 {
                        eof = true;
                        break;
                    } else {
                        let e = std::io::Error::last_os_error();
                        match e.raw_os_error() {
                            Some(code) if code == libc::EAGAIN || code == libc::EWOULDBLOCK => {
                                break;
                            }
                            Some(code) if code == libc::EINTR => continue,
                            _ => {
                                eof = true;
                                break;
                            }
                        }
                    }
                }
            } else if fd.revents & (libc::POLLERR | libc::POLLHUP | libc::POLLNVAL) != 0 {
                // stdin errored/hung up: stop writing, the child will exit.
                drop(stdin.take());
                input_closed = true;
            } else if fd.revents & libc::POLLOUT != 0 {
                let n = unsafe {
                    libc::write(
                        fd.fd,
                        body[written..].as_ptr().cast::<libc::c_void>(),
                        body.len() - written,
                    )
                };
                if n > 0 {
                    written += n as usize;
                } else {
                    let e = std::io::Error::last_os_error();
                    match e.raw_os_error() {
                        Some(code) if code == libc::EAGAIN || code == libc::EWOULDBLOCK => {}
                        Some(code) if code == libc::EINTR => {}
                        // EPIPE and anything else: child is gone.
                        _ => {
                            drop(stdin.take());
                            input_closed = true;
                        }
                    }
                }
            }
        }
    }
    let status = exited.expect("loop only exits after the child reports an exit status");
    if !status.success() {
        return Err(internal(format!("exec-op child exited {status}")));
    }
    if !eof {
        return Err(internal("exec-op exited without closing stdout".into()));
    }
    Ok(output)
}

/// Windows uses one reader & writer for the anonymous child pipes.
/// Reader failure triggers termination immediately. All thread completion
/// waits are bounded; unconfirmed termination poisons this worker.
#[cfg(not(unix))]
fn drive_child(
    child: &mut std::process::Child,
    body: &[u8],
    cap: usize,
    deadline: Instant,
) -> Result<Vec<u8>, IpcError> {
    let Some(stdout) = child.stdout.take() else {
        let reap = kill_and_reap(child);
        return Err(internal(format!(
            "exec-op stdout unavailable; {}",
            reap.note()
        )));
    };
    let writer = child.stdin.take().map(|mut pipe| {
        let owned = body.to_vec();
        std::thread::spawn(move || pipe.write_all(&owned))
    });
    let mut reader = Some(std::thread::spawn(move || {
        let mut pipe = stdout;
        let mut out = Vec::new();
        let mut chunk = [0u8; 8192];
        loop {
            match pipe.read(&mut chunk) {
                Ok(0) => break Ok(out),
                Ok(n) => {
                    if out.len() + n > cap {
                        break Err(());
                    }
                    out.extend_from_slice(&chunk[..n]);
                }
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => break Err(()),
            }
        }
    }));
    let mut output = None;
    let mut failure = None;
    let status = loop {
        if reader.as_ref().is_some_and(|task| task.is_finished()) {
            output = Some(
                reader
                    .take()
                    .expect("reader exists")
                    .join()
                    .unwrap_or(Err(())),
            );
            if matches!(output, Some(Err(()))) {
                let reap = kill_and_reap(child);
                failure = Some(internal(format!(
                    "exec-op response exceeded max_response_bytes or read failed; {}",
                    reap.note()
                )));
                break None;
            }
        }
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if Instant::now() >= deadline => {
                let reap = kill_and_reap(child);
                let code = if matches!(reap, Reap::Reaped) {
                    ErrorCode::Timeout
                } else {
                    ErrorCode::Internal
                };
                failure = Some(IpcError::new(
                    code,
                    format!("operation exceeded exec-op deadline; {}", reap.note()),
                ));
                break None;
            }
            Ok(None) => std::thread::sleep(CHILD_IO_TICK),
            Err(e) => {
                let reap = kill_and_reap(child);
                failure = Some(internal(format!(
                    "exec-op status check failed: {e}; {}",
                    reap.note()
                )));
                break None;
            }
        }
    };
    // Even if an unexpected descendant retains a pipe, do not block in join.
    let drain = Instant::now() + DRAIN_BOUND;
    while writer.as_ref().is_some_and(|task| !task.is_finished())
        || reader.as_ref().is_some_and(|task| !task.is_finished())
    {
        if Instant::now() >= drain {
            return Err(internal(format!(
                "exec-op pipe termination unconfirmed; {}",
                failure.map_or_else(|| "child exited".into(), |error| error.to_string())
            )));
        }
        std::thread::sleep(CHILD_IO_TICK);
    }
    let write_ok = writer.is_none_or(|task| matches!(task.join(), Ok(Ok(()))));
    if let Some(task) = reader {
        output = Some(task.join().unwrap_or(Err(())));
    }
    if let Some(error) = failure {
        return Err(error);
    }
    if !write_ok {
        return Err(internal("exec-op request write failed".into()));
    }
    let output = output.unwrap_or(Err(())).map_err(|()| {
        internal("exec-op response exceeded max_response_bytes or read failed".into())
    })?;
    let status = status.expect("no failure implies child exited");
    if !status.success() {
        return Err(internal(format!("exec-op child exited {status}")));
    }
    Ok(output)
}

/// Entry point for `cockpit worker exec-op`. Reads one bounded request
/// body from stdin, handles it with a leaf worker, writes one bounded
/// response body to stdout. Returns the process exit code.
pub fn exec_op_stdio(limits: Limits) -> u8 {
    let cap = limits.max_request_bytes.saturating_add(1);
    let mut input = Vec::new();
    let mut stdin = std::io::stdin();
    let mut chunk = [0u8; 8192];
    let oversized = loop {
        match stdin.read(&mut chunk) {
            Ok(0) => break false,
            Ok(n) => {
                if input.len() + n > cap {
                    break true;
                }
                input.extend_from_slice(&chunk[..n]);
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => return 2,
        }
    };
    let mut worker = Worker::new(limits);
    let out = if oversized {
        worker.error_bytes(
            None,
            IpcError::new(
                ErrorCode::OversizedFrame,
                "request exceeds max_request_bytes",
            ),
        )
    } else {
        worker.handle(&input)
    };
    let mut stdout = std::io::stdout();
    if stdout
        .write_all(&out)
        .and_then(|()| stdout.flush())
        .is_err()
    {
        return 2;
    }
    0
}

fn invalid(message: impl Into<String>) -> IpcError {
    IpcError::new(ErrorCode::InvalidArguments, message)
}

fn internal(message: String) -> IpcError {
    IpcError::new(ErrorCode::Internal, message)
}

impl ipc::Handler for Worker {
    fn handle(&mut self, request: &[u8]) -> Vec<u8> {
        if !self.limits.viable() {
            // Limits cannot carry even a minimal error response: emit the
            // failure and return empty — transports must close rather
            // than write an oversized frame.
            self.emit(None, None, Phase::Failed, Some(ErrorCode::Internal));
            return Vec::new();
        }
        let body = request;
        let request = match self.parse(request) {
            Ok(request) => request,
            Err((id, error)) => {
                self.emit(id.as_deref(), None, Phase::Failed, Some(error.code));
                return self.error_bytes(id, error);
            }
        };
        if self.execution_poisoned {
            self.emit(
                Some(&request.id),
                Some(&request.op),
                Phase::Failed,
                Some(ErrorCode::Internal),
            );
            return self.error_bytes(Some(request.id), internal("child termination previously unconfirmed; restart worker before more operations".into()));
        }
        let id = request.id.clone();
        let op = request.op.clone();
        self.emit(Some(&id), Some(&op), Phase::Started, None);

        // Remember the valid id BEFORE the version gate: a request that
        // failed on version still consumed its id, so a retry with the
        // same id is a conflict, not a silent replay.
        if !self.remember(&id) {
            let error = IpcError::new(ErrorCode::ConflictingRequestId, "request id already used");
            self.emit(Some(&id), Some(&op), Phase::Failed, Some(error.code));
            return self.error_bytes(Some(id), error);
        }

        let result = if request.version != PROTOCOL_VERSION {
            Err(IpcError::new(
                ErrorCode::UnsupportedVersion,
                format!(
                    "version {} unsupported; expected {PROTOCOL_VERSION}",
                    request.version
                ),
            ))
        } else {
            self.execute(&request, body)
        };

        match result {
            Ok(bytes) => {
                let failed = serde_json::from_slice::<Response>(&bytes)
                    .ok()
                    .and_then(|r| match r.outcome {
                        Outcome::Error { error } => Some(error.code),
                        Outcome::Ok { .. } => None,
                    });
                match failed {
                    Some(code) => self.emit(Some(&id), Some(&op), Phase::Failed, Some(code)),
                    None => self.emit(Some(&id), Some(&op), Phase::Completed, None),
                }
                bytes
            }
            Err(error) => {
                if error.message.contains("termination unconfirmed") {
                    // One uncertain process prevents further child/pipe accumulation.
                    self.execution_poisoned = true;
                }
                self.emit(Some(&id), Some(&op), Phase::Failed, Some(error.code));
                self.error_bytes(Some(id), error)
            }
        }
    }
}
