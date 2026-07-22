//! Provider-independent protocol negotiation and HELLO/ACK state machines.

use nwipc_capabilities::{
    NegotiatedCapabilities, RequestedCapabilities, RequiredCapabilities, SupportedCapabilities,
    TransportCapabilities, negotiate,
};
use nwipc_error::{ErrorCategory, ErrorCode, ErrorReport, Recoverability};
use nwipc_types::{Generation, SessionId};

const MAGIC: &[u8; 4] = b"NWHP";
const HELLO_KIND: u8 = 1;
const ACK_KIND: u8 = 2;
const HELLO_FIXED: usize = 64;
const ACK_LENGTH: usize = 56;
/// Current protocol version. The high byte is the major and the low byte is the minor.
pub const CURRENT_VERSION: ProtocolVersion = ProtocolVersion::new(1, 0);
/// Largest authentication proof accepted in a HELLO.
pub const MAX_PROOF_LENGTH: usize = 64;

/// A wire-compatible major/minor protocol version.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
#[repr(transparent)]
pub struct ProtocolVersion(u16);

impl ProtocolVersion {
    /// Creates a version from a major and minor component.
    pub const fn new(major: u8, minor: u8) -> Self {
        Self(u16::from_be_bytes([major, minor]))
    }

    /// Decodes the stable wire representation. Version zero is invalid.
    pub const fn from_wire(value: u16) -> Option<Self> {
        if value == 0 { None } else { Some(Self(value)) }
    }

    /// Stable integer representation.
    pub const fn to_wire(self) -> u16 {
        self.0
    }
    /// Major compatibility component.
    pub const fn major(self) -> u8 {
        self.0.to_be_bytes()[0]
    }
    /// Minor feature component.
    pub const fn minor(self) -> u8 {
        self.0.to_be_bytes()[1]
    }
}

/// Inclusive range of versions supported by one endpoint.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct VersionRange {
    minimum: ProtocolVersion,
    maximum: ProtocolVersion,
}

impl VersionRange {
    /// Creates a range confined to one protocol major.
    ///
    /// # Errors
    ///
    /// Returns `InvalidRange` for descending bounds or mixed major versions.
    pub fn new(minimum: ProtocolVersion, maximum: ProtocolVersion) -> Result<Self, ErrorReport> {
        if minimum > maximum || minimum.major() != maximum.major() {
            return Err(protocol_error(
                ErrorCode::InvalidRange,
                "protocol version range",
            ));
        }
        Ok(Self { minimum, maximum })
    }
    /// A range containing only one version.
    pub const fn exact(version: ProtocolVersion) -> Self {
        Self {
            minimum: version,
            maximum: version,
        }
    }
    /// Lowest supported version.
    pub const fn minimum(self) -> ProtocolVersion {
        self.minimum
    }
    /// Highest supported version.
    pub const fn maximum(self) -> ProtocolVersion {
        self.maximum
    }
}

/// Selects the newest mutually supported version and rejects major mismatches.
///
/// # Errors
///
/// Returns `LayoutVersionMismatch` when the ranges have no compatible version.
pub fn negotiate_version(
    local: VersionRange,
    remote: VersionRange,
) -> Result<ProtocolVersion, ErrorReport> {
    if local.minimum.major() != remote.minimum.major() {
        return Err(protocol_error(
            ErrorCode::LayoutVersionMismatch,
            "protocol major version",
        ));
    }
    let minimum = local.minimum.max(remote.minimum);
    let maximum = local.maximum.min(remote.maximum);
    if minimum > maximum {
        return Err(protocol_error(
            ErrorCode::LayoutVersionMismatch,
            "protocol version overlap",
        ));
    }
    Ok(maximum)
}

/// Endpoint identity carried by handshake frames.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HandshakeIdentity {
    /// Session owning the resources.
    pub session_id: SessionId,
    /// Active resource generation.
    pub generation: Generation,
    /// Role sending the frame.
    pub role: EndpointRole,
}

/// Stable protocol endpoint role.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum EndpointRole {
    Coordinator = 1,
    Peer = 2,
    Renderer = 3,
}

impl EndpointRole {
    const fn from_wire(value: u8) -> Option<Self> {
        match value {
            1 => Some(Self::Coordinator),
            2 => Some(Self::Peer),
            3 => Some(Self::Renderer),
            _ => None,
        }
    }
}

