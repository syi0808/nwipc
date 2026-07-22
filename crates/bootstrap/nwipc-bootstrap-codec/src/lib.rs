//! Deterministic binary bootstrap envelope codec.

use nwipc_bootstrap_schema::{
    BootstrapEnvelope, BootstrapSecret, EndpointRole, MAX_ENVELOPE_LENGTH, OpaqueDescriptor,
    ProtocolRange, ProviderKind, SCHEMA_VERSION,
};
use nwipc_error::{ErrorCategory, ErrorCode, ErrorReport, Recoverability};
use nwipc_types::{Generation, SessionId};

const MAGIC: &[u8; 4] = b"NWBS";
const HEADER_LENGTH: usize = 6;
const FIELD_HEADER_LENGTH: usize = 6;
const REQUIRED: u16 = 1 << 15;
const SESSION: u16 = 1;
const GENERATION: u16 = 2;
const PROTOCOLS: u16 = 3;
const ROLE: u16 = 4;
const MEMORY: u16 = 5;
const SIGNAL: u16 = 6;
const SECRET: u16 = 7;
const REQUIRED_FIELDS: u16 = (1 << 7) - 1;

/// Encodes a complete envelope into its canonical peer wire representation.
///
/// # Errors
///
/// Returns `InvalidRange` if the encoded representation exceeds its fixed maximum.
pub fn encode(envelope: &BootstrapEnvelope) -> Result<Vec<u8>, ErrorReport> {
    envelope.validate()?;
    let mut output = Vec::with_capacity(128);
    output.extend_from_slice(MAGIC);
    output.extend_from_slice(&SCHEMA_VERSION.to_le_bytes());
    field(&mut output, SESSION, &envelope.session_id().to_bytes())?;
    field(
        &mut output,
        GENERATION,
        &envelope.generation().get().to_le_bytes(),
    )?;
    let mut protocols = [0; 4];
    protocols[..2].copy_from_slice(&envelope.protocols().minimum().to_le_bytes());
    protocols[2..].copy_from_slice(&envelope.protocols().maximum().to_le_bytes());
    field(&mut output, PROTOCOLS, &protocols)?;
    field(&mut output, ROLE, &[envelope.role() as u8])?;
    descriptor(&mut output, MEMORY, envelope.memory())?;
    descriptor(&mut output, SIGNAL, envelope.signal())?;
    field(&mut output, SECRET, envelope.secret().expose())?;
    if output.len() > MAX_ENVELOPE_LENGTH {
        return Err(codec_error(ErrorCode::InvalidRange, "encode bootstrap"));
    }
    Ok(output)
}

