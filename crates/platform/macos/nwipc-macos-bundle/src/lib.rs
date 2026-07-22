//! Injected-bundle page and document lifecycle orchestration.

use std::collections::{HashMap, HashSet};

use nwipc_error::{ErrorCategory, ErrorCode, ErrorReport, Recoverability};
use nwipc_macos_bundle_api::{
    BundleEntrypoint, BundleEvent, FrameContext, InitializationData, PageId,
};
use nwipc_renderer_bootstrap::RendererAttachment;
use nwipc_types::{DocumentGeneration, Generation, SessionId};

/// JavaScript binding operations supplied by the JSC adapter.
pub trait BindingInstaller: Send {
    /// Installs the NWIPC binding for one eligible document.
    ///
    /// # Errors
    ///
    /// Returns a renderer binding installation failure.
    fn install(
        &mut self,
        context: FrameContext,
        document: DocumentGeneration,
    ) -> Result<(), ErrorReport>;
    /// Removes protected objects and callbacks for an invalidated document.
    fn invalidate(&mut self, page: PageId, document: DocumentGeneration);
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Page {
    document: DocumentGeneration,
    binding_installed: bool,
}

struct KeptAttachment<Memory, Signal> {
    _attachment: RendererAttachment<Memory, Signal>,
}

/// Generation-scoped state owned inside the `WebContent` process.
pub struct MacosBundle<Installer> {
    installer: Installer,
    initialization: Option<InitializationData>,
    attachment: Option<Box<dyn Send>>,
    identity: Option<(SessionId, Generation)>,
    pages: HashMap<PageId, Page>,
    destroyed_pages: HashSet<PageId>,
    next_document: u64,
}

impl<Installer: BindingInstaller> MacosBundle<Installer> {
    /// Creates an inert bundle. Bindings remain closed until [`Self::activate`] succeeds.
    pub fn new(installer: Installer) -> Self {
        Self {
            installer,
            initialization: None,
            attachment: None,
            identity: None,
            pages: HashMap::new(),
            destroyed_pages: HashSet::new(),
            next_document: 1,
        }
    }

    /// Retains validated provider resources and opens the binding-install gate.
    pub fn activate<Memory: Send + 'static, Signal: Send + 'static>(
        &mut self,
        attachment: RendererAttachment<Memory, Signal>,
    ) {
        self.identity = Some((
            attachment.envelope().session_id(),
            attachment.envelope().generation(),
        ));
        self.attachment = Some(Box::new(KeptAttachment {
            _attachment: attachment,
        }));
    }

    /// Active session and resource generation.
    pub const fn identity(&self) -> Option<(SessionId, Generation)> {
        self.identity
    }

    /// Returns whether an eligible binding is installed for a page.
    pub fn binding_is_installed(&self, page: PageId) -> bool {
        self.pages
            .get(&page)
            .is_some_and(|page| page.binding_installed)
    }

    fn create_page(&mut self, page: PageId) -> Result<(), ErrorReport> {
        if self.destroyed_pages.contains(&page) || self.pages.contains_key(&page) {
            return Err(lifecycle_error("bundle page creation"));
        }
        let document = self.allocate_document()?;
        self.pages.insert(
            page,
            Page {
                document,
                binding_installed: false,
            },
        );
        Ok(())
    }

    fn install(&mut self, context: FrameContext) -> Result<(), ErrorReport> {
        if !context.binding_is_allowed() {
            return Ok(());
        }
        if self.attachment.is_none() || self.initialization.is_none() {
            return Err(bootstrap_error("bundle binding before attach"));
        }
        let page = self
            .pages
            .get_mut(&context.page)
            .ok_or_else(|| lifecycle_error("bundle unknown page"))?;
        if page.binding_installed {
            return Ok(());
        }
        self.installer.install(context, page.document)?;
        page.binding_installed = true;
        Ok(())
    }

    fn invalidate_document(&mut self, page_id: PageId) -> Result<(), ErrorReport> {
        let next_document = self.allocate_document()?;
        let page = self
            .pages
            .get_mut(&page_id)
            .ok_or_else(|| lifecycle_error("bundle document invalidation"))?;
        self.installer.invalidate(page_id, page.document);
        *page = Page {
            document: next_document,
            binding_installed: false,
        };
        Ok(())
    }

    fn destroy_page(&mut self, page_id: PageId) -> Result<(), ErrorReport> {
        let page = self
            .pages
            .remove(&page_id)
            .ok_or_else(|| lifecycle_error("bundle page destruction"))?;
        self.installer.invalidate(page_id, page.document);
        self.destroyed_pages.insert(page_id);
        Ok(())
    }