/// Initiator policy used to create and validate a negotiation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InitiatorConfig {
    /// Local identity.
    pub identity: HandshakeIdentity,
    /// Expected ACK sender.
    pub remote_role: EndpointRole,
    /// Supported version interval.
    pub versions: VersionRange,
    /// Desired capabilities.
    pub requested: RequestedCapabilities,
    /// Mandatory desired capabilities.
    pub required: RequiredCapabilities,
    /// Maximum complete logical message accepted locally.
    pub maximum_message: u32,
    /// Opaque bootstrap proof, compared without interpretation by the acceptor.
    pub proof: Vec<u8>,
}

/// Acceptor policy used to validate HELLO and select an ACK.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AcceptorConfig {
    /// Local identity.
    pub identity: HandshakeIdentity,
    /// Required HELLO sender role.
    pub remote_role: EndpointRole,
    /// Supported version interval.
    pub versions: VersionRange,
    /// Capabilities provided by this endpoint.
    pub supported: SupportedCapabilities,
    /// Maximum complete logical message accepted locally.
    pub maximum_message: u32,
    /// Expected opaque bootstrap proof.
    pub proof: Vec<u8>,
}

/// Final immutable handshake selection.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NegotiatedProtocol {
    /// Selected protocol version.
    pub version: ProtocolVersion,
    /// Mutually enabled capabilities.
    pub capabilities: NegotiatedCapabilities,
    /// Smaller endpoint message limit.
    pub maximum_message: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum InitiatorState {
    Ready,
    HelloSent,
    Open,
    Failed,
}

/// Stateful initiator that rejects duplicate and out-of-order frames.
pub struct InitiatorHandshake {
    config: InitiatorConfig,
    state: InitiatorState,
}

impl InitiatorHandshake {
    /// Creates a validated handshake initiator.
    ///
    /// # Errors
    ///
    /// Returns `InvalidRange` for an empty message limit or invalid proof length.
    pub fn new(config: InitiatorConfig) -> Result<Self, ErrorReport> {
        validate_limits(config.maximum_message, &config.proof)?;
        Ok(Self {
            config,
            state: InitiatorState::Ready,
        })
    }

    /// Encodes exactly one HELLO.
    ///
    /// # Errors
    ///
    /// Returns `InvalidStateTransition` after the first call.
    pub fn hello(&mut self) -> Result<Vec<u8>, ErrorReport> {
        if self.state != InitiatorState::Ready {
            return self.fail(ErrorCode::InvalidStateTransition, "send protocol hello");
        }
        let proof_length = u16::try_from(self.config.proof.len())
            .map_err(|_| protocol_error(ErrorCode::InvalidRange, "protocol proof length"))?;
        let mut frame = vec![0; HELLO_FIXED + usize::from(proof_length)];
        frame[..4].copy_from_slice(MAGIC);
        frame[4] = HELLO_KIND;
        frame[5] = self.config.identity.role as u8;
        put_u16(&mut frame, 6, proof_length);
        put_u16(&mut frame, 8, self.config.versions.minimum.to_wire());
        put_u16(&mut frame, 10, self.config.versions.maximum.to_wire());
        frame[12..28].copy_from_slice(&self.config.identity.session_id.to_bytes());
        put_u64(&mut frame, 28, self.config.identity.generation.get());
        put_u64(&mut frame, 36, self.config.requested.capabilities().bits());
        put_u64(&mut frame, 44, self.config.required.capabilities().bits());
        put_u32(&mut frame, 52, self.config.maximum_message);
        frame[HELLO_FIXED..].copy_from_slice(&self.config.proof);
        self.state = InitiatorState::HelloSent;
        Ok(frame)
    }

    /// Validates one ACK and opens the negotiation.
    ///
    /// # Errors
    ///
    /// Returns a stable protocol error for malformed, stale, or invalid negotiation bytes.
    pub fn acknowledge(&mut self, frame: &[u8]) -> Result<NegotiatedProtocol, ErrorReport> {
        if self.state != InitiatorState::HelloSent {
            return self.fail(
                ErrorCode::InvalidStateTransition,
                "receive protocol acknowledgement",
            );
        }
        let result = decode_ack(frame).and_then(|ack| {
            validate_identity(
                ack.identity,
                self.config.identity.session_id,
                self.config.identity.generation,
                self.config.remote_role,
            )?;
            if ack.selection.version < self.config.versions.minimum
                || ack.selection.version > self.config.versions.maximum
                || !self
                    .config
                    .requested
                    .capabilities()
                    .contains(ack.selection.capabilities.capabilities())
                || !ack
                    .selection
                    .capabilities
                    .capabilities()
                    .contains(self.config.required.capabilities())
                || ack.selection.maximum_message == 0
                || ack.selection.maximum_message > self.config.maximum_message
            {
                return Err(protocol_error(
                    ErrorCode::ProtocolViolation,
                    "protocol acknowledgement selection",
                ));
            }
            Ok(ack.selection)
        });
        match result {
            Ok(selection) => {
                self.state = InitiatorState::Open;
                Ok(selection)
            }
            Err(error) => {
                self.state = InitiatorState::Failed;
                Err(error)
            }
        }
    }