/// Decodes an exact bootstrap envelope, rejecting trailing, duplicate, and required unknown fields.
///
/// # Errors
///
/// Returns a typed bootstrap error for malformed or unsupported input.
pub fn decode(input: &[u8]) -> Result<BootstrapEnvelope, ErrorReport> {
    if input.len() > MAX_ENVELOPE_LENGTH {
        return Err(codec_error(
            ErrorCode::InvalidRange,
            "decode bootstrap length",
        ));
    }
    if input.len() < HEADER_LENGTH {
        return Err(codec_error(ErrorCode::Truncated, "decode bootstrap header"));
    }
    if &input[..4] != MAGIC {
        return Err(codec_error(
            ErrorCode::InvalidMagic,
            "decode bootstrap magic",
        ));
    }
    if u16::from_le_bytes([input[4], input[5]]) != SCHEMA_VERSION {
        return Err(codec_error(
            ErrorCode::LayoutVersionMismatch,
            "decode bootstrap schema",
        ));
    }

    let mut cursor = HEADER_LENGTH;
    let mut seen = 0_u16;
    let mut session = None;
    let mut generation = None;
    let mut protocols = None;
    let mut role = None;
    let mut memory = None;
    let mut signal = None;
    let mut secret = None;

    while cursor < input.len() {
        let header = take(input, &mut cursor, FIELD_HEADER_LENGTH)?;
        let wire_kind = u16::from_le_bytes([header[0], header[1]]);
        let kind = wire_kind & !REQUIRED;
        let length = u32::from_le_bytes([header[2], header[3], header[4], header[5]]);
        let length = usize::try_from(length)
            .map_err(|_| codec_error(ErrorCode::InvalidRange, "decode bootstrap field"))?;
        let value = take(input, &mut cursor, length)?;
        if !(SESSION..=SECRET).contains(&kind) {
            if wire_kind & REQUIRED != 0 {
                return Err(codec_error(
                    ErrorCode::UnknownRequiredFlag,
                    "decode required bootstrap field",
                ));
            }
            continue;
        }
        let bit = 1_u16 << (kind - 1);
        if seen & bit != 0 {
            return Err(codec_error(
                ErrorCode::ProtocolViolation,
                "decode duplicate bootstrap field",
            ));
        }
        seen |= bit;
        match kind {
            SESSION => session = Some(decode_session(value)?),
            GENERATION => generation = Some(decode_generation(value)?),
            PROTOCOLS => protocols = Some(decode_protocols(value)?),
            ROLE => role = Some(decode_role(value)?),
            MEMORY => memory = Some(decode_descriptor(value)?),
            SIGNAL => signal = Some(decode_descriptor(value)?),
            SECRET => secret = Some(BootstrapSecret::new(value.to_vec())?),
            _ => unreachable!(),
        }
    }
    if seen != REQUIRED_FIELDS {
        return Err(codec_error(
            ErrorCode::Truncated,
            "decode required bootstrap fields",
        ));
    }
    BootstrapEnvelope::new(
        session.ok_or_else(missing_field)?,
        generation.ok_or_else(missing_field)?,
        protocols.ok_or_else(missing_field)?,
        role.ok_or_else(missing_field)?,
        memory.ok_or_else(missing_field)?,
        signal.ok_or_else(missing_field)?,
        secret.ok_or_else(missing_field)?,
    )
}

fn missing_field() -> ErrorReport {
    codec_error(ErrorCode::Truncated, "decode required bootstrap field")
}

fn field(output: &mut Vec<u8>, kind: u16, value: &[u8]) -> Result<(), ErrorReport> {
    let length = u32::try_from(value.len())
        .map_err(|_| codec_error(ErrorCode::InvalidRange, "encode bootstrap field"))?;
    output.extend_from_slice(&(kind | REQUIRED).to_le_bytes());
    output.extend_from_slice(&length.to_le_bytes());
    output.extend_from_slice(value);
    Ok(())
}

fn descriptor(
    output: &mut Vec<u8>,
    kind: u16,
    descriptor: &OpaqueDescriptor,
) -> Result<(), ErrorReport> {
    let mut value = Vec::with_capacity(descriptor.bytes().len() + 1);
    value.push(descriptor.provider() as u8);
    value.extend_from_slice(descriptor.bytes());
    field(output, kind, &value)
}

fn take<'a>(input: &'a [u8], cursor: &mut usize, length: usize) -> Result<&'a [u8], ErrorReport> {
    let end = cursor
        .checked_add(length)
        .filter(|end| *end <= input.len())
        .ok_or_else(|| codec_error(ErrorCode::Truncated, "decode bootstrap field"))?;
    let value = &input[*cursor..end];
    *cursor = end;
    Ok(value)
}

fn decode_session(value: &[u8]) -> Result<SessionId, ErrorReport> {
    let bytes: [u8; 16] = value
        .try_into()
        .map_err(|_| codec_error(ErrorCode::InvalidRange, "decode bootstrap session"))?;
    SessionId::from_bytes(bytes)
        .ok_or_else(|| codec_error(ErrorCode::ProtocolViolation, "decode bootstrap session"))
}

fn decode_generation(value: &[u8]) -> Result<Generation, ErrorReport> {
    let bytes: [u8; 8] = value
        .try_into()
        .map_err(|_| codec_error(ErrorCode::InvalidRange, "decode bootstrap generation"))?;
    Generation::new(u64::from_le_bytes(bytes))
        .ok_or_else(|| codec_error(ErrorCode::ProtocolViolation, "decode bootstrap generation"))
}

