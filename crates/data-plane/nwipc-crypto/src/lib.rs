//! Generation-bound authenticated encryption for complete transport frames.
//!
//! A one-shot bootstrap secret is expanded into independent renderer-to-peer and
//! peer-to-renderer keys. The protected frame counter is authenticated and must be received in
//! strict FIFO order, making replay, deletion, and reordering terminal for the generation.

use std::fmt;

use chacha20poly1305::aead::{AeadInPlace, KeyInit};
use chacha20poly1305::{Key, Tag, XChaCha20Poly1305, XNonce};
use hkdf::Hkdf;
use nwipc_error::{ErrorCategory, ErrorCode, ErrorReport, Recoverability};
use nwipc_types::{Generation, SessionId};
use sha2::Sha256;
use zeroize::Zeroize;

const KEY_LENGTH: usize = 32;
const NONCE_PREFIX_LENGTH: usize = 16;
const COUNTER_LENGTH: usize = 8;
const TAG_LENGTH: usize = 16;
const DERIVED_LENGTH: usize = KEY_LENGTH + NONCE_PREFIX_LENGTH;
const CONTEXT_LABEL: &[u8] = b"nwipc frame protection v1";
/// Entropy required for a production bootstrap secret.
pub const MINIMUM_SECRET_LENGTH: usize = 32;
/// Bytes added to every protected transport frame.
pub const FRAME_OVERHEAD: usize = COUNTER_LENGTH + TAG_LENGTH;

/// Endpoint role used to select independent directional keys.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EndpointRole {
    /// `WebKit` renderer endpoint.
    Renderer,
    /// Native peer endpoint.
    Peer,
}

impl EndpointRole {
    const fn outbound_direction(self) -> Direction {
        match self {
            Self::Renderer => Direction::RendererToPeer,
            Self::Peer => Direction::PeerToRenderer,
        }
    }

    const fn inbound_direction(self) -> Direction {
        match self {
            Self::Renderer => Direction::PeerToRenderer,
            Self::Peer => Direction::RendererToPeer,
        }
    }
}

#[derive(Clone, Copy)]
enum Direction {
    RendererToPeer,
    PeerToRenderer,
}

impl Direction {
    const fn label(self) -> &'static [u8] {
        match self {
            Self::RendererToPeer => b"renderer-to-peer",
            Self::PeerToRenderer => b"peer-to-renderer",
        }
    }
}

/// Bidirectional frame protection bound to one session generation and endpoint role.
pub struct EndpointProtection {
    outbound: FrameSealer,
    inbound: FrameOpener,
}

impl EndpointProtection {
    /// Derives independent directional keys from bootstrap authentication material.
    ///
    /// # Errors
    ///
    /// Rejects secrets shorter than 32 bytes and key-derivation failures.
    pub fn derive(
        secret: &[u8],
        session_id: SessionId,
        generation: Generation,
        role: EndpointRole,
    ) -> Result<Self, ErrorReport> {
        if secret.len() < MINIMUM_SECRET_LENGTH {
            return Err(configuration_error("frame protection secret length"));
        }
        Ok(Self {
            outbound: FrameSealer::derive(
                secret,
                session_id,
                generation,
                role.outbound_direction(),
            )?,
            inbound: FrameOpener::derive(secret, session_id, generation, role.inbound_direction())?,
        })
    }

    /// Authenticates and encrypts a frame without consuming its send counter until committed.
    ///
    /// Dropping the returned pending frame leaves the counter unchanged, so backpressure and
    /// crash-before-commit do not create a gap in the authenticated FIFO sequence.
    ///
    /// # Errors
    ///
    /// Returns a terminal security error after the direction exhausts its 64-bit counter.
    pub fn prepare<'protection>(
        &'protection mut self,
        plaintext: &[u8],
    ) -> Result<PendingProtectedFrame<'protection>, ErrorReport> {
        self.outbound.prepare(plaintext)
    }

    /// Authenticates, replay-checks, and decrypts the next inbound frame.
    ///
    /// # Errors
    ///
    /// Rejects truncated, tampered, replayed, reordered, or wrong-generation frames.
    pub fn open(&mut self, protected: &[u8]) -> Result<Vec<u8>, ErrorReport> {
        self.inbound.open(protected)
    }
}