    fn fail<T>(&mut self, code: ErrorCode, operation: &'static str) -> Result<T, ErrorReport> {
        self.state = InitiatorState::Failed;
        Err(protocol_error(code, operation))
    }
}

/// Stateless acceptor operation. Each instance accepts exactly one HELLO.
pub struct AcceptorHandshake {
    config: AcceptorConfig,
    consumed: bool,
}

impl AcceptorHandshake {
    /// Creates a validated acceptor.
    ///
    /// # Errors
    ///
    /// Returns `InvalidRange` for an empty message limit or invalid proof length.
    pub fn new(config: AcceptorConfig) -> Result<Self, ErrorReport> {
        validate_limits(config.maximum_message, &config.proof)?;
        Ok(Self {
            config,
            consumed: false,
        })
    }

    /// Validates HELLO, negotiates, and returns the ACK bytes plus selection.
    ///
    /// # Errors
    ///
    /// Returns a stable protocol error for malformed, stale, unauthenticated, or duplicate input.
    pub fn accept(&mut self, frame: &[u8]) -> Result<(Vec<u8>, NegotiatedProtocol), ErrorReport> {
        if self.consumed {
            return Err(protocol_error(
                ErrorCode::InvalidStateTransition,
                "accept protocol hello",
            ));
        }
        self.consumed = true;
        let hello = decode_hello(frame)?;
        validate_identity(
            hello.identity,
            self.config.identity.session_id,
            self.config.identity.generation,
            self.config.remote_role,
        )?;
        if hello.proof != self.config.proof {
            return Err(protocol_error(
                ErrorCode::ProtocolViolation,
                "protocol authentication proof",
            ));
        }
        let version = negotiate_version(self.config.versions, hello.versions)?;
        let capabilities = negotiate(self.config.supported, hello.requested, hello.required)?;
        let selection = NegotiatedProtocol {
            version,
            capabilities,
            maximum_message: self.config.maximum_message.min(hello.maximum_message),
        };
        let ack = encode_ack(self.config.identity, selection);
        Ok((ack, selection))
    }
}

struct Hello {
    identity: HandshakeIdentity,
    versions: VersionRange,
    requested: RequestedCapabilities,
    required: RequiredCapabilities,
    maximum_message: u32,
    proof: Vec<u8>,
}
struct Ack {
    identity: HandshakeIdentity,
    selection: NegotiatedProtocol,
}

fn decode_hello(frame: &[u8]) -> Result<Hello, ErrorReport> {
    let prefix = frame
        .get(..HELLO_FIXED)
        .ok_or_else(|| protocol_error(ErrorCode::Truncated, "decode protocol hello"))?;
    validate_prefix(prefix, HELLO_KIND)?;
    if prefix[56..64] != [0; 8] {
        return Err(protocol_error(
            ErrorCode::ProtocolViolation,
            "protocol hello reserved",
        ));
    }
    let proof_length = usize::from(get_u16(prefix, 6));
    if proof_length > MAX_PROOF_LENGTH
        || frame.len()
            != HELLO_FIXED
                .checked_add(proof_length)
                .ok_or_else(|| protocol_error(ErrorCode::InvalidRange, "protocol hello length"))?
    {
        return Err(protocol_error(
            ErrorCode::InvalidRange,
            "protocol hello length",
        ));
    }
    let identity = decode_identity(prefix)?;
    let minimum = ProtocolVersion::from_wire(get_u16(prefix, 8))
        .ok_or_else(|| protocol_error(ErrorCode::ProtocolViolation, "protocol minimum version"))?;
    let maximum = ProtocolVersion::from_wire(get_u16(prefix, 10))
        .ok_or_else(|| protocol_error(ErrorCode::ProtocolViolation, "protocol maximum version"))?;
    let maximum_message = get_u32(prefix, 52);
    validate_limits(maximum_message, &frame[HELLO_FIXED..])?;
    Ok(Hello {
        identity,
        versions: VersionRange::new(minimum, maximum)?,
        requested: RequestedCapabilities::new(TransportCapabilities::from_bits(get_u64(
            prefix, 36,
        ))),
        required: RequiredCapabilities::new(TransportCapabilities::from_bits(get_u64(prefix, 44))),
        maximum_message,
        proof: frame[HELLO_FIXED..].to_vec(),
    })
}

