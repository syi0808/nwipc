//! Wry control-plane integration for the production macOS `WebKit` transport.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard};

use nwipc_capabilities::TransportTopology;
use nwipc_error::{ErrorCategory, ErrorCode, ErrorReport, Recoverability};
use nwipc_macos_artifact::MacosArtifact;
use nwipc_macos_host::{MacosHost, WebViewConfigurator, WebViewPlan};
use nwipc_macos_spi::{MacosSpi, SpiProbe};
use nwipc_types::{Generation, SessionId};
#[cfg(target_os = "macos")]
use wry::WebViewBuilderExtDarwin;

/// Stable application identity for one Wry webview.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct WebViewId(String);

impl WebViewId {
    /// Validates and copies an application-provided webview ID.
    ///
    /// # Errors
    ///
    /// Rejects empty IDs and IDs longer than 128 bytes.
    pub fn new(value: impl Into<String>) -> Result<Self, ErrorReport> {
        let value = value.into();
        if value.is_empty() || value.len() > 128 {
            return Err(adapter_error(
                ErrorCode::InvalidRange,
                "Wry webview identity",
            ));
        }
        Ok(Self(value))
    }

    /// Returns the application-provided ID.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct WebViewSession {
    session: SessionId,
    generation: Generation,
}

struct State {
    host: MacosHost,
    webviews: HashMap<WebViewId, WebViewSession>,
    callback_failure: Option<ErrorReport>,
}

/// Cloneable Wry lifecycle bridge. It owns no event loop, thread, process, or payload path.
#[derive(Clone)]
pub struct WryAdapter {
    state: Arc<Mutex<State>>,
}

impl WryAdapter {
    /// Probes `WebKit` SPI and prepares a configuration plan before any webview is built.
    ///
    /// # Errors
    ///
    /// Fails closed for unsupported `WebKit`, an invalid bundle, or invalid initialization data.
    pub fn configure(
        probe: &impl SpiProbe,
        bundle: impl AsRef<Path>,
        initialization: &[u8],
    ) -> Result<Self, ErrorReport> {
        let spi = MacosSpi::probe(probe)?;
        let artifact = MacosArtifact::inspect(bundle)?;
        let plan = WebViewPlan::new(spi, artifact, initialization, TransportTopology::direct())?;
        Ok(Self {
            state: Arc::new(Mutex::new(State {
                host: MacosHost::new(plan),
                webviews: HashMap::new(),
                callback_failure: None,
            })),
        })
    }

    /// Applies NWIPC's process-pool settings to an existing `WebKit` configuration.
    ///
    /// Existing public Wry builder options remain owned by Wry; this method only applies the
    /// injected-bundle and initialization settings required by the host plan.
    ///
    /// # Errors
    ///
    /// Propagates configuration failures before a webview is registered.
    pub fn configure_webview(
        &self,
        configurator: &mut impl WebViewConfigurator,
    ) -> Result<(), ErrorReport> {
        self.lock()?.host.plan().apply(configurator)
    }

    /// Maps a Wry webview ID to one active session generation.
    ///
    /// # Errors
    ///
    /// Rejects duplicate webview IDs and duplicate session identities.
    pub fn register(
        &self,
        webview: WebViewId,
        session: SessionId,
        generation: Generation,
    ) -> Result<(), ErrorReport> {
        let mut state = self.lock()?;
        if state.webviews.contains_key(&webview) {
            return Err(adapter_error(
                ErrorCode::InvalidStateTransition,
                "duplicate Wry webview",
            ));
        }
        state.host.register(session, generation)?;
        state.webviews.insert(
            webview,
            WebViewSession {
                session,
                generation,
            },
        );
        Ok(())
    }

    /// Maps a Wry webview to a session allocated by the public NWIPC facade.
    ///
    /// # Errors
    ///
    /// Rejects duplicate webview IDs and duplicate session identities.
    pub fn register_session(
        &self,
        webview: WebViewId,
        session: &nwipc::Session,
    ) -> Result<(), ErrorReport> {
        self.register(webview, session.id(), session.generation())
    }

