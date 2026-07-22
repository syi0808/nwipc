//! Darwin notification provider for cross-process change hints.

use std::ffi::CString;
use std::fmt;
use std::time::{Duration, Instant};

use nwipc_error::{ErrorCategory, ErrorCode, ErrorReport, Recoverability};
use nwipc_signal_api::{SignalDirection, SignalListener, SignalSender, WaitOutcome};
use nwipc_types::{Generation, SessionId};

const PREFIX: &str = "com.nwipc.signal.v1";
const MAXIMUM_NAME_LENGTH: usize = 255;

/// Redacted Darwin notification name bound to a generation.
#[derive(Clone, Eq, PartialEq)]
pub struct DarwinSignalDescriptor {
    name: String,
    generation: Generation,
}

impl DarwinSignalDescriptor {
    /// Creates a collision-resistant session/generation/direction name.
    pub fn new(session_id: SessionId, generation: Generation, direction: SignalDirection) -> Self {
        let mut session = String::with_capacity(32);
        for byte in session_id.to_bytes() {
            use fmt::Write as _;
            write!(session, "{byte:02x}").expect("writing to String cannot fail");
        }
        Self {
            name: format!(
                "{PREFIX}.{session}.{}.{}",
                generation.get(),
                direction.suffix()
            ),
            generation,
        }
    }

    /// Encodes the generation followed by the provider name.
    pub fn encode(&self) -> Vec<u8> {
        let mut output = Vec::with_capacity(8 + self.name.len());
        output.extend_from_slice(&self.generation.get().to_le_bytes());
        output.extend_from_slice(self.name.as_bytes());
        output
    }

    /// Decodes a bounded provider descriptor.
    ///
    /// # Errors
    ///
    /// Rejects malformed, unrecognized, or zero-generation descriptors.
    pub fn decode(input: &[u8]) -> Result<Self, ErrorReport> {
        let (generation, name) = input
            .split_at_checked(8)
            .ok_or_else(|| signal_error(ErrorCode::Truncated, "decode Darwin signal"))?;
        let generation_bytes = generation
            .try_into()
            .map_err(|_| signal_error(ErrorCode::Truncated, "decode Darwin signal"))?;
        let generation = Generation::new(u64::from_le_bytes(generation_bytes))
            .ok_or_else(|| signal_error(ErrorCode::StaleGeneration, "decode Darwin signal"))?;
        let name = std::str::from_utf8(name)
            .map_err(|_| signal_error(ErrorCode::ProtocolViolation, "decode Darwin signal"))?;
        if !name.starts_with(PREFIX)
            || name.len() > MAXIMUM_NAME_LENGTH
            || name.bytes().any(|byte| byte == 0)
        {
            return Err(signal_error(
                ErrorCode::ProtocolViolation,
                "decode Darwin signal",
            ));
        }
        Ok(Self {
            name: name.to_owned(),
            generation,
        })
    }

    /// Bound resource generation.
    pub const fn generation(&self) -> Generation {
        self.generation
    }
}

impl fmt::Debug for DarwinSignalDescriptor {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DarwinSignalDescriptor")
            .field("name", &"<redacted>")
            .field("generation", &self.generation)
            .finish()
    }
}

/// Darwin notification provider capability and endpoint factory.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct DarwinSignal;

/// Non-secret Darwin provider characteristics.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DarwinSignalDiagnostics {
    /// Native notifications may merge repeated posts.
    pub coalescing: bool,
    /// Correctness requires a poll path because notifications are hints.
    pub correctness_poll_required: bool,
    /// Process-boundary notification delivery is available.
    pub cross_process: bool,
}

impl DarwinSignal {
    /// Initializes Darwin notifications on macOS.
    ///
    /// # Errors
    ///
    /// Returns explicit `Unsupported` on other platforms.
    pub fn initialize() -> Result<Self, ErrorReport> {
        #[cfg(target_os = "macos")]
        {
            Ok(Self)
        }
        #[cfg(not(target_os = "macos"))]
        {
            Err(ErrorReport::unsupported("initialize Darwin signal"))
        }
    }

    /// Returns non-secret provider behavior for diagnostics.
    pub const fn diagnostics(self) -> DarwinSignalDiagnostics {
        DarwinSignalDiagnostics {
            coalescing: true,
            correctness_poll_required: true,
            cross_process: cfg!(target_os = "macos"),
        }
    }

    /// Opens a sender after validating the active generation.
    ///
    /// # Errors
    ///
    /// Rejects stale generations and invalid platform names.
    pub fn sender(
        self,
        descriptor: &DarwinSignalDescriptor,
        expected_generation: Generation,
    ) -> Result<DarwinSender, ErrorReport> {
        validate_generation(descriptor, expected_generation)?;
        let name = CString::new(descriptor.name.as_bytes())
            .map_err(|_| signal_error(ErrorCode::ProtocolViolation, "open Darwin sender"))?;
        Ok(DarwinSender { name })
    }