fn decode_ack(frame: &[u8]) -> Result<Ack, ErrorReport> {
    if frame.len() != ACK_LENGTH {
        return Err(protocol_error(
            ErrorCode::Truncated,
            "decode protocol acknowledgement",
        ));
    }
    validate_prefix(frame, ACK_KIND)?;
    if get_u16(frame, 6) != 0 || get_u16(frame, 10) != 0 || frame[48..56] != [0; 8] {
        return Err(protocol_error(
            ErrorCode::ProtocolViolation,
            "protocol acknowledgement reserved",
        ));
    }
    let version = ProtocolVersion::from_wire(get_u16(frame, 8)).ok_or_else(|| {
        protocol_error(
            ErrorCode::ProtocolViolation,
            "protocol acknowledgement version",
        )
    })?;
    let maximum_message = get_u32(frame, 44);
    if maximum_message == 0 {
        return Err(protocol_error(
            ErrorCode::InvalidRange,
            "protocol acknowledgement limit",
        ));
    }
    Ok(Ack {
        identity: decode_identity(frame)?,
        selection: NegotiatedProtocol {
            version,
            capabilities: NegotiatedCapabilities::new(TransportCapabilities::from_bits(get_u64(
                frame, 36,
            ))),
            maximum_message,
        },
    })
}

fn encode_ack(identity: HandshakeIdentity, selection: NegotiatedProtocol) -> Vec<u8> {
    let mut frame = vec![0; ACK_LENGTH];
    frame[..4].copy_from_slice(MAGIC);
    frame[4] = ACK_KIND;
    frame[5] = identity.role as u8;
    put_u16(&mut frame, 8, selection.version.to_wire());
    frame[12..28].copy_from_slice(&identity.session_id.to_bytes());
    put_u64(&mut frame, 28, identity.generation.get());
    put_u64(&mut frame, 36, selection.capabilities.capabilities().bits());
    put_u32(&mut frame, 44, selection.maximum_message);
    frame
}

