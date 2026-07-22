//! Stable, WebKit-header-free contract between the injected-bundle shim and Rust orchestration.

use nwipc_error::{ErrorCategory, ErrorCode, ErrorReport, Recoverability};

/// Maximum initialization parameter accepted across the bundle boundary.
pub const MAX_INITIALIZATION_DATA: usize = 16 * 1024;

/// Opaque `WebKit` page identity, never dereferenced by portable code.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[repr(transparent)]
pub struct PageId(u64);

impl PageId {
    /// Creates a non-zero page identity.
    pub const fn new(value: u64) -> Option<Self> {
        if value == 0 { None } else { Some(Self(value)) }
    }

    /// Returns the embedding-provided value.
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// Opaque `WebKit` frame identity.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[repr(transparent)]
pub struct FrameId(u64);

impl FrameId {
    /// Creates a non-zero frame identity.
    pub const fn new(value: u64) -> Option<Self> {
        if value == 0 { None } else { Some(Self(value)) }
    }
}

/// Script world in which a window-object callback occurred.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ScriptWorld {
    /// Page's normal JavaScript world.
    Normal,
    /// An isolated application or extension world.
    Isolated,
}

/// Data needed to decide whether a JavaScript binding may be installed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FrameContext {
    /// Owning page.
    pub page: PageId,
    /// Frame receiving the global object.
    pub frame: FrameId,
    /// True only for the page's main frame.
    pub is_main_frame: bool,
    /// JavaScript execution world.
    pub world: ScriptWorld,
}

impl FrameContext {
    /// Returns true only for the main frame in the normal world.
    pub const fn binding_is_allowed(self) -> bool {
        self.is_main_frame && matches!(self.world, ScriptWorld::Normal)
    }
}

/// Owned, bounded initialization data copied from the host.
#[derive(Eq, PartialEq)]
pub struct InitializationData(Vec<u8>);

impl InitializationData {
    /// Copies a property-list compatible bootstrap envelope.
    ///
    /// # Errors
    ///
    /// Rejects missing and oversized parameters before bundle initialization.
    pub fn copy_from(bytes: &[u8]) -> Result<Self, ErrorReport> {
        if bytes.is_empty() || bytes.len() > MAX_INITIALIZATION_DATA {
            return Err(boundary_error("bundle initialization data"));
        }
        Ok(Self(bytes.to_vec()))
    }

    /// Borrows the copied bytes for one decoder invocation.
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

impl core::fmt::Debug for InitializationData {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter.write_fmt(format_args!(
            "InitializationData(<redacted:{} bytes>)",
            self.0.len()
        ))
    }
}

impl Drop for InitializationData {
    fn drop(&mut self) {
        self.0.fill(0);
    }
}

/// Callback input normalized by the C/Objective-C shim.
#[derive(Debug)]
pub enum BundleEvent<'a> {
    /// `WebKit` created a page.
    PageCreated(PageId),
    /// A frame received a new normal or isolated world global object.
    WindowObjectCleared(FrameContext),
    /// `WebKit` invalidated a document before a replacement is installed.
    DocumentInvalidated(PageId),
    /// `WebKit` destroyed a page.
    PageDestroyed(PageId),
    /// Initialization user data arrived.
    Initialize(&'a [u8]),
}

/// Bundle implementation consumed only by the shim.
pub trait BundleEntrypoint: Send {
    /// Delivers one normalized lifecycle event.
    ///
    /// # Errors
    ///
    /// Returns a structured bootstrap, lifecycle, or platform failure.
    fn handle(&mut self, event: BundleEvent<'_>) -> Result<(), ErrorReport>;
}

fn boundary_error(operation: &'static str) -> ErrorReport {
    ErrorReport::new(
        ErrorCategory::Bootstrap,
        ErrorCode::InvalidRange,
        Recoverability::ReplaceEndpoint,
        operation,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_main_normal_world_is_eligible() {
        let page = PageId::new(1).unwrap();
        let frame = FrameId::new(2).unwrap();
        for (main, world, expected) in [
            (true, ScriptWorld::Normal, true),
            (false, ScriptWorld::Normal, false),
            (true, ScriptWorld::Isolated, false),
        ] {
            assert_eq!(
                FrameContext {
                    page,
                    frame,
                    is_main_frame: main,
                    world
                }
                .binding_is_allowed(),
                expected
            );
        }
    }

    #[test]
    fn initialization_data_is_bounded_and_redacted() {
        assert!(InitializationData::copy_from(&[]).is_err());
        let data = InitializationData::copy_from(b"secret").unwrap();
        assert!(!format!("{data:?}").contains("secret"));
    }
}