    /// Registers a listener and consumes any registration-time synthetic state.
    ///
    /// # Errors
    ///
    /// Rejects stale generations or a platform registration failure.
    pub fn listener(
        self,
        descriptor: &DarwinSignalDescriptor,
        expected_generation: Generation,
    ) -> Result<DarwinListener, ErrorReport> {
        validate_generation(descriptor, expected_generation)?;
        let name = CString::new(descriptor.name.as_bytes())
            .map_err(|_| signal_error(ErrorCode::ProtocolViolation, "open Darwin listener"))?;
        let token = platform::register(&name)?;
        let mut listener = DarwinListener { token: Some(token) };
        let _ = listener.try_wait()?;
        Ok(listener)
    }
}

/// Darwin notification sender.
#[derive(Debug)]
pub struct DarwinSender {
    name: CString,
}

impl SignalSender for DarwinSender {
    fn notify(&self) -> Result<(), ErrorReport> {
        platform::post(&self.name)
    }
}

/// Registered Darwin notification listener.
#[derive(Debug)]
pub struct DarwinListener {
    token: Option<platform::Token>,
}

impl SignalListener for DarwinListener {
    fn try_wait(&mut self) -> Result<WaitOutcome, ErrorReport> {
        let Some(token) = self.token else {
            return Ok(WaitOutcome::Cancelled);
        };
        platform::check(token).map(|changed| {
            if changed {
                WaitOutcome::Signaled
            } else {
                WaitOutcome::TimedOut
            }
        })
    }

    fn wait_timeout(&mut self, timeout: Duration) -> Result<WaitOutcome, ErrorReport> {
        let deadline = Instant::now().checked_add(timeout);
        loop {
            let outcome = self.try_wait()?;
            if outcome != WaitOutcome::TimedOut || timeout.is_zero() {
                return Ok(outcome);
            }
            let Some(deadline) = deadline else {
                std::thread::sleep(Duration::from_millis(1));
                continue;
            };
            let now = Instant::now();
            if now >= deadline {
                return Ok(WaitOutcome::TimedOut);
            }
            std::thread::sleep((deadline - now).min(Duration::from_millis(1)));
        }
    }

    fn cancel(&mut self) {
        if let Some(token) = self.token.take() {
            platform::cancel(token);
        }
    }
}

impl Drop for DarwinListener {
    fn drop(&mut self) {
        self.cancel();
    }
}

fn validate_generation(
    descriptor: &DarwinSignalDescriptor,
    expected: Generation,
) -> Result<(), ErrorReport> {
    if descriptor.generation != expected {
        return Err(signal_error(
            ErrorCode::StaleGeneration,
            "open Darwin signal endpoint",
        ));
    }
    Ok(())
}

fn signal_error(code: ErrorCode, operation: &'static str) -> ErrorReport {
    ErrorReport::new(
        ErrorCategory::Signal,
        code,
        if code == ErrorCode::StaleGeneration {
            Recoverability::ReplaceEndpoint
        } else {
            Recoverability::Terminal
        },
        operation,
    )
}

#[cfg(target_os = "macos")]
mod platform {
    use std::ffi::{c_char, c_int};

    use super::{CString, ErrorCode, ErrorReport, signal_error};

    const NOTIFY_STATUS_OK: u32 = 0;

    #[link(name = "System")]
    unsafe extern "C" {
        fn notify_post(name: *const c_char) -> u32;
        fn notify_register_check(name: *const c_char, token: *mut c_int) -> u32;
        fn notify_check(token: c_int, check: *mut c_int) -> u32;
        fn notify_cancel(token: c_int) -> u32;
    }

    #[derive(Clone, Copy, Debug)]
    pub(super) struct Token(c_int);

    pub(super) fn register(name: &CString) -> Result<Token, ErrorReport> {
        let mut token = 0;
        let status = unsafe { notify_register_check(name.as_ptr(), &raw mut token) };
        if status != NOTIFY_STATUS_OK {
            return Err(signal_error(
                ErrorCode::RequiredCapabilityMissing,
                "register Darwin signal",
            ));
        }
        Ok(Token(token))
    }

    pub(super) fn post(name: &CString) -> Result<(), ErrorReport> {
        if unsafe { notify_post(name.as_ptr()) } == NOTIFY_STATUS_OK {
            Ok(())
        } else {
            Err(signal_error(
                ErrorCode::RequiredCapabilityMissing,
                "post Darwin signal",
            ))
        }
    }

    pub(super) fn check(token: Token) -> Result<bool, ErrorReport> {
        let mut changed = 0;
        if unsafe { notify_check(token.0, &raw mut changed) } == NOTIFY_STATUS_OK {
            Ok(changed != 0)
        } else {
            Err(signal_error(ErrorCode::Closed, "check Darwin signal"))
        }
    }