    /// Routes Wry's successful page attachment event.
    ///
    /// # Errors
    ///
    /// Rejects unknown or stale webview events.
    pub fn renderer_attached(
        &self,
        webview: &WebViewId,
        generation: Generation,
    ) -> Result<(), ErrorReport> {
        let mut state = self.lock()?;
        let identity = state.identity(webview, generation)?;
        state
            .host
            .renderer_attached(identity.session, identity.generation)
    }

    /// Replaces the generation after Wry reports `WebContent` termination.
    ///
    /// # Errors
    ///
    /// Rejects unknown or stale webview events.
    pub fn renderer_terminated(
        &self,
        webview: &WebViewId,
        generation: Generation,
    ) -> Result<Generation, ErrorReport> {
        let mut state = self.lock()?;
        let identity = state.identity(webview, generation)?;
        state.replace(webview, identity)
    }

    /// Removes the Wry mapping and closes its host-side generation.
    ///
    /// # Errors
    ///
    /// Rejects unknown or stale webview events.
    pub fn remove(&self, webview: &WebViewId, generation: Generation) -> Result<(), ErrorReport> {
        let mut state = self.lock()?;
        let identity = state.identity(webview, generation)?;
        state
            .host
            .unregister(identity.session, identity.generation)?;
        state.webviews.remove(webview);
        Ok(())
    }

    /// Returns the current generation mapped to a Wry webview.
    pub fn generation(&self, webview: &WebViewId) -> Option<Generation> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .webviews
            .get(webview)
            .map(|identity| identity.generation)
    }

    /// Returns the last lifecycle failure captured by a callback that cannot return an error.
    pub fn take_callback_failure(&self) -> Option<ErrorReport> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .callback_failure
            .take()
    }

    fn lock(&self) -> Result<MutexGuard<'_, State>, ErrorReport> {
        self.state
            .lock()
            .map_err(|_| adapter_error(ErrorCode::Internal, "Wry adapter state"))
    }

    fn record_callback_failure(&self, error: ErrorReport) {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .callback_failure = Some(error);
    }

    fn renderer_terminated_current(&self, webview: &WebViewId) -> Result<Generation, ErrorReport> {
        let mut state = self.lock()?;
        let identity = state.current_identity(webview)?;
        state.replace(webview, identity)
    }
}

impl State {
    fn current_identity(&self, webview: &WebViewId) -> Result<WebViewSession, ErrorReport> {
        self.webviews
            .get(webview)
            .copied()
            .ok_or_else(|| adapter_error(ErrorCode::Closed, "unknown Wry webview"))
    }

    fn identity(
        &self,
        webview: &WebViewId,
        generation: Generation,
    ) -> Result<WebViewSession, ErrorReport> {
        let identity = self.current_identity(webview)?;
        if identity.generation != generation {
            return Err(adapter_error(
                ErrorCode::StaleGeneration,
                "stale Wry webview event",
            ));
        }
        Ok(identity)
    }

    fn replace(
        &mut self,
        webview: &WebViewId,
        identity: WebViewSession,
    ) -> Result<Generation, ErrorReport> {
        let replacement = self
            .host
            .replace_renderer(identity.session, identity.generation)?;
        self.webviews
            .get_mut(webview)
            .expect("identity exists")
            .generation = replacement;
        Ok(replacement)
    }
}

/// Bridge implemented by the application's native `WebKit` configuration wrapper.
///
/// The adapter first applies [`WebViewPlan`] through [`WebViewConfigurator`], then this bridge
/// merges the resulting native configuration into the existing Wry builder.
#[cfg(target_os = "macos")]
pub trait WryWebViewConfiguration<'a>: WebViewConfigurator {
    /// Merges the configured native object without replacing unrelated Wry builder options.
    fn merge(self, builder: wry::WebViewBuilder<'a>) -> wry::WebViewBuilder<'a>;
}

/// NWIPC extension for Wry's builder on macOS.
#[cfg(target_os = "macos")]
pub trait WryBuilderExt<'a>: Sized {
    /// Applies the host plan, registers identity routing, and installs `WebContent` replacement.
    ///
    /// # Errors
    ///
    /// Returns a configuration or duplicate-identity failure before the builder is returned.
    fn with_nwipc<Configuration: WryWebViewConfiguration<'a>>(
        self,
        adapter: &WryAdapter,
        webview: WebViewId,
        session: &nwipc::Session,
        configuration: Configuration,
    ) -> Result<Self, ErrorReport>;
}