impl fmt::Debug for EndpointProtection {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("EndpointProtection")
            .field("key_material", &"<redacted>")
            .finish()
    }
}

struct FrameSealer {
    cipher: XChaCha20Poly1305,
    nonce_prefix: [u8; NONCE_PREFIX_LENGTH],
    aad_prefix: [u8; 24],
    next_counter: Option<u64>,
}

impl FrameSealer {
    fn derive(
        secret: &[u8],
        session_id: SessionId,
        generation: Generation,
        direction: Direction,
    ) -> Result<Self, ErrorReport> {
        let (cipher, nonce_prefix, aad_prefix) =
            derive_direction(secret, session_id, generation, direction)?;
        Ok(Self {
            cipher,
            nonce_prefix,
            aad_prefix,
            next_counter: Some(0),
        })
    }

    fn prepare<'sealer>(
        &'sealer mut self,
        plaintext: &[u8],
    ) -> Result<PendingProtectedFrame<'sealer>, ErrorReport> {
        let counter = self.next_counter.ok_or_else(|| {
            security_error(ErrorCode::ProtocolViolation, "frame counter exhausted")
        })?;
        let mut bytes = Vec::with_capacity(
            plaintext
                .len()
                .checked_add(FRAME_OVERHEAD)
                .ok_or_else(|| configuration_error("protected frame length"))?,
        );
        bytes.extend_from_slice(&counter.to_le_bytes());
        bytes.extend_from_slice(plaintext);
        let tag = self
            .cipher
            .encrypt_in_place_detached(
                &nonce(&self.nonce_prefix, counter),
                &aad(&self.aad_prefix, counter),
                &mut bytes[COUNTER_LENGTH..],
            )
            .map_err(|_| security_error(ErrorCode::AuthenticationFailed, "seal transport frame"))?;
        bytes.extend_from_slice(tag.as_slice());
        Ok(PendingProtectedFrame {
            sealer: self,
            counter,
            bytes,
        })
    }
}

/// Authenticated ciphertext awaiting publication by the underlying channel.
pub struct PendingProtectedFrame<'sealer> {
    sealer: &'sealer mut FrameSealer,
    counter: u64,
    bytes: Vec<u8>,
}

impl PendingProtectedFrame<'_> {
    /// Returns the complete protected wire frame.
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Advances the directional counter after the channel cursor is published.
    pub fn commit(self) {
        self.sealer.next_counter = self.counter.checked_add(1);
    }
}

struct FrameOpener {
    cipher: XChaCha20Poly1305,
    nonce_prefix: [u8; NONCE_PREFIX_LENGTH],
    aad_prefix: [u8; 24],
    expected_counter: Option<u64>,
}

impl FrameOpener {
    fn derive(
        secret: &[u8],
        session_id: SessionId,
        generation: Generation,
        direction: Direction,
    ) -> Result<Self, ErrorReport> {
        let (cipher, nonce_prefix, aad_prefix) =
            derive_direction(secret, session_id, generation, direction)?;
        Ok(Self {
            cipher,
            nonce_prefix,
            aad_prefix,
            expected_counter: Some(0),
        })
    }

    fn open(&mut self, protected: &[u8]) -> Result<Vec<u8>, ErrorReport> {
        if protected.len() < FRAME_OVERHEAD {
            return Err(security_error(
                ErrorCode::AuthenticationFailed,
                "open truncated transport frame",
            ));
        }
        let counter = u64::from_le_bytes(
            protected[..COUNTER_LENGTH]
                .try_into()
                .expect("counter length was validated"),
        );
        if self.expected_counter != Some(counter) {
            return Err(security_error(
                ErrorCode::ReplayDetected,
                "transport frame sequence",
            ));
        }
        let tag_offset = protected.len() - TAG_LENGTH;
        let mut plaintext = protected[COUNTER_LENGTH..tag_offset].to_vec();
        let tag = Tag::from_slice(&protected[tag_offset..]);
        self.cipher
            .decrypt_in_place_detached(
                &nonce(&self.nonce_prefix, counter),
                &aad(&self.aad_prefix, counter),
                &mut plaintext,
                tag,
            )
            .map_err(|_| {
                security_error(
                    ErrorCode::AuthenticationFailed,
                    "authenticate transport frame",
                )
            })?;
        self.expected_counter = counter.checked_add(1);
        Ok(plaintext)
    }
}