    pub(super) fn cancel(token: Token) {
        let _ = unsafe { notify_cancel(token.0) };
    }
}

#[cfg(not(target_os = "macos"))]
mod platform {
    use super::{CString, ErrorReport};

    #[derive(Clone, Copy, Debug)]
    pub(super) struct Token;

    pub(super) fn register(_: &CString) -> Result<Token, ErrorReport> {
        Err(ErrorReport::unsupported("register Darwin signal"))
    }

    pub(super) fn post(_: &CString) -> Result<(), ErrorReport> {
        Err(ErrorReport::unsupported("post Darwin signal"))
    }

    pub(super) fn check(_: Token) -> Result<bool, ErrorReport> {
        Err(ErrorReport::unsupported("check Darwin signal"))
    }

    pub(super) const fn cancel(_: Token) {}
}

#[cfg(test)]
mod tests {
    #[cfg(target_os = "macos")]
    use std::process::Command;

    use super::*;

    fn descriptor() -> DarwinSignalDescriptor {
        DarwinSignalDescriptor::new(
            SessionId::from_u128(9).unwrap(),
            Generation::new(2).unwrap(),
            SignalDirection::RendererToPeer,
        )
    }

    #[test]
    fn descriptor_round_trip_is_redacted() {
        let descriptor = descriptor();
        assert_eq!(
            DarwinSignalDescriptor::decode(&descriptor.encode()).unwrap(),
            descriptor
        );
        assert!(!format!("{descriptor:?}").contains("00000009"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn same_process_notify_coalesces_and_cancels() {
        let provider = DarwinSignal::initialize().unwrap();
        let descriptor = descriptor();
        let generation = descriptor.generation();
        let sender = provider.sender(&descriptor, generation).unwrap();
        let mut listener = provider.listener(&descriptor, generation).unwrap();
        sender.notify().unwrap();
        sender.notify().unwrap();
        assert_eq!(
            listener.wait_timeout(Duration::from_secs(1)).unwrap(),
            WaitOutcome::Signaled
        );
        listener.cancel();
        assert_eq!(listener.try_wait().unwrap(), WaitOutcome::Cancelled);
        assert_eq!(
            provider
                .sender(&descriptor, Generation::new(3).unwrap())
                .unwrap_err()
                .code(),
            ErrorCode::StaleGeneration
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn two_process_notification_delivery() {
        const ENVIRONMENT: &str = "NWIPC_DARWIN_PROCESS_DESCRIPTOR";
        if std::env::var_os(ENVIRONMENT).is_some() {
            return;
        }
        let unique = (u128::from(std::process::id()) << 64)
            | std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos();
        let descriptor = DarwinSignalDescriptor::new(
            SessionId::from_u128(unique).unwrap(),
            Generation::new(13).unwrap(),
            SignalDirection::PeerToRenderer,
        );
        let provider = DarwinSignal::initialize().unwrap();
        let sender = provider
            .sender(&descriptor, descriptor.generation())
            .unwrap();
        let ready_path = std::env::temp_dir().join(format!("nwipc-darwin-ready-{unique}"));
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "tests::darwin_process_child", "--nocapture"])
            .env(ENVIRONMENT, encode_hex(&descriptor.encode()))
            .env("NWIPC_DARWIN_PROCESS_READY", &ready_path)
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        while !ready_path.exists() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(ready_path.exists());
        sender.notify().unwrap();
        assert!(child.wait().unwrap().success());
        std::fs::remove_file(ready_path).unwrap();
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn darwin_process_child() {
        let Ok(encoded) = std::env::var("NWIPC_DARWIN_PROCESS_DESCRIPTOR") else {
            return;
        };
        let descriptor = DarwinSignalDescriptor::decode(&decode_hex(&encoded)).unwrap();
        let provider = DarwinSignal::initialize().unwrap();
        let mut listener = provider
            .listener(&descriptor, descriptor.generation())
            .unwrap();
        std::fs::write(
            std::env::var_os("NWIPC_DARWIN_PROCESS_READY").unwrap(),
            b"ready",
        )
        .unwrap();
        assert_eq!(
            listener.wait_timeout(Duration::from_secs(2)).unwrap(),
            WaitOutcome::Signaled
        );
    }

    #[cfg(target_os = "macos")]
    fn encode_hex(bytes: &[u8]) -> String {
        bytes.iter().fold(String::new(), |mut output, byte| {
            use std::fmt::Write as _;
            write!(output, "{byte:02x}").unwrap();
            output
        })
    }

    #[cfg(target_os = "macos")]
    fn decode_hex(value: &str) -> Vec<u8> {
        value
            .as_bytes()
            .chunks_exact(2)
            .map(|pair| {
                let pair = std::str::from_utf8(pair).unwrap();
                u8::from_str_radix(pair, 16).unwrap()
            })
            .collect()
    }
}