#[cfg(target_os = "macos")]
impl<'a> WryBuilderExt<'a> for wry::WebViewBuilder<'a> {
    fn with_nwipc<Configuration: WryWebViewConfiguration<'a>>(
        self,
        adapter: &WryAdapter,
        webview: WebViewId,
        session: &nwipc::Session,
        mut configuration: Configuration,
    ) -> Result<Self, ErrorReport> {
        adapter.configure_webview(&mut configuration)?;
        adapter.register_session(webview.clone(), session)?;
        let callback_adapter = adapter.clone();
        let builder = configuration.merge(self);
        Ok(
            builder.with_on_web_content_process_terminate_handler(move || {
                if let Err(error) = callback_adapter.renderer_terminated_current(&webview) {
                    callback_adapter.record_callback_failure(error);
                }
            }),
        )
    }
}

fn adapter_error(code: ErrorCode, operation: &'static str) -> ErrorReport {
    ErrorReport::new(
        ErrorCategory::Lifecycle,
        code,
        Recoverability::ReplaceEndpoint,
        operation,
    )
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

    #[derive(Default)]
    struct Configuration(Vec<&'static str>);

    impl WebViewConfigurator for Configuration {
        fn set_injected_bundle(&mut self, _: &Path) -> Result<(), ErrorReport> {
            self.0.push("bundle");
            Ok(())
        }

        fn set_initialization_data(&mut self, _: &[u8]) -> Result<(), ErrorReport> {
            self.0.push("bootstrap");
            Ok(())
        }

        fn commit_process_pool_configuration(&mut self) -> Result<(), ErrorReport> {
            self.0.push("commit");
            Ok(())
        }
    }

    #[test]
    fn builder_configuration_and_lifecycle_preserve_identity() {
        let bundle = fixture_bundle();
        let adapter = WryAdapter::configure(&SupportedProbe, &bundle, b"plist").unwrap();
        let mut configuration = Configuration::default();
        adapter.configure_webview(&mut configuration).unwrap();
        assert_eq!(configuration.0, ["bundle", "bootstrap", "commit"]);

        let webview = WebViewId::new("main").unwrap();
        let session = SessionId::from_u128(1).unwrap();
        let generation = Generation::new(7).unwrap();
        adapter
            .register(webview.clone(), session, generation)
            .unwrap();
        adapter.renderer_attached(&webview, generation).unwrap();
        assert_eq!(
            adapter
                .renderer_terminated(&webview, generation)
                .unwrap()
                .get(),
            8
        );
        assert_eq!(adapter.generation(&webview).unwrap().get(), 8);
        assert_eq!(
            adapter
                .renderer_attached(&webview, generation)
                .unwrap_err()
                .code(),
            ErrorCode::StaleGeneration
        );
        let replacement = Generation::new(8).unwrap();
        adapter.renderer_attached(&webview, replacement).unwrap();
        adapter.remove(&webview, replacement).unwrap();
        assert!(adapter.generation(&webview).is_none());

        fs::remove_dir_all(bundle).unwrap();
    }

    #[test]
    fn rejects_duplicate_and_invalid_webview_ids() {
        assert!(WebViewId::new("").is_err());
        assert!(WebViewId::new("x".repeat(129)).is_err());

        let bundle = fixture_bundle();
        let adapter = WryAdapter::configure(&SupportedProbe, &bundle, b"plist").unwrap();
        let webview = WebViewId::new("main").unwrap();
        adapter
            .register(
                webview.clone(),
                SessionId::from_u128(1).unwrap(),
                Generation::new(1).unwrap(),
            )
            .unwrap();
        assert!(
            adapter
                .register(
                    webview,
                    SessionId::from_u128(2).unwrap(),
                    Generation::new(1).unwrap(),
                )
                .is_err()
        );
        fs::remove_dir_all(bundle).unwrap();
    }

    fn fixture_bundle() -> std::path::PathBuf {
        let bundle = std::env::temp_dir().join(format!(
            "nwipc-wry-test-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
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
        bundle
    }
}
