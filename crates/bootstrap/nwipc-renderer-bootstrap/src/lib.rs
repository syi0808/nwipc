//! Renderer property-list bootstrap validation and ordered provider attachment.

use std::collections::BTreeMap;

use nwipc_bootstrap_schema::{
    BootstrapEnvelope, BootstrapSecret, EndpointRole, OpaqueDescriptor, ProtocolRange,
    ProviderKind, SCHEMA_VERSION,
};
use nwipc_error::{ErrorCategory, ErrorCode, ErrorReport, Recoverability};
use nwipc_types::{Generation, SessionId};

/// Property-list scalar copied out of `WebKit` initialization user data.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PropertyValue {
    /// Unsigned integer.
    Integer(u64),
    /// Opaque data.
    Data(Vec<u8>),
    /// Nested dictionary.
    Dictionary(PropertyDictionary),
}

/// Property-list dictionary with deterministic field lookup.
pub type PropertyDictionary = BTreeMap<String, PropertyValue>;

/// Provider attachment operations owned by the injected bundle.
pub trait RendererProviders {
    /// Attached resource handles retained until installation succeeds.
    type Memory;
    /// Attached signal handles retained until installation succeeds.
    type Signal;

    /// Attaches shared memory after all envelope validation passes.
    ///
    /// # Errors
    ///
    /// Returns a structured provider attachment failure.
    fn attach_memory(&mut self, descriptor: &OpaqueDescriptor)
    -> Result<Self::Memory, ErrorReport>;
    /// Attaches notifications after memory succeeds.
    ///
    /// # Errors
    ///
    /// Returns a structured provider attachment failure.
    fn attach_signal(&mut self, descriptor: &OpaqueDescriptor)
    -> Result<Self::Signal, ErrorReport>;
}

/// Fully validated renderer resources. Constructing this value is the JS-open gate.
pub struct RendererAttachment<Memory, Signal> {
    /// Attached memory resources.
    pub memory: Memory,
    /// Attached signal resources.
    pub signal: Signal,
    envelope: BootstrapEnvelope,
}

impl<Memory, Signal> RendererAttachment<Memory, Signal> {
    /// Validated envelope identity.
    pub const fn envelope(&self) -> &BootstrapEnvelope {
        &self.envelope
    }
}

/// Stateless decoder and ordered attach coordinator.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RendererBootstrap;

impl RendererBootstrap {
    /// Decodes a strict property-list dictionary.
    ///
    /// # Errors
    ///
    /// Rejects missing, unknown, mistyped, and mismatched fields.
    pub fn decode(mut values: PropertyDictionary) -> Result<BootstrapEnvelope, ErrorReport> {
        let schema = take_u16(&mut values, "schema")?;
        if schema != SCHEMA_VERSION {
            return Err(bootstrap_error(
                ErrorCode::LayoutVersionMismatch,
                "renderer bootstrap schema",
            ));
        }
        let session_bytes = take_data(&mut values, "session")?;
        let session: [u8; 16] = session_bytes
            .try_into()
            .map_err(|_| bootstrap_error(ErrorCode::InvalidRange, "renderer bootstrap session"))?;
        let session = SessionId::from_bytes(session).ok_or_else(|| {
            bootstrap_error(ErrorCode::InvalidRange, "renderer bootstrap session")
        })?;
        let generation =
            Generation::new(take_u64(&mut values, "generation")?).ok_or_else(|| {
                bootstrap_error(ErrorCode::InvalidRange, "renderer bootstrap generation")
            })?;
        let protocols = ProtocolRange::new(
            take_u16(&mut values, "protocolMin")?,
            take_u16(&mut values, "protocolMax")?,
        )?;
        let role = u8::try_from(take_u64(&mut values, "role")?)
            .ok()
            .and_then(EndpointRole::from_wire)
            .ok_or_else(|| {
                bootstrap_error(ErrorCode::ProtocolViolation, "renderer bootstrap role")
            })?;
        let memory = take_descriptor(&mut values, "memory")?;
        let signal = take_descriptor(&mut values, "signal")?;
        let secret = BootstrapSecret::new(take_data(&mut values, "secret")?)?;
        if !values.is_empty() {
            return Err(bootstrap_error(
                ErrorCode::ProtocolViolation,
                "renderer bootstrap unknown field",
            ));
        }
        BootstrapEnvelope::new(session, generation, protocols, role, memory, signal, secret)
    }

    /// Validates endpoint identity before attaching either provider.
    ///
    /// # Errors
    ///
    /// Fails closed and drops partial memory attachment when signal attachment fails.
    pub fn attach<Providers: RendererProviders>(
        envelope: BootstrapEnvelope,
        session: SessionId,
        generation: Generation,
        protocol: u16,
        providers: &mut Providers,
    ) -> Result<RendererAttachment<Providers::Memory, Providers::Signal>, ErrorReport> {
        envelope.validate_for(EndpointRole::Renderer, session, generation, protocol)?;
        let memory = providers.attach_memory(envelope.memory())?;
        let signal = providers.attach_signal(envelope.signal())?;
        Ok(RendererAttachment {
            memory,
            signal,
            envelope,
        })
    }
}

