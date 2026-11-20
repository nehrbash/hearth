use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    ffi::{CStr, CString, c_char, c_int, c_void},
    io::{Read, Write},
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::{net::UnixStream, process::CommandExt},
    },
    path::PathBuf,
    process::{Child, Command, Stdio},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use zeroize::{Zeroize, Zeroizing};

pub fn nonce() -> Result<String> {
    let mut b = [0u8; 16];
    std::fs::File::open("/dev/urandom")?.read_exact(&mut b)?;
    Ok(b.iter().map(|b| format!("{b:02x}")).collect())
}
pub fn harden() -> Result<()> {
    let limit = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    ensure!(
        unsafe { libc::setrlimit(libc::RLIMIT_CORE, &limit) } == 0,
        "Cannot disable core dumps"
    );
    ensure!(
        unsafe { libc::prctl(libc::PR_SET_DUMPABLE, 0) } == 0,
        "Cannot disable process dumps"
    );
    ensure!(
        unsafe { libc::getuid() == libc::geteuid() && libc::getgid() == libc::getegid() },
        "PAM worker must not be setuid"
    );
    Ok(())
}
#[derive(Clone)]
pub struct Policy {
    pub service: String,
    pub config_dir: Option<PathBuf>,
    pub timeout: Duration,
}
impl Policy {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            !self.service.is_empty()
                && self.service.len() <= 128
                && self
                    .service
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-'),
            "Invalid PAM service"
        );
        ensure!(
            (Duration::from_millis(100)..=Duration::from_secs(120)).contains(&self.timeout),
            "Invalid PAM deadline"
        );
        if let Some(path) = &self.config_dir {
            ensure!(
                path.is_absolute() && path.as_os_str().len() <= 4096,
                "Invalid PAM config directory"
            );
        }
        Ok(())
    }
}
#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Input {
    Authenticate {
        session: String,
        request_id: String,
    },
    Reply {
        session: String,
        attempt: String,
        prompt_id: String,
        value: String,
    },
    Cancel {
        session: String,
        attempt: String,
    },
}
impl Drop for Input {
    fn drop(&mut self) {
        if let Self::Reply { value, .. } = self {
            value.zeroize();
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Authenticated,
    Denied,
    AccountDenied,
    Cancelled,
    Timeout,
    Error,
}
#[derive(Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum ChildEvent {
    Prompt {
        prompt_id: String,
        style: String,
        text: String,
        response_required: bool,
    },
    Phase {
        phase: String,
    },
    Result {
        status: Outcome,
    },
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ChildReply {
    prompt_id: String,
    value: String,
}
impl Drop for ChildReply {
    fn drop(&mut self) {
        self.value.zeroize();
    }
}
fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}
fn status(phase: &str, outcome: Option<Outcome>) -> Value {
    json!({"version":1,"service":"pam","status":"available","observed_at_ms":now_ms(),"state":{"phase":phase,"outcome":outcome},"error":null})
}

struct Attempt {
    child: Option<Child>,
    pgid: libc::pid_t,
    socket: UnixStream,
    id: String,
    request_id: String,
    deadline: Instant,
    deadline_ms: u64,
    prompt: Option<String>,
    input: Zeroizing<Vec<u8>>,
    output: Zeroizing<Vec<u8>>,
    offset: usize,
    result: Option<Outcome>,
    exit: Option<bool>,
    eof: bool,
    events: usize,
}
impl Drop for Attempt {
    fn drop(&mut self) {
        unsafe {
            libc::kill(-self.pgid, libc::SIGKILL);
        }
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}
pub struct Supervisor {
    pub session: String,
    policy: Policy,
    active: Option<Attempt>,
    pub events: VecDeque<Value>,
}
impl Supervisor {
    pub fn new(policy: Policy) -> Result<Self> {
        policy.validate()?;
        let session = nonce()?;
        let events = VecDeque::from([
            json!({"type":"ready","session":session,"service":policy.service}),
            status("idle", None),
        ]);
        Ok(Self {
            session,
            policy,
            active: None,
            events,
        })
    }
    pub fn active_fd(&self) -> Option<i32> {
        self.active.as_ref().map(|a| a.socket.as_raw_fd())
    }
    fn reject(&mut self) {
        self.events.push_back(json!({"type":"command_rejected","reason":"Invalid, stale, duplicate, or busy command"}));
    }
    pub fn command(&mut self, command: &Input) -> Result<()> {
        ensure!(self.events.len() < 64, "PAM event queue full");
        match command {
            Input::Authenticate {
                session,
                request_id,
            } => {
                if session != &self.session
                    || self.active.is_some()
                    || request_id.is_empty()
                    || request_id.len() > 128
                    || request_id.chars().any(char::is_control)
                {
                    self.reject();
                    return Ok(());
                }
                let id = nonce()?;
                let (socket, child_socket) = UnixStream::pair()?;
                socket.set_nonblocking(true)?;
                let mut cmd = Command::new(std::env::current_exe()?);
                cmd.arg("--pam-attempt")
                    .arg("--service")
                    .arg(&self.policy.service)
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .process_group(0);
                if let Some(path) = &self.policy.config_dir {
                    cmd.arg("--config-dir").arg(path);
                }
                let fd = child_socket.as_raw_fd();
                let parent = unsafe { libc::getpid() };
                unsafe {
                    cmd.pre_exec(move || {
                        if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) < 0
                            || libc::getppid() != parent
                        {
                            return Err(std::io::Error::other("PAM parent disappeared"));
                        }
                        if libc::dup2(fd, 3) < 0 || libc::fcntl(3, libc::F_SETFD, 0) < 0 {
                            return Err(std::io::Error::last_os_error());
                        }
                        Ok(())
                    });
                }
                let child = cmd.spawn().context("Cannot start PAM attempt")?;
                drop(child_socket);
                let deadline_ms = now_ms() + self.policy.timeout.as_millis() as u64;
                self.events.push_back(json!({"type":"started","session":self.session,"attempt":id,"request_id":request_id,"deadline_ms":deadline_ms}));
                self.events.push_back(status("authenticating", None));
                self.active = Some(Attempt {
                    pgid: child.id() as libc::pid_t,
                    child: Some(child),
                    socket,
                    id,
                    request_id: request_id.clone(),
                    deadline: Instant::now() + self.policy.timeout,
                    deadline_ms,
                    prompt: None,
                    input: Zeroizing::new(Vec::new()),
                    output: Zeroizing::new(Vec::new()),
                    offset: 0,
                    result: None,
                    exit: None,
                    eof: false,
                    events: 0,
                });
            }
            Input::Reply {
                session,
                attempt,
                prompt_id,
                value,
            } => {
                let Some(a) = &mut self.active else {
                    self.reject();
                    return Ok(());
                };
                if session != &self.session
                    || attempt != &a.id
                    || a.prompt.as_ref() != Some(prompt_id)
                    || value.len() > 4096
                    || value.contains('\0')
                    || Instant::now() >= a.deadline
                    || !a.output.is_empty()
                {
                    self.reject();
                    return Ok(());
                }
                a.prompt = None;
                a.output = Zeroizing::new(serde_json::to_vec(&ChildReply {
                    prompt_id: prompt_id.clone(),
                    value: value.clone(),
                })?);
                a.output.push(b'\n');
                a.offset = 0;
            }
            Input::Cancel { session, attempt } => {
                if session == &self.session
                    && self.active.as_ref().is_some_and(|a| &a.id == attempt)
                {
                    self.finish(Outcome::Cancelled);
                } else {
                    self.reject();
                }
            }
        }
        Ok(())
    }
    fn finish(&mut self, outcome: Outcome) {
        if let Some(a) = self.active.take() {
            self.events.push_back(json!({"type":"result","session":self.session,"attempt":a.id,"request_id":a.request_id,"status":outcome}));
            self.events.push_back(status("finished", Some(outcome)));
            drop(a);
        }
    }
    pub fn tick(&mut self) -> Result<()> {
        ensure!(self.events.len() < 64, "PAM event queue full");
        let Some(a) = &mut self.active else {
            return Ok(());
        };
        if Instant::now() >= a.deadline {
            self.finish(Outcome::Timeout);
            return Ok(());
        }
        if a.offset < a.output.len() {
            match a.socket.write(&a.output[a.offset..]) {
                Ok(0) => {
                    self.finish(Outcome::Error);
                    return Ok(());
                }
                Ok(n) => {
                    a.offset += n;
                    if a.offset == a.output.len() {
                        a.output.zeroize();
                        a.offset = 0;
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(_) => {
                    self.finish(Outcome::Error);
                    return Ok(());
                }
            }
        }
        let mut bytes = Zeroizing::new([0u8; 4096]);
        for _ in 0..8 {
            match a.socket.read(bytes.as_mut()) {
                Ok(0) => {
                    a.eof = true;
                    break;
                }
                Ok(n) => {
                    a.input.extend_from_slice(&bytes[..n]);
                    ensure!(a.input.len() <= 16384, "Invalid PAM child message");
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(_) => {
                    self.finish(Outcome::Error);
                    return Ok(());
                }
            }
        }
        while let Some(line) = take_line(&mut a.input)? {
            let event: ChildEvent = serde_json::from_slice(&line)
                .map_err(|_| anyhow::anyhow!("Invalid PAM child event"))?;
            a.events += 1;
            ensure!(
                a.events <= 128 && a.result.is_none(),
                "Invalid PAM conversation sequence"
            );
            match event {
                ChildEvent::Prompt {
                    prompt_id,
                    style,
                    text,
                    response_required,
                } => {
                    ensure!(
                        prompt_id.len() == 32
                            && prompt_id.bytes().all(|b| b.is_ascii_hexdigit())
                            && text.len() <= 4096
                            && a.prompt.is_none(),
                        "Invalid PAM prompt"
                    );
                    ensure!(
                        (matches!(style.as_str(), "echo_off" | "echo_on") && response_required)
                            || (matches!(style.as_str(), "info" | "error") && !response_required),
                        "Invalid PAM prompt style"
                    );
                    if response_required {
                        a.prompt = Some(prompt_id.clone());
                    }
                    self.events.push_back(json!({"type":"prompt","session":self.session,"attempt":a.id,"prompt_id":prompt_id,"style":style,"text":text,"response_required":response_required,"deadline_ms":a.deadline_ms}));
                }
                ChildEvent::Phase { phase } => {
                    ensure!(phase == "account", "Invalid PAM phase");
                    self.events.push_back(status("account", None));
                }
                ChildEvent::Result { status } => {
                    ensure!(
                        matches!(
                            status,
                            Outcome::Authenticated
                                | Outcome::Denied
                                | Outcome::AccountDenied
                                | Outcome::Error
                        ),
                        "Invalid PAM outcome"
                    );
                    a.result = Some(status);
                }
            }
        }
        if a.exit.is_none() {
            let mut info = std::mem::MaybeUninit::<libc::siginfo_t>::zeroed();
            // Keeping the leader unreaped prevents PGID reuse before group cleanup.
            let result = unsafe {
                libc::waitid(
                    libc::P_PID,
                    a.pgid as libc::id_t,
                    info.as_mut_ptr(),
                    libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
                )
            };
            if result < 0 {
                return Err(std::io::Error::last_os_error().into());
            }
            let info = unsafe { info.assume_init() };
            if unsafe { info.si_pid() } != 0 {
                a.exit = Some(info.si_code == libc::CLD_EXITED && unsafe { info.si_status() } == 0);
            }
        }
        if let Some(success) = a.exit
            && a.eof
        {
            let result = if success && a.input.is_empty() {
                a.result.unwrap_or(Outcome::Error)
            } else {
                Outcome::Error
            };
            self.finish(result);
        }
        Ok(())
    }
}
pub fn take_line(input: &mut Zeroizing<Vec<u8>>) -> Result<Option<Zeroizing<Vec<u8>>>> {
    ensure!(input.len() <= 16384, "PAM protocol line exceeds16KiB");
    let Some(end) = input.iter().position(|b| *b == b'\n') else {
        return Ok(None);
    };
    let line = Zeroizing::new(input[..end].to_vec());
    let tail = Zeroizing::new(input[end + 1..].to_vec());
    input.zeroize();
    *input = tail;
    Ok(Some(line))
}

#[repr(C)]
struct PamMessage {
    style: c_int,
    text: *const c_char,
}
#[repr(C)]
struct PamResponse {
    response: *mut c_char,
    code: c_int,
}
#[repr(C)]
struct PamConversation {
    callback: unsafe extern "C" fn(
        c_int,
        *mut *const PamMessage,
        *mut *mut PamResponse,
        *mut c_void,
    ) -> c_int,
    data: *mut c_void,
}
#[link(name = "pam")]
unsafe extern "C" {
    fn pam_start(
        service: *const c_char,
        user: *const c_char,
        conv: *const PamConversation,
        handle: *mut *mut c_void,
    ) -> c_int;
    fn pam_start_confdir(
        service: *const c_char,
        user: *const c_char,
        conv: *const PamConversation,
        dir: *const c_char,
        handle: *mut *mut c_void,
    ) -> c_int;
    fn pam_authenticate(handle: *mut c_void, flags: c_int) -> c_int;
    fn pam_acct_mgmt(handle: *mut c_void, flags: c_int) -> c_int;
    fn pam_get_item(handle: *mut c_void, item: c_int, value: *mut *const c_void) -> c_int;
    fn pam_end(handle: *mut c_void, status: c_int) -> c_int;
}
struct Conversation {
    socket: UnixStream,
}
impl Conversation {
    fn emit(&mut self, event: &ChildEvent) -> Result<()> {
        let mut data = Zeroizing::new(serde_json::to_vec(event)?);
        data.push(b'\n');
        self.socket.write_all(&data)?;
        Ok(())
    }
    fn ask(&mut self, style: c_int, text: &str) -> Result<Option<Zeroizing<String>>> {
        let (style, required) = match style {
            1 => ("echo_off", true),
            2 => ("echo_on", true),
            3 => ("error", false),
            4 => ("info", false),
            _ => anyhow::bail!("Unsupported PAM conversation"),
        };
        let prompt_id = nonce()?;
        self.emit(&ChildEvent::Prompt {
            prompt_id: prompt_id.clone(),
            style: style.into(),
            text: text.into(),
            response_required: required,
        })?;
        if !required {
            return Ok(None);
        }
        let mut line = Zeroizing::new(Vec::new());
        while line.len() <= 16384 {
            let mut byte = [0u8];
            self.socket.read_exact(&mut byte)?;
            line.push(byte[0]);
            byte.zeroize();
            if line.last() == Some(&b'\n') {
                break;
            }
        }
        ensure!(
            line.len() <= 16384 && line.last() == Some(&b'\n'),
            "Invalid PAM reply"
        );
        let mut reply: ChildReply =
            serde_json::from_slice(&line).map_err(|_| anyhow::anyhow!("Invalid PAM reply"))?;
        ensure!(
            reply.prompt_id == prompt_id
                && reply.value.len() <= 4096
                && !reply.value.contains('\0'),
            "Invalid PAM reply"
        );
        Ok(Some(Zeroizing::new(std::mem::take(&mut reply.value))))
    }
}
unsafe extern "C" fn conversation(
    count: c_int,
    messages: *mut *const PamMessage,
    responses: *mut *mut PamResponse,
    data: *mut c_void,
) -> c_int {
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<()> {
        ensure!(
            (1..=32).contains(&count)
                && !messages.is_null()
                && !responses.is_null()
                && !data.is_null(),
            "Invalid PAM conversation"
        );
        unsafe {
            *responses = std::ptr::null_mut();
        }
        let state = unsafe { &mut *(data as *mut Conversation) };
        let replies = unsafe {
            libc::calloc(count as usize, std::mem::size_of::<PamResponse>()) as *mut PamResponse
        };
        ensure!(!replies.is_null(), "PAM response allocation failed");
        let outcome = (|| -> Result<()> {
            for i in 0..count as usize {
                let message = unsafe { *messages.add(i) };
                ensure!(!message.is_null(), "Invalid PAM message");
                let message = unsafe { &*message };
                ensure!(!message.text.is_null(), "Invalid PAM message text");
                let len = unsafe { libc::strnlen(message.text, 4097) };
                ensure!(len <= 4096, "PAM prompt too long");
                let text = std::str::from_utf8(unsafe {
                    std::slice::from_raw_parts(message.text.cast::<u8>(), len)
                })?;
                if let Some(reply) = state.ask(message.style, text)? {
                    let buffer = unsafe { libc::calloc(reply.len() + 1, 1) as *mut c_char };
                    ensure!(!buffer.is_null(), "PAM reply allocation failed");
                    unsafe {
                        std::ptr::copy_nonoverlapping(reply.as_ptr(), buffer.cast(), reply.len());
                        (*replies.add(i)).response = buffer;
                    }
                }
            }
            Ok(())
        })();
        if outcome.is_err() {
            for i in 0..count as usize {
                let ptr = unsafe { (*replies.add(i)).response };
                if !ptr.is_null() {
                    let len = unsafe { libc::strlen(ptr) };
                    unsafe { std::slice::from_raw_parts_mut(ptr.cast::<u8>(), len) }.zeroize();
                    unsafe {
                        libc::free(ptr.cast());
                    }
                }
            }
            unsafe {
                libc::free(replies.cast());
            }
            outcome?;
        }
        unsafe {
            *responses = replies;
        }
        Ok(())
    }));
    if matches!(result, Ok(Ok(()))) { 0 } else { 19 }
}
fn username() -> Result<CString> {
    let uid = unsafe { libc::getuid() };
    let mut user = std::mem::MaybeUninit::<libc::passwd>::uninit();
    let mut result = std::ptr::null_mut();
    let mut buffer = Zeroizing::new(vec![0u8; 65536]);
    ensure!(
        unsafe {
            libc::getpwuid_r(
                uid,
                user.as_mut_ptr(),
                buffer.as_mut_ptr().cast(),
                buffer.len(),
                &mut result,
            )
        } == 0
            && !result.is_null(),
        "Cannot resolve current user"
    );
    let user = unsafe { user.assume_init() };
    ensure!(!user.pw_name.is_null(), "Missing user name");
    Ok(unsafe { CStr::from_ptr(user.pw_name) }.to_owned())
}
pub fn run_attempt(policy: &Policy) -> Result<()> {
    harden()?;
    policy.validate()?;
    let config = policy
        .config_dir
        .clone()
        .unwrap_or_else(|| PathBuf::from("/etc/pam.d"))
        .join(&policy.service);
    ensure!(
        std::fs::metadata(config)?.is_file(),
        "Selected PAM policy is absent"
    );
    let socket = unsafe { UnixStream::from_raw_fd(3) };
    let mut state = Conversation { socket };
    let service = CString::new(policy.service.as_str())?;
    let user = username()?;
    let conv = PamConversation {
        callback: conversation,
        data: (&mut state as *mut Conversation).cast(),
    };
    let mut handle = std::ptr::null_mut();
    let started = if let Some(dir) = &policy.config_dir {
        let dir = CString::new(dir.as_os_str().as_encoded_bytes())?;
        unsafe {
            pam_start_confdir(
                service.as_ptr(),
                user.as_ptr(),
                &conv,
                dir.as_ptr(),
                &mut handle,
            )
        }
    } else {
        unsafe { pam_start(service.as_ptr(), user.as_ptr(), &conv, &mut handle) }
    };
    let outcome = if started != 0 {
        Outcome::Error
    } else {
        let authenticated = unsafe { pam_authenticate(handle, 0) };
        let same_user = || {
            let mut value = std::ptr::null();
            unsafe {
                pam_get_item(handle, 2, &mut value) == 0
                    && !value.is_null()
                    && CStr::from_ptr(value.cast()) == user.as_c_str()
            }
        };
        let (outcome, status) = if authenticated == 0 && !same_user() {
            (Outcome::Error, 4)
        } else if authenticated != 0 {
            (Outcome::Denied, authenticated)
        } else {
            state.emit(&ChildEvent::Phase {
                phase: "account".into(),
            })?;
            let account = unsafe { pam_acct_mgmt(handle, 0) };
            if account == 0 && same_user() {
                (Outcome::Authenticated, 0)
            } else if account == 0 {
                (Outcome::Error, 4)
            } else {
                (Outcome::AccountDenied, account)
            }
        };
        let ended = unsafe { pam_end(handle, status) };
        if ended == 0 { outcome } else { Outcome::Error }
    };
    state.emit(&ChildEvent::Result { status: outcome })?;
    Ok(())
}
