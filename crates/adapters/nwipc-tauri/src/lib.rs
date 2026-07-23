//! Tauri plugin and lifecycle integration for NWIPC's macOS `WebKit` control plane.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard};

use nwipc_capabilities::TransportTopology;
use nwipc_error::{ErrorCategory, ErrorCode, ErrorReport, Recoverability};
use nwipc_macos_artifact::MacosArtifact;
use nwipc_macos_host::{MacosHost, WebViewConfigurator, WebViewPlan};
use nwipc_macos_spi::{MacosSpi, SpiProbe};
use nwipc_types::{Generation, SessionId};
use serde::Serialize;
use tauri::plugin::{Builder as PluginBuilder, TauriPlugin};
use tauri::{Manager, RunEvent, Runtime, State as TauriState, WindowEvent};

/// Application decision when NWIPC is unavailable during startup.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FallbackDecision {
    /// Abort startup and return the structured NWIPC failure.
    Abort,
    /// Continue without registering the NWIPC plugin.
    ContinueWithoutNwipc,
}

/// Application-owned fallback policy. The adapter never selects or starts a fallback itself.
pub trait FallbackPolicy {
    /// Decides how the application handles one fail-closed startup error.
    fn on_unavailable(&self, error: &ErrorReport) -> FallbackDecision;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct WindowSession {
    session: SessionId,
    generation: Generation,
}

struct State {
    host: MacosHost,
    windows: HashMap<String, WindowSession>,
    callback_failure: Option<ErrorReport>,
}

/// Cloneable Tauri plugin state. It does not impose a process-global singleton.
#[derive(Clone)]
pub struct TauriAdapter {
    state: Arc<Mutex<State>>,
}

impl TauriAdapter {
    /// Probes `WebKit` SPI and prepares the host plan before Tauri creates a webview.
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
                windows: HashMap::new(),
                callback_failure: None,
            })),
        })
    }

    /// Applies application fallback policy without silently changing transport semantics.
    ///
    /// # Errors
    ///
    /// Returns the original structured failure when the policy chooses [`FallbackDecision::Abort`].
    pub fn configure_with_fallback(
        probe: &impl SpiProbe,
        bundle: impl AsRef<Path>,
        initialization: &[u8],
        policy: &impl FallbackPolicy,
    ) -> Result<Option<Self>, ErrorReport> {
        match Self::configure(probe, bundle, initialization) {
            Ok(adapter) => Ok(Some(adapter)),
            Err(error) => match policy.on_unavailable(&error) {
                FallbackDecision::Abort => Err(error),
                FallbackDecision::ContinueWithoutNwipc => Ok(None),
            },
        }
    }

    /// Applies NWIPC's process-pool settings to Tauri's native `WebKit` configuration.
    ///
    /// # Errors
    ///
    /// Propagates configuration failures before the webview or window is registered.
    pub fn configure_webview(
        &self,
        configurator: &mut impl WebViewConfigurator,
    ) -> Result<(), ErrorReport> {
        self.lock()?.host.plan().apply(configurator)
    }

    /// Maps a Tauri window label to one active session generation.
    ///
    /// # Errors
    ///
    /// Rejects invalid labels, duplicate labels, and duplicate session identities.
    pub fn register_window(
        &self,
        label: impl Into<String>,
        session: SessionId,
        generation: Generation,
    ) -> Result<(), ErrorReport> {
        let label = label.into();
        validate_label(&label)?;
        let mut state = self.lock()?;
        if state.windows.contains_key(&label) {
            return Err(adapter_error(
                ErrorCode::InvalidStateTransition,
                "duplicate Tauri window",
            ));
        }
        state.host.register(session, generation)?;
        state.windows.insert(
            label,
            WindowSession {
                session,
                generation,
            },
        );
        Ok(())
    }

    /// Maps a Tauri label to a session allocated by the public NWIPC facade.
    ///
    /// # Errors
    ///
    /// Rejects invalid or duplicate labels and duplicate session identities.
    pub fn register_session(
        &self,
        label: impl Into<String>,
        session: &nwipc::Session,
    ) -> Result<(), ErrorReport> {
        self.register_window(label, session.id(), session.generation())
    }

    /// Marks a window's injected-bundle renderer attached.
    ///
    /// # Errors
    ///
    /// Rejects unknown, stale, and duplicate attachment events.
    pub fn renderer_attached(
        &self,
        label: &str,
        generation: Generation,
    ) -> Result<(), ErrorReport> {
        let mut state = self.lock()?;
        let identity = state.identity(label, generation)?;
        state
            .host
            .renderer_attached(identity.session, identity.generation)
    }

    /// Replaces a Tauri window's resource generation after `WebContent` termination or reload.
    ///
    /// # Errors
    ///
    /// Rejects unknown and stale lifecycle events.
    pub fn renderer_replaced(
        &self,
        label: &str,
        generation: Generation,
    ) -> Result<Generation, ErrorReport> {
        let mut state = self.lock()?;
        let identity = state.identity(label, generation)?;
        state.replace(label, identity)
    }

    /// Cleans up one window label and its active generation.
    ///
    /// # Errors
    ///
    /// Rejects unknown and stale lifecycle events.
    pub fn remove_window(&self, label: &str, generation: Generation) -> Result<(), ErrorReport> {
        let mut state = self.lock()?;
        let identity = state.identity(label, generation)?;
        state
            .host
            .unregister(identity.session, identity.generation)?;
        state.windows.remove(label);
        Ok(())
    }

    /// Returns redacted plugin diagnostics suitable for a Tauri command response.
    pub fn diagnostics(&self) -> Vec<WindowDiagnostics> {
        let state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut diagnostics: Vec<_> = state
            .windows
            .iter()
            .map(|(label, identity)| WindowDiagnostics {
                label: label.clone(),
                generation: identity.generation.get(),
            })
            .collect();
        diagnostics.sort_by(|left, right| left.label.cmp(&right.label));
        diagnostics
    }

    /// Returns the last lifecycle failure captured by a Tauri callback.
    pub fn take_callback_failure(&self) -> Option<ErrorReport> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .callback_failure
            .take()
    }

    /// Builds the Tauri plugin with cleanup and a diagnostics-only command.
    pub fn plugin<R: Runtime>(&self) -> TauriPlugin<R> {
        let managed = ManagedAdapter(self.clone());
        let window_cleanup = self.clone();
        let cleanup = self.clone();
        PluginBuilder::new("nwipc")
            .setup(move |app, _| {
                app.manage(managed.clone());
                Ok(())
            })
            .invoke_handler(tauri::generate_handler![diagnostics_command])
            .on_event(move |_, event| {
                if let RunEvent::WindowEvent {
                    label,
                    event: WindowEvent::Destroyed,
                    ..
                } = event
                {
                    window_cleanup.remove_window_current(label);
                }
            })
            .on_drop(move |_| cleanup.cleanup_all())
            .build()
    }

    fn lock(&self) -> Result<MutexGuard<'_, State>, ErrorReport> {
        self.state
            .lock()
            .map_err(|_| adapter_error(ErrorCode::Internal, "Tauri adapter state"))
    }

    fn cleanup_all(&self) {
        let labels: Vec<_> = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .windows
            .keys()
            .cloned()
            .collect();
        for label in labels {
            self.remove_window_current(&label);
        }
    }

    fn remove_window_current(&self, label: &str) {
        let result = self.lock().and_then(|mut state| {
            let identity = state.current_identity(label)?;
            state
                .host
                .unregister(identity.session, identity.generation)?;
            state.windows.remove(label);
            Ok(())
        });
        if let Err(error) = result {
            if error.code() != ErrorCode::Closed {
                self.record_callback_failure(error);
            }
        }
    }

    fn record_callback_failure(&self, error: ErrorReport) {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .callback_failure = Some(error);
    }
}

