//! Stable output contract for the real `WKWebView` process harness.

use nwipc_error::{ErrorCategory, ErrorCode, ErrorReport, Recoverability};

/// E2E-only shared `IOSurface` byte length.
pub const ECHO_REGION_LENGTH: usize = 4 * 1024;
/// Binary payload containing zero and non-UTF-8 bytes.
pub const ECHO_PAYLOAD: &[u8] = b"\0\x01\xff\x02nwipc-renderer-peer-echo";
/// Environment variable carrying the encoded `IOSurface` descriptor.
pub const ECHO_DESCRIPTOR_ENV: &str = "NWIPC_WEBKIT_E2E_IOSURFACE";
/// Per-run Darwin notification posted after renderer-side echo verification.
pub const ECHO_NOTIFICATION_ENV: &str = "NWIPC_WEBKIT_E2E_ECHO_NOTIFICATION";
/// Resource generation selected by this process smoke.
pub const ECHO_GENERATION: u64 = 1;

const ECHO_MAGIC: &[u8; 4] = b"NWE1";
const HEADER_LENGTH: usize = 12;

/// State stored in the E2E-only shared region.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum EchoState {
    /// Region has not been published by the renderer.
    Empty = 0,
    /// Renderer published the request bytes.
    RendererRequest = 1,
    /// Native peer published the identical echo bytes.
    PeerEcho = 2,
    /// Renderer verified the echo and closed the test exchange.
    RendererVerified = 3,
}

impl EchoState {
    fn from_wire(value: u32) -> Option<Self> {
        match value {
            0 => Some(Self::Empty),
            1 => Some(Self::RendererRequest),
            2 => Some(Self::PeerEcho),
            3 => Some(Self::RendererVerified),
            _ => None,
        }
    }
}

/// Decoded E2E echo frame.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EchoFrame<'bytes> {
    /// Current producer/consumer state.
    pub state: EchoState,
    /// Complete binary payload.
    pub payload: &'bytes [u8],
}

/// Encodes one complete E2E frame for a single locked `IOSurface` write.
///
/// # Errors
///
/// Rejects a payload that cannot fit the bounded smoke-test region.
pub fn encode_echo_frame(
    state: EchoState,
    payload: &[u8],
) -> Result<[u8; ECHO_REGION_LENGTH], ErrorReport> {
    if payload.len() > ECHO_REGION_LENGTH - HEADER_LENGTH {
        return Err(report_error());
    }
    let length = u32::try_from(payload.len()).map_err(|_| report_error())?;
    let mut output = [0; ECHO_REGION_LENGTH];
    output[..4].copy_from_slice(ECHO_MAGIC);
    output[4..8].copy_from_slice(&(state as u32).to_le_bytes());
    output[8..12].copy_from_slice(&length.to_le_bytes());
    output[HEADER_LENGTH..HEADER_LENGTH + payload.len()].copy_from_slice(payload);
    Ok(output)
}

/// Decodes and validates one complete E2E region snapshot.
///
/// # Errors
///
/// Rejects invalid magic, state, length, and non-zero trailing bytes.
pub fn decode_echo_frame(bytes: &[u8]) -> Result<EchoFrame<'_>, ErrorReport> {
    if bytes.len() != ECHO_REGION_LENGTH || &bytes[..4] != ECHO_MAGIC {
        return Err(report_error());
    }
    let state = EchoState::from_wire(u32::from_le_bytes(
        bytes[4..8].try_into().map_err(|_| report_error())?,
    ))
    .ok_or_else(report_error)?;
    let length = usize::try_from(u32::from_le_bytes(
        bytes[8..12].try_into().map_err(|_| report_error())?,
    ))
    .map_err(|_| report_error())?;
    let end = HEADER_LENGTH
        .checked_add(length)
        .filter(|end| *end <= bytes.len())
        .ok_or_else(report_error)?;
    if bytes[end..].iter().any(|byte| *byte != 0) {
        return Err(report_error());
    }
    Ok(EchoFrame {
        state,
        payload: &bytes[HEADER_LENGTH..end],
    })
}

/// Successful observations emitted by the native `AppKit` harness.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WebKitE2eReport {
    observations: u8,
}

impl WebKitE2eReport {
    const INITIAL_BUNDLE: u8 = 1 << 0;
    const REPLACEMENT_PROCESS: u8 = 1 << 1;
    const HARDENED_PROCESS: u8 = 1 << 2;
    const BINARY_ECHO: u8 = 1 << 3;
    const COMPLETE: u8 = Self::INITIAL_BUNDLE
        | Self::REPLACEMENT_PROCESS
        | Self::HARDENED_PROCESS
        | Self::BINARY_ECHO;

    /// Parses the bounded one-line native harness contract.
    ///
    /// # Errors
    ///
    /// Rejects missing, unknown, or unsuccessful observations.
    pub fn parse(output: &str) -> Result<Self, ErrorReport> {
        let line = output
            .lines()
            .find(|line| line.starts_with("webkit-e2e: "))
            .ok_or_else(report_error)?;
        let observations = [
            ("initial-load=ok", Self::INITIAL_BUNDLE),
            ("replacement-process=ok", Self::REPLACEMENT_PROCESS),
            ("hardened-process=ok", Self::HARDENED_PROCESS),
            ("binary-echo=ok", Self::BINARY_ECHO),
        ]
        .into_iter()
        .filter_map(|(marker, bit)| line.contains(marker).then_some(bit))
        .fold(0, |observations, bit| observations | bit);
        if observations != Self::COMPLETE {
            return Err(report_error());
        }
        Ok(Self { observations })
    }

    /// Whether the first `WebContent` process invoked `WKBundleInitialize`.
    pub const fn initial_bundle_loaded(self) -> bool {
        self.observations & Self::INITIAL_BUNDLE != 0
    }

    /// Whether a different `WebContent` process completed replacement navigation.
    pub const fn replacement_process_observed(self) -> bool {
        self.observations & Self::REPLACEMENT_PROCESS != 0
    }

    /// Whether hardened artifact inspection preceded process execution.
    pub const fn hardened_process(self) -> bool {
        self.observations & Self::HARDENED_PROCESS != 0
    }

    /// Whether renderer and native peer exchanged exact binary bytes directly.
    pub const fn binary_echo(self) -> bool {
        self.observations & Self::BINARY_ECHO != 0
    }
}

fn report_error() -> ErrorReport {
    ErrorReport::new(
        ErrorCategory::Platform,
        ErrorCode::ProtocolViolation,
        Recoverability::Terminal,
        "webkit e2e harness report",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_only_a_complete_process_report() {
        let report = WebKitE2eReport::parse(
            "webkit-e2e: initial-load=ok binary-echo=ok replacement-process=ok hardened-process=ok\n",
        )
        .unwrap();
        assert!(report.initial_bundle_loaded());
        assert!(report.binary_echo());
        assert!(WebKitE2eReport::parse("webkit-e2e: initial-load=ok").is_err());
    }

    #[test]
    fn echo_frame_preserves_binary_payload_and_rejects_trailing_data() {
        let encoded = encode_echo_frame(EchoState::RendererRequest, ECHO_PAYLOAD).unwrap();
        assert_eq!(
            decode_echo_frame(&encoded).unwrap(),
            EchoFrame {
                state: EchoState::RendererRequest,
                payload: ECHO_PAYLOAD,
            }
        );
        let mut corrupt = encoded;
        corrupt[ECHO_REGION_LENGTH - 1] = 1;
        assert!(decode_echo_frame(&corrupt).is_err());
    }
}