type DerivedDirection = (XChaCha20Poly1305, [u8; NONCE_PREFIX_LENGTH], [u8; 24]);

fn derive_direction(
    secret: &[u8],
    session_id: SessionId,
    generation: Generation,
    direction: Direction,
) -> Result<DerivedDirection, ErrorReport> {
    let mut salt = [0; 24];
    salt[..16].copy_from_slice(&session_id.to_bytes());
    salt[16..].copy_from_slice(&generation.get().to_le_bytes());
    let hkdf = Hkdf::<Sha256>::new(Some(&salt), secret);
    let mut derived = [0; DERIVED_LENGTH];
    let mut info = Vec::with_capacity(CONTEXT_LABEL.len() + direction.label().len() + 1);
    info.extend_from_slice(CONTEXT_LABEL);
    info.push(0);
    info.extend_from_slice(direction.label());
    hkdf.expand(&info, &mut derived)
        .map_err(|_| configuration_error("derive frame protection key"))?;
    let cipher = XChaCha20Poly1305::new(Key::from_slice(&derived[..KEY_LENGTH]));
    let mut nonce_prefix = [0; NONCE_PREFIX_LENGTH];
    nonce_prefix.copy_from_slice(&derived[KEY_LENGTH..]);
    derived.zeroize();
    let mut aad_prefix = [0; 24];
    aad_prefix[..16].copy_from_slice(&session_id.to_bytes());
    aad_prefix[16..].copy_from_slice(&generation.get().to_le_bytes());
    Ok((cipher, nonce_prefix, aad_prefix))
}

fn nonce(prefix: &[u8; NONCE_PREFIX_LENGTH], counter: u64) -> XNonce {
    let mut nonce = [0; 24];
    nonce[..NONCE_PREFIX_LENGTH].copy_from_slice(prefix);
    nonce[NONCE_PREFIX_LENGTH..].copy_from_slice(&counter.to_le_bytes());
    XNonce::from(nonce)
}

fn aad(prefix: &[u8; 24], counter: u64) -> [u8; 32] {
    let mut aad = [0; 32];
    aad[..24].copy_from_slice(prefix);
    aad[24..].copy_from_slice(&counter.to_le_bytes());
    aad
}

fn configuration_error(operation: &'static str) -> ErrorReport {
    ErrorReport::new(
        ErrorCategory::Configuration,
        ErrorCode::InvalidRange,
        Recoverability::Terminal,
        operation,
    )
}