fn take_descriptor(
    values: &mut PropertyDictionary,
    key: &'static str,
) -> Result<OpaqueDescriptor, ErrorReport> {
    let PropertyValue::Dictionary(mut descriptor) = take(values, key)? else {
        return Err(type_error());
    };
    let provider = u8::try_from(take_u64(&mut descriptor, "provider")?)
        .ok()
        .and_then(ProviderKind::from_wire)
        .ok_or_else(|| {
            bootstrap_error(ErrorCode::ProtocolViolation, "renderer bootstrap provider")
        })?;
    let bytes = take_data(&mut descriptor, "data")?;
    if !descriptor.is_empty() {
        return Err(bootstrap_error(
            ErrorCode::ProtocolViolation,
            "renderer bootstrap descriptor field",
        ));
    }
    OpaqueDescriptor::new(provider, bytes)
}

fn take(values: &mut PropertyDictionary, key: &'static str) -> Result<PropertyValue, ErrorReport> {
    values
        .remove(key)
        .ok_or_else(|| bootstrap_error(ErrorCode::Truncated, "renderer bootstrap required field"))
}

fn take_u64(values: &mut PropertyDictionary, key: &'static str) -> Result<u64, ErrorReport> {
    match take(values, key)? {
        PropertyValue::Integer(value) => Ok(value),
        _ => Err(type_error()),
    }
}

fn take_u16(values: &mut PropertyDictionary, key: &'static str) -> Result<u16, ErrorReport> {
    u16::try_from(take_u64(values, key)?).map_err(|_| type_error())
}

fn take_data(values: &mut PropertyDictionary, key: &'static str) -> Result<Vec<u8>, ErrorReport> {
    match take(values, key)? {
        PropertyValue::Data(value) => Ok(value),
        _ => Err(type_error()),
    }
}

fn type_error() -> ErrorReport {
    bootstrap_error(
        ErrorCode::ProtocolViolation,
        "renderer bootstrap field type",
    )
}

fn bootstrap_error(code: ErrorCode, operation: &'static str) -> ErrorReport {
    ErrorReport::new(
        ErrorCategory::Bootstrap,
        code,
        Recoverability::ReplaceEndpoint,
        operation,
    )
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::rc::Rc;

    use super::*;

    fn dictionary() -> PropertyDictionary {
        let descriptor = |provider| {
            BTreeMap::from([
                ("provider".into(), PropertyValue::Integer(provider)),
                ("data".into(), PropertyValue::Data(vec![1])),
            ])
        };
        BTreeMap::from([
            ("schema".into(), PropertyValue::Integer(1)),
            (
                "session".into(),
                PropertyValue::Data(1_u128.to_le_bytes().to_vec()),
            ),
            ("generation".into(), PropertyValue::Integer(1)),
            ("protocolMin".into(), PropertyValue::Integer(1)),
            ("protocolMax".into(), PropertyValue::Integer(1)),
            ("role".into(), PropertyValue::Integer(2)),
            ("memory".into(), PropertyValue::Dictionary(descriptor(2))),
            ("signal".into(), PropertyValue::Dictionary(descriptor(5))),
            ("secret".into(), PropertyValue::Data(vec![9; 32])),
        ])
    }

    #[test]
    fn strict_decode_accepts_renderer_envelope() {
        let envelope = RendererBootstrap::decode(dictionary()).unwrap();
        assert_eq!(envelope.role(), EndpointRole::Renderer);
    }

    #[test]
    fn missing_and_unknown_fields_fail_closed() {
        let mut missing = dictionary();
        missing.remove("secret");
        assert_eq!(
            RendererBootstrap::decode(missing).unwrap_err().code(),
            ErrorCode::Truncated
        );
        let mut unknown = dictionary();
        unknown.insert("future".into(), PropertyValue::Integer(1));
        assert!(RendererBootstrap::decode(unknown).is_err());
    }

    #[test]
    fn signal_failure_drops_partial_memory_attachment() {
        struct Memory(Rc<Cell<bool>>);
        impl Drop for Memory {
            fn drop(&mut self) {
                self.0.set(true);
            }
        }
        struct Providers(Rc<Cell<bool>>);
        impl RendererProviders for Providers {
            type Memory = Memory;
            type Signal = ();
            fn attach_memory(&mut self, _: &OpaqueDescriptor) -> Result<Self::Memory, ErrorReport> {
                Ok(Memory(Rc::clone(&self.0)))
            }
            fn attach_signal(&mut self, _: &OpaqueDescriptor) -> Result<Self::Signal, ErrorReport> {
                Err(ErrorReport::unsupported("signal attach"))
            }
        }

        let dropped = Rc::new(Cell::new(false));
        let envelope = RendererBootstrap::decode(dictionary()).unwrap();
        let result = RendererBootstrap::attach(
            envelope,
            SessionId::from_u128(1).unwrap(),
            Generation::new(1).unwrap(),
            1,
            &mut Providers(Rc::clone(&dropped)),
        );
        assert!(result.is_err());
        assert!(dropped.get());
    }
}