impl State {
    fn current_identity(&self, label: &str) -> Result<WindowSession, ErrorReport> {
        self.windows
            .get(label)
            .copied()
            .ok_or_else(|| adapter_error(ErrorCode::Closed, "unknown Tauri window"))
    }

    fn identity(&self, label: &str, generation: Generation) -> Result<WindowSession, ErrorReport> {
        let identity = self.current_identity(label)?;
        if identity.generation != generation {
            return Err(adapter_error(
                ErrorCode::StaleGeneration,
                "stale Tauri window event",
            ));
        }
        Ok(identity)
    }

    fn replace(&mut self, label: &str, identity: WindowSession) -> Result<Generation, ErrorReport> {
        let replacement = self
            .host
            .replace_renderer(identity.session, identity.generation)?;
        self.windows
            .get_mut(label)
            .expect("identity exists")
            .generation = replacement;
        Ok(replacement)
    }
}

/// Redacted diagnostics returned by the plugin command.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WindowDiagnostics {
    /// Tauri window label.
    pub label: String,
    /// Current generation; session identity and provider handles remain hidden.
    pub generation: u64,
}

#[derive(Clone)]
struct ManagedAdapter(TauriAdapter);

#[tauri::command]
#[allow(clippy::needless_pass_by_value)]
fn diagnostics_command(adapter: TauriState<'_, ManagedAdapter>) -> Vec<WindowDiagnostics> {
    adapter.0.diagnostics()
}

/// Builder extension that installs one explicitly configured NWIPC plugin instance.
pub trait TauriBuilderExt<R: Runtime>: Sized {
    /// Registers NWIPC without changing the application's invoke handler or spawning a peer.
    #[must_use]
    fn plugin_nwipc(self, adapter: &TauriAdapter) -> Self;
}