fn security_error(code: ErrorCode, operation: &'static str) -> ErrorReport {
    ErrorReport::new(
        ErrorCategory::Security,
        code,
        Recoverability::ReplaceEndpoint,
        operation,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn endpoints(secret: &[u8], generation: u64) -> (EndpointProtection, EndpointProtection) {
        let session = SessionId::from_u128(0xfeed).unwrap();
        let generation = Generation::new(generation).unwrap();
        (
            EndpointProtection::derive(secret, session, generation, EndpointRole::Renderer)
                .unwrap(),
            EndpointProtection::derive(secret, session, generation, EndpointRole::Peer).unwrap(),
        )
    }

    fn seal(protection: &mut EndpointProtection, plaintext: &[u8]) -> Vec<u8> {
        let pending = protection.prepare(plaintext).unwrap();
        let protected = pending.bytes().to_vec();
        pending.commit();
        protected
    }

    #[test]
    fn exchanges_confidential_bidirectional_frames() {
        let secret = [0x42; MINIMUM_SECRET_LENGTH];
        let (mut renderer, mut peer) = endpoints(&secret, 7);
        let request = seal(&mut renderer, b"renderer secret payload");
        assert!(
            !request
                .windows(b"secret payload".len())
                .any(|window| window == b"secret payload")
        );
        assert_eq!(peer.open(&request).unwrap(), b"renderer secret payload");
        let response = seal(&mut peer, b"peer response");
        assert_eq!(renderer.open(&response).unwrap(), b"peer response");
    }

    #[test]
    fn tamper_does_not_advance_receive_sequence() {
        let secret = [0x24; MINIMUM_SECRET_LENGTH];
        let (mut renderer, mut peer) = endpoints(&secret, 3);
        let protected = seal(&mut renderer, b"valid");
        let mut ciphertext_tamper = protected.clone();
        ciphertext_tamper[COUNTER_LENGTH] ^= 1;
        let error = peer.open(&ciphertext_tamper).unwrap_err();
        assert_eq!(error.category(), ErrorCategory::Security);
        assert_eq!(error.code(), ErrorCode::AuthenticationFailed);
        let mut tag_tamper = protected.clone();
        *tag_tamper.last_mut().unwrap() ^= 1;
        assert_eq!(
            peer.open(&tag_tamper).unwrap_err().code(),
            ErrorCode::AuthenticationFailed
        );
        let mut counter_tamper = protected.clone();
        counter_tamper[0] ^= 1;
        assert_eq!(
            peer.open(&counter_tamper).unwrap_err().code(),
            ErrorCode::ReplayDetected
        );
        assert_eq!(peer.open(&protected).unwrap(), b"valid");
    }

    #[test]
    fn rejects_replay_wrong_generation_and_wrong_secret() {
        let secret = [0x18; MINIMUM_SECRET_LENGTH];
        let (mut renderer, mut peer) = endpoints(&secret, 1);
        let protected = seal(&mut renderer, b"once");
        assert_eq!(peer.open(&protected).unwrap(), b"once");
        assert_eq!(
            peer.open(&protected).unwrap_err().code(),
            ErrorCode::ReplayDetected
        );

        let (_, mut next_generation) = endpoints(&secret, 2);
        assert_eq!(
            next_generation.open(&protected).unwrap_err().code(),
            ErrorCode::AuthenticationFailed
        );
        let (_, mut wrong_secret) = endpoints(&[0x81; MINIMUM_SECRET_LENGTH], 1);
        assert_eq!(
            wrong_secret.open(&protected).unwrap_err().code(),
            ErrorCode::AuthenticationFailed
        );
    }

    #[test]
    fn dropped_pending_frame_reuses_unpublished_counter() {
        let secret = [0x55; MINIMUM_SECRET_LENGTH];
        let (mut renderer, mut peer) = endpoints(&secret, 4);
        let discarded = renderer.prepare(b"discarded").unwrap();
        assert_eq!(&discarded.bytes()[..COUNTER_LENGTH], &0_u64.to_le_bytes());
        drop(discarded);
        let committed = seal(&mut renderer, b"committed");
        assert_eq!(&committed[..COUNTER_LENGTH], &0_u64.to_le_bytes());
        assert_eq!(peer.open(&committed).unwrap(), b"committed");
    }

    #[test]
    fn rejects_weak_secret_and_redacts_debug_output() {
        let session = SessionId::from_u128(1).unwrap();
        let generation = Generation::new(1).unwrap();
        assert_eq!(
            EndpointProtection::derive(b"short", session, generation, EndpointRole::Renderer)
                .unwrap_err()
                .code(),
            ErrorCode::InvalidRange
        );
        let protection = EndpointProtection::derive(
            &[0x99; MINIMUM_SECRET_LENGTH],
            session,
            generation,
            EndpointRole::Renderer,
        )
        .unwrap();
        let debug = format!("{protection:?}");
        assert!(debug.contains("<redacted>"));
        assert!(!debug.contains("99"));
    }
}