fn validate_prefix(frame: &[u8], kind: u8) -> Result<(), ErrorReport> {
    if frame.get(..4) != Some(MAGIC.as_slice()) || frame.get(4) != Some(&kind) {
        return Err(protocol_error(
            ErrorCode::ProtocolViolation,
            "protocol frame prefix",
        ));
    }
    Ok(())
}
fn decode_identity(frame: &[u8]) -> Result<HandshakeIdentity, ErrorReport> {
    let role = EndpointRole::from_wire(frame[5])
        .ok_or_else(|| protocol_error(ErrorCode::ProtocolViolation, "protocol endpoint role"))?;
    let session: [u8; 16] = frame[12..28]
        .try_into()
        .map_err(|_| protocol_error(ErrorCode::Truncated, "protocol session"))?;
    let session_id = SessionId::from_bytes(session)
        .ok_or_else(|| protocol_error(ErrorCode::ProtocolViolation, "protocol session"))?;
    let generation = Generation::new(get_u64(frame, 28))
        .ok_or_else(|| protocol_error(ErrorCode::ProtocolViolation, "protocol generation"))?;
    Ok(HandshakeIdentity {
        session_id,
        generation,
        role,
    })
}
fn validate_identity(
    actual: HandshakeIdentity,
    session: SessionId,
    generation: Generation,
    role: EndpointRole,
) -> Result<(), ErrorReport> {
    if actual.session_id != session || actual.role != role {
        return Err(protocol_error(
            ErrorCode::ProtocolViolation,
            "protocol endpoint identity",
        ));
    }
    if actual.generation != generation {
        return Err(protocol_error(
            ErrorCode::StaleGeneration,
            "protocol generation",
        ));
    }
    Ok(())
}
fn validate_limits(maximum_message: u32, proof: &[u8]) -> Result<(), ErrorReport> {
    if maximum_message == 0 || proof.is_empty() || proof.len() > MAX_PROOF_LENGTH {
        return Err(protocol_error(
            ErrorCode::InvalidRange,
            "protocol handshake limits",
        ));
    }
    Ok(())
}
fn protocol_error(code: ErrorCode, operation: &'static str) -> ErrorReport {
    ErrorReport::new(
        ErrorCategory::Protocol,
        code,
        Recoverability::ReplaceEndpoint,
        operation,
    )
}
fn get_u16(bytes: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes(
        bytes[offset..offset + 2]
            .try_into()
            .expect("validated fixed frame"),
    )
}
fn get_u32(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(
        bytes[offset..offset + 4]
            .try_into()
            .expect("validated fixed frame"),
    )
}
fn get_u64(bytes: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes(
        bytes[offset..offset + 8]
            .try_into()
            .expect("validated fixed frame"),
    )
}
fn put_u16(bytes: &mut [u8], offset: usize, value: u16) {
    bytes[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
}
fn put_u32(bytes: &mut [u8], offset: usize, value: u32) {
    bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}
fn put_u64(bytes: &mut [u8], offset: usize, value: u64) {
    bytes[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;
    fn session() -> SessionId {
        SessionId::from_u128(0x0102).unwrap()
    }
    fn generation() -> Generation {
        Generation::new(7).unwrap()
    }
    fn common() -> TransportCapabilities {
        TransportCapabilities::BINARY_MESSAGES.union(TransportCapabilities::BOUNDED_BACKPRESSURE)
    }
    fn initiator() -> InitiatorHandshake {
        InitiatorHandshake::new(InitiatorConfig {
            identity: HandshakeIdentity {
                session_id: session(),
                generation: generation(),
                role: EndpointRole::Peer,
            },
            remote_role: EndpointRole::Coordinator,
            versions: VersionRange::exact(CURRENT_VERSION),
            requested: RequestedCapabilities::new(common()),
            required: RequiredCapabilities::new(TransportCapabilities::BINARY_MESSAGES),
            maximum_message: 4096,
            proof: b"proof".to_vec(),
        })
        .unwrap()
    }
    fn acceptor() -> AcceptorHandshake {
        AcceptorHandshake::new(AcceptorConfig {
            identity: HandshakeIdentity {
                session_id: session(),
                generation: generation(),
                role: EndpointRole::Coordinator,
            },
            remote_role: EndpointRole::Peer,
            versions: VersionRange::exact(CURRENT_VERSION),
            supported: SupportedCapabilities::new(common()),
            maximum_message: 2048,
            proof: b"proof".to_vec(),
        })
        .unwrap()
    }

    #[test]
    fn negotiates_golden_hello_and_ack() {
        let mut initiator = initiator();
        let hello = initiator.hello().unwrap();
        let expected = decode_hex(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../../tests/protocol-fixtures/protocol-v1-hello.hex"
        )));
        assert_eq!(hello, expected);
        let (ack, accepted) = acceptor().accept(&hello).unwrap();
        let selected = initiator.acknowledge(&ack).unwrap();
        assert_eq!(selected, accepted);
        assert_eq!(selected.maximum_message, 2048);
    }
    #[test]
    fn rejects_order_duplicates_stale_and_missing_capability() {
        assert_eq!(
            initiator().acknowledge(&[]).unwrap_err().code(),
            ErrorCode::InvalidStateTransition
        );
        let mut initiator = initiator();
        let hello = initiator.hello().unwrap();
        assert_eq!(
            initiator.hello().unwrap_err().code(),
            ErrorCode::InvalidStateTransition
        );
        let mut stale = acceptor();
        let mut hello = hello;
        hello[28] = 8;
        assert_eq!(
            stale.accept(&hello).unwrap_err().code(),
            ErrorCode::StaleGeneration
        );
        let missing = negotiate(
            SupportedCapabilities::new(TransportCapabilities::NONE),
            RequestedCapabilities::new(TransportCapabilities::BINARY_MESSAGES),
            RequiredCapabilities::new(TransportCapabilities::BINARY_MESSAGES),
        );
        assert_eq!(
            missing.unwrap_err().code(),
            ErrorCode::RequiredCapabilityMissing
        );
    }
    #[test]
    fn arbitrary_frames_do_not_panic() {
        for length in 0..140 {
            let input: Vec<u8> = (0_u8..=u8::MAX).cycle().take(length).collect();
            let _ = acceptor().accept(&input);
        }
    }
    #[test]
    fn version_negotiation_overlap_property() {
        for local_min in 0..=8 {
            for local_max in local_min..=8 {
                for remote_min in 0..=8 {
                    for remote_max in remote_min..=8 {
                        let local = VersionRange::new(
                            ProtocolVersion::new(1, local_min),
                            ProtocolVersion::new(1, local_max),
                        )
                        .unwrap();
                        let remote = VersionRange::new(
                            ProtocolVersion::new(1, remote_min),
                            ProtocolVersion::new(1, remote_max),
                        )
                        .unwrap();
                        let expected = (local_min.max(remote_min) <= local_max.min(remote_max))
                            .then(|| ProtocolVersion::new(1, local_max.min(remote_max)));
                        assert_eq!(negotiate_version(local, remote).ok(), expected);
                    }
                }
            }
        }
    }
    fn decode_hex(source: &str) -> Vec<u8> {
        source
            .split_whitespace()
            .map(|byte| u8::from_str_radix(byte, 16).unwrap())
            .collect()
    }
}