    fn allocate_document(&mut self) -> Result<DocumentGeneration, ErrorReport> {
        let document = DocumentGeneration::new(self.next_document)
            .ok_or_else(|| lifecycle_error("bundle document identity"))?;
        self.next_document = self
            .next_document
            .checked_add(1)
            .ok_or_else(|| lifecycle_error("bundle document identity"))?;
        Ok(document)
    }
}

impl<Installer: BindingInstaller> BundleEntrypoint for MacosBundle<Installer> {
    fn handle(&mut self, event: BundleEvent<'_>) -> Result<(), ErrorReport> {
        match event {
            BundleEvent::Initialize(bytes) => {
                if self.initialization.is_some() {
                    return Err(lifecycle_error("duplicate bundle initialization"));
                }
                self.initialization = Some(InitializationData::copy_from(bytes)?);
                Ok(())
            }
            BundleEvent::PageCreated(page) => self.create_page(page),
            BundleEvent::WindowObjectCleared(context) => self.install(context),
            BundleEvent::DocumentInvalidated(page) => self.invalidate_document(page),
            BundleEvent::PageDestroyed(page) => self.destroy_page(page),
        }
    }
}

fn lifecycle_error(operation: &'static str) -> ErrorReport {
    ErrorReport::new(
        ErrorCategory::Lifecycle,
        ErrorCode::InvalidStateTransition,
        Recoverability::ReplaceEndpoint,
        operation,
    )
}

fn bootstrap_error(operation: &'static str) -> ErrorReport {
    ErrorReport::new(
        ErrorCategory::Bootstrap,
        ErrorCode::InvalidStateTransition,
        Recoverability::ReplaceEndpoint,
        operation,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use nwipc_bootstrap_schema::{
        BootstrapEnvelope, BootstrapSecret, EndpointRole, OpaqueDescriptor, ProtocolRange,
        ProviderKind,
    };
    use nwipc_renderer_bootstrap::RendererAttachment;

    #[derive(Default)]
    struct Installer {
        installed: usize,
        invalidated: usize,
    }
    impl BindingInstaller for Installer {
        fn install(&mut self, _: FrameContext, _: DocumentGeneration) -> Result<(), ErrorReport> {
            self.installed += 1;
            Ok(())
        }
        fn invalidate(&mut self, _: PageId, _: DocumentGeneration) {
            self.invalidated += 1;
        }
    }

    struct Providers;
    impl nwipc_renderer_bootstrap::RendererProviders for Providers {
        type Memory = ();
        type Signal = ();
        fn attach_memory(&mut self, _: &OpaqueDescriptor) -> Result<(), ErrorReport> {
            Ok(())
        }
        fn attach_signal(&mut self, _: &OpaqueDescriptor) -> Result<(), ErrorReport> {
            Ok(())
        }
    }

    fn attachment() -> RendererAttachment<(), ()> {
        let envelope = BootstrapEnvelope::new(
            SessionId::from_u128(1).unwrap(),
            Generation::new(1).unwrap(),
            ProtocolRange::new(1, 1).unwrap(),
            EndpointRole::Renderer,
            OpaqueDescriptor::new(ProviderKind::IoSurface, vec![1]).unwrap(),
            OpaqueDescriptor::new(ProviderKind::Hybrid, vec![2]).unwrap(),
            BootstrapSecret::new(vec![3]).unwrap(),
        )
        .unwrap();
        nwipc_renderer_bootstrap::RendererBootstrap::attach(
            envelope,
            SessionId::from_u128(1).unwrap(),
            Generation::new(1).unwrap(),
            1,
            &mut Providers,
        )
        .unwrap()
    }

    #[test]
    fn binding_requires_initialization_and_attachment() {
        let page = PageId::new(1).unwrap();
        let frame = nwipc_macos_bundle_api::FrameId::new(2).unwrap();
        let context = FrameContext {
            page,
            frame,
            is_main_frame: true,
            world: nwipc_macos_bundle_api::ScriptWorld::Normal,
        };
        let mut bundle = MacosBundle::new(Installer::default());
        bundle.handle(BundleEvent::PageCreated(page)).unwrap();
        assert!(
            bundle
                .handle(BundleEvent::WindowObjectCleared(context))
                .is_err()
        );
        bundle.handle(BundleEvent::Initialize(b"plist")).unwrap();
        bundle.activate(attachment());
        bundle
            .handle(BundleEvent::WindowObjectCleared(context))
            .unwrap();
        assert!(bundle.binding_is_installed(page));
    }

    #[test]
    fn subframes_are_ignored_and_reload_invalidates_old_document() {
        let page = PageId::new(1).unwrap();
        let frame = nwipc_macos_bundle_api::FrameId::new(2).unwrap();
        let mut bundle = MacosBundle::new(Installer::default());
        bundle.handle(BundleEvent::Initialize(b"plist")).unwrap();
        bundle.activate(attachment());
        bundle.handle(BundleEvent::PageCreated(page)).unwrap();
        bundle
            .handle(BundleEvent::WindowObjectCleared(FrameContext {
                page,
                frame,
                is_main_frame: false,
                world: nwipc_macos_bundle_api::ScriptWorld::Normal,
            }))
            .unwrap();
        assert!(!bundle.binding_is_installed(page));
        bundle
            .handle(BundleEvent::WindowObjectCleared(FrameContext {
                page,
                frame,
                is_main_frame: true,
                world: nwipc_macos_bundle_api::ScriptWorld::Normal,
            }))
            .unwrap();
        bundle
            .handle(BundleEvent::DocumentInvalidated(page))
            .unwrap();
        assert!(!bundle.binding_is_installed(page));
    }
}
