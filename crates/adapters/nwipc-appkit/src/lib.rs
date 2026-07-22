//! Reference `AppKit` control-plane adapter with explicit capability diagnostics.

use std::path::Path;

use nwipc_capabilities::TransportTopology;
use nwipc_error::ErrorReport;
use nwipc_macos_artifact::MacosArtifact;
use nwipc_macos_host::{MacosHost, WebViewPlan};
use nwipc_macos_spi::{MacosSpi, SpiProbe};
use nwipc_types::{Generation, SessionId};

/// User-visible adapter status. Unsupported configurations never become silent no-ops.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AppKitStatus {
    /// SPI, bundle, and topology validation completed.
    Ready,
    /// Startup failed with a structured public error.
    Failed(ErrorReport),
}

/// Reference host owner used by the `AppKit` example.
pub struct AppKitAdapter {
    host: MacosHost,
}

impl AppKitAdapter {
    /// Probes SPI, inspects the bundle, and produces a pre-WebView configuration.
    ///
    /// # Errors
    ///
    /// Fails closed for an unknown OS/build, missing SPI, invalid artifact, or invalid bootstrap.
    pub fn configure(
        probe: &impl SpiProbe,
        bundle: impl AsRef<Path>,
        initialization: &[u8],
    ) -> Result<Self, ErrorReport> {
        let spi = MacosSpi::probe(probe)?;
        let artifact = MacosArtifact::inspect(bundle)?;
        let plan = WebViewPlan::new(spi, artifact, initialization, TransportTopology::direct())?;
        Ok(Self {
            host: MacosHost::new(plan),
        })
    }

    /// Registers a session that owns peer and renderer bootstrap resources.
    ///
    /// # Errors
    ///
    /// Rejects duplicate session identities.
    pub fn register_session(
        &mut self,
        session: SessionId,
        generation: Generation,
    ) -> Result<(), ErrorReport> {
        self.host.register(session, generation)
    }

    /// Routes successful injected-bundle attachment into the session lifecycle.
    ///
    /// # Errors
    ///
    /// Rejects unknown, stale, and duplicate renderer attachment events.
    pub fn renderer_attached(
        &mut self,
        session: SessionId,
        generation: Generation,
    ) -> Result<(), ErrorReport> {
        self.host.renderer_attached(session, generation)
    }

    /// Routes a renderer reload or `WebContent` process exit to generation replacement.
    ///
    /// # Errors
    ///
    /// Rejects unknown and stale session generations.
    pub fn renderer_replaced(
        &mut self,
        session: SessionId,
        generation: Generation,
    ) -> Result<Generation, ErrorReport> {
        self.host.replace_renderer(session, generation)
    }

    /// Exposes only the control-plane host for `WebView` construction.
    pub const fn host(&self) -> &MacosHost {
        &self.host
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use nwipc_macos_artifact::{BUNDLE_EXECUTABLE, MANIFEST_FILE, current_manifest};
    use nwipc_macos_spi::{OsVersion, SpiEntry};

    use super::*;

    struct SupportedProbe;
    impl SpiProbe for SupportedProbe {
        fn os_version(&self) -> Option<OsVersion> {
            Some(OsVersion {
                major: 26,
                minor: 2,
                patch: 0,
            })
        }
        fn has_entry(&self, _: SpiEntry) -> bool {
            true
        }
    }

    #[test]
    fn reload_replaces_generation_and_rejects_stale_events() {
        let bundle = std::env::temp_dir().join(format!("nwipc-appkit-test-{}", std::process::id()));
        let contents = bundle.join("Contents");
        fs::create_dir_all(contents.join("MacOS")).unwrap();
        fs::create_dir_all(contents.join("Resources")).unwrap();
        fs::write(
            contents.join("Info.plist"),
            format!("<string>{BUNDLE_EXECUTABLE}</string>"),
        )
        .unwrap();
        fs::write(contents.join("MacOS").join(BUNDLE_EXECUTABLE), b"bundle").unwrap();
        fs::write(
            contents.join("Resources").join(MANIFEST_FILE),
            current_manifest(),
        )
        .unwrap();

        let mut adapter = AppKitAdapter::configure(&SupportedProbe, &bundle, b"plist").unwrap();
        let session = SessionId::from_u128(1).unwrap();
        let generation = Generation::new(1).unwrap();
        adapter.register_session(session, generation).unwrap();
        adapter.renderer_attached(session, generation).unwrap();
        let replacement = adapter.renderer_replaced(session, generation).unwrap();
        assert_eq!(replacement.get(), 2);
        assert!(adapter.renderer_attached(session, generation).is_err());

        fs::remove_dir_all(bundle).unwrap();
    }
}