impl<R: Runtime> TauriBuilderExt<R> for tauri::Builder<R> {
    fn plugin_nwipc(self, adapter: &TauriAdapter) -> Self {
        self.plugin(adapter.plugin())
    }
}

/// Bridge implemented by the application's native Tauri `WebKit` configuration wrapper.
///
/// NWIPC applies its host plan before the wrapper merges the native configuration into the
/// existing Tauri webview-window builder.
#[cfg(target_os = "macos")]
pub trait TauriWebViewConfiguration<'a, R: Runtime, M: Manager<R>>: WebViewConfigurator {
    /// Merges the configured native object without replacing unrelated Tauri builder options.
    fn merge(
        self,
        builder: tauri::WebviewWindowBuilder<'a, R, M>,
    ) -> tauri::WebviewWindowBuilder<'a, R, M>;
}

/// NWIPC extension for a Tauri webview-window builder on macOS.
#[cfg(target_os = "macos")]
pub trait TauriWebviewWindowBuilderExt<'a, R: Runtime, M: Manager<R>>: Sized {
    /// Applies `WebKit` configuration and registers the label before returning the builder.
    ///
    /// # Errors
    ///
    /// Returns a configuration, label, or duplicate-session failure.
    fn with_nwipc<Configuration: TauriWebViewConfiguration<'a, R, M>>(
        self,
        adapter: &TauriAdapter,
        label: impl Into<String>,
        session: &nwipc::Session,
        configuration: Configuration,
    ) -> Result<Self, ErrorReport>;
}

#[cfg(target_os = "macos")]
impl<'a, R: Runtime, M: Manager<R>> TauriWebviewWindowBuilderExt<'a, R, M>
    for tauri::WebviewWindowBuilder<'a, R, M>
{
    fn with_nwipc<Configuration: TauriWebViewConfiguration<'a, R, M>>(
        self,
        adapter: &TauriAdapter,
        label: impl Into<String>,
        session: &nwipc::Session,
        mut configuration: Configuration,
    ) -> Result<Self, ErrorReport> {
        adapter.configure_webview(&mut configuration)?;
        adapter.register_session(label, session)?;
        Ok(configuration.merge(self))
    }
}

fn validate_label(label: &str) -> Result<(), ErrorReport> {
    if label.is_empty() || label.len() > 128 {
        return Err(adapter_error(ErrorCode::InvalidRange, "Tauri window label"));
    }
    Ok(())
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

    struct UnsupportedProbe;

    impl SpiProbe for UnsupportedProbe {
        fn os_version(&self) -> Option<OsVersion> {
            None
        }

        fn has_entry(&self, _: SpiEntry) -> bool {
            false
        }
    }

    struct Continue;

    impl FallbackPolicy for Continue {
        fn on_unavailable(&self, _: &ErrorReport) -> FallbackDecision {
            FallbackDecision::ContinueWithoutNwipc
        }
    }

    #[test]
    fn maps_labels_replaces_generations_and_cleans_up() {
        let bundle = fixture_bundle();
        let adapter = TauriAdapter::configure(&SupportedProbe, &bundle, b"plist").unwrap();
        let session = SessionId::from_u128(1).unwrap();
        let generation = Generation::new(3).unwrap();
        adapter
            .register_window("main", session, generation)
            .unwrap();
        adapter.renderer_attached("main", generation).unwrap();
        assert_eq!(
            adapter.renderer_replaced("main", generation).unwrap().get(),
            4
        );
        assert_eq!(
            adapter
                .renderer_attached("main", generation)
                .unwrap_err()
                .code(),
            ErrorCode::StaleGeneration
        );
        assert_eq!(
            adapter.diagnostics(),
            [WindowDiagnostics {
                label: "main".into(),
                generation: 4,
            }]
        );
        adapter
            .remove_window("main", Generation::new(4).unwrap())
            .unwrap();
        assert!(adapter.diagnostics().is_empty());
        fs::remove_dir_all(bundle).unwrap();
    }

    #[test]
    fn fallback_is_explicit_and_labels_are_bounded() {
        assert!(
            TauriAdapter::configure_with_fallback(
                &UnsupportedProbe,
                "missing",
                b"plist",
                &Continue
            )
            .unwrap()
            .is_none()
        );

        let bundle = fixture_bundle();
        let adapter = TauriAdapter::configure(&SupportedProbe, &bundle, b"plist").unwrap();
        assert!(
            adapter
                .register_window(
                    "",
                    SessionId::from_u128(1).unwrap(),
                    Generation::new(1).unwrap(),
                )
                .is_err()
        );
        fs::remove_dir_all(bundle).unwrap();
    }

    fn fixture_bundle() -> std::path::PathBuf {
        let bundle = std::env::temp_dir().join(format!(
            "nwipc-tauri-test-{}-{:?}",
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