fn decode_protocols(value: &[u8]) -> Result<ProtocolRange, ErrorReport> {
    let bytes: [u8; 4] = value
        .try_into()
        .map_err(|_| codec_error(ErrorCode::InvalidRange, "decode bootstrap protocols"))?;
    ProtocolRange::new(
        u16::from_le_bytes([bytes[0], bytes[1]]),
        u16::from_le_bytes([bytes[2], bytes[3]]),
    )
}

fn decode_role(value: &[u8]) -> Result<EndpointRole, ErrorReport> {
    let &[wire] = value else {
        return Err(codec_error(
            ErrorCode::InvalidRange,
            "decode bootstrap role",
        ));
    };
    EndpointRole::from_wire(wire)
        .ok_or_else(|| codec_error(ErrorCode::ProtocolViolation, "decode bootstrap role"))
}

fn decode_descriptor(value: &[u8]) -> Result<OpaqueDescriptor, ErrorReport> {
    let (&provider, bytes) = value
        .split_first()
        .ok_or_else(|| codec_error(ErrorCode::Truncated, "decode bootstrap descriptor"))?;
    let provider = ProviderKind::from_wire(provider)
        .ok_or_else(|| codec_error(ErrorCode::ProtocolViolation, "decode bootstrap provider"))?;
    OpaqueDescriptor::new(provider, bytes.to_vec())
}

fn codec_error(code: ErrorCode, operation: &'static str) -> ErrorReport {
    ErrorReport::new(
        ErrorCategory::Bootstrap,
        code,
        Recoverability::ReplaceEndpoint,
        operation,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn envelope() -> BootstrapEnvelope {
        BootstrapEnvelope::new(
            SessionId::from_u128(0x0102).unwrap(),
            Generation::new(9).unwrap(),
            ProtocolRange::new(1, 3).unwrap(),
            EndpointRole::Peer,
            OpaqueDescriptor::new(ProviderKind::ProcessTest, b"memory".to_vec()).unwrap(),
            OpaqueDescriptor::new(ProviderKind::Poll, b"signal".to_vec()).unwrap(),
            BootstrapSecret::new(vec![0xa5; 16]).unwrap(),
        )
        .unwrap()
    }

    #[test]
    fn round_trips_canonical_envelope() {
        let encoded = encode(&envelope()).unwrap();
        let expected = decode_hex(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../../tests/protocol-fixtures/bootstrap-v1.hex"
        )));
        assert_eq!(encoded, expected);
        assert_eq!(decode(&encoded).unwrap(), envelope());
    }

    #[test]
    fn rejects_truncation_duplicate_and_required_unknown() {
        let encoded = encode(&envelope()).unwrap();
        assert_eq!(
            decode(&encoded[..encoded.len() - 1]).unwrap_err().code(),
            ErrorCode::Truncated
        );

        let mut duplicate = encoded.clone();
        duplicate.extend_from_slice(&(SESSION | REQUIRED).to_le_bytes());
        duplicate.extend_from_slice(&16_u32.to_le_bytes());
        duplicate.extend_from_slice(&[1; 16]);
        assert_eq!(
            decode(&duplicate).unwrap_err().code(),
            ErrorCode::ProtocolViolation
        );

        let mut unknown = encoded;
        unknown.extend_from_slice(&(99_u16 | REQUIRED).to_le_bytes());
        unknown.extend_from_slice(&0_u32.to_le_bytes());
        assert_eq!(
            decode(&unknown).unwrap_err().code(),
            ErrorCode::UnknownRequiredFlag
        );
    }

    #[test]
    fn ignores_unknown_optional_field() {
        let mut encoded = encode(&envelope()).unwrap();
        encoded.extend_from_slice(&99_u16.to_le_bytes());
        encoded.extend_from_slice(&3_u32.to_le_bytes());
        encoded.extend_from_slice(b"new");
        assert_eq!(decode(&encoded).unwrap(), envelope());
    }

    fn decode_hex(source: &str) -> Vec<u8> {
        source
            .split_whitespace()
            .map(|byte| u8::from_str_radix(byte, 16).unwrap())
            .collect()
    }
}
