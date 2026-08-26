//! Shared controller for dynamic roots which become filesystem paths.
//!
//! Windows WSL distributions are the first provider. Discovery and activation
//! always run on the background executor; render reads the cached process-level
//! store only. macOS/Linux providers return an empty list, which also proves
//! that adding the capability creates no placeholder UI on those platforms.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use ferail_core::platform_locations::{
    PathBackedPlatformRoot, PlatformActivation, PlatformDiscovery, PlatformRootErrorKind,
    PlatformRootId, PlatformRootsStore,
};
use gpui::{App, Context, Window};
use gpui_component::WindowExt as _;
use gpui_component::notification::Notification;

use crate::shell::Shell;

pub(crate) trait PathBackedPlatformRootProvider: Send + Sync {
    fn discover(&self, cancel: &AtomicBool) -> Vec<PathBackedPlatformRoot>;
    fn activate(
        &self,
        id: &PlatformRootId,
        cancel: &AtomicBool,
    ) -> Result<PathBuf, PlatformRootErrorKind>;
}

struct SystemProvider;

impl PathBackedPlatformRootProvider for SystemProvider {
    fn discover(&self, cancel: &AtomicBool) -> Vec<PathBackedPlatformRoot> {
        crate::platform_shell::discover_path_backed_platform_roots(cancel)
    }

    fn activate(
        &self,
        id: &PlatformRootId,
        cancel: &AtomicBool,
    ) -> Result<PathBuf, PlatformRootErrorKind> {
        crate::platform_shell::activate_path_backed_platform_root(id, cancel)
    }
}

/// Process-level controller around the platform-neutral store. The atomics are
/// worker cancellation handles only; no worker owns this UI-thread structure.
#[derive(Default)]
pub struct PlatformLocations {
    store: PlatformRootsStore,
    discovery_cancel: Option<Arc<AtomicBool>>,
    activation_cancels: HashMap<PlatformRootId, (PlatformActivation, Arc<AtomicBool>)>,
}

impl PlatformLocations {
    pub fn roots(&self) -> &[PathBackedPlatformRoot] {
        self.store.roots()
    }

    fn begin_discovery(&mut self) -> (PlatformDiscovery, Arc<AtomicBool>) {
        if let Some(cancel) = self.discovery_cancel.take() {
            cancel.store(true, Ordering::Relaxed);
        }
        for (_, cancel) in self
            .activation_cancels
            .drain()
            .map(|(_, activation)| activation)
        {
            cancel.store(true, Ordering::Relaxed);
        }
        let cancel = Arc::new(AtomicBool::new(false));
        self.discovery_cancel = Some(cancel.clone());
        (self.store.begin_discovery(), cancel)
    }

    fn apply_discovery(
        &mut self,
        token: PlatformDiscovery,
        roots: Vec<PathBackedPlatformRoot>,
    ) -> bool {
        let applied = self.store.apply_discovery(token, roots);
        if applied {
            self.discovery_cancel = None;
        }
        applied
    }

    fn begin_activation(
        &mut self,
        id: &PlatformRootId,
    ) -> Option<(PlatformActivation, Arc<AtomicBool>)> {
        // An explicit click wins over a background refresh of the cached
        // roots. Cancel and invalidate that discovery before minting the
        // activation token, otherwise its late snapshot could silently clear
        // a just-started activation for the same root.
        if let Some(cancel) = self.discovery_cancel.take() {
            cancel.store(true, Ordering::Relaxed);
            let _ = self.store.begin_discovery();
        }
        let token = self.store.begin_activation(id)?;
        let cancel = Arc::new(AtomicBool::new(false));
        self.activation_cancels
            .insert(id.clone(), (token.clone(), cancel.clone()));
        Some((token, cancel))
    }

    fn finish_activation(
        &mut self,
        token: &PlatformActivation,
        result: Result<PathBuf, PlatformRootErrorKind>,
    ) -> Option<PathBuf> {
        if self
            .activation_cancels
            .get(token.root())
            .is_some_and(|(current, _)| current == token)
        {
            self.activation_cancels.remove(token.root());
        }
        self.store.finish_activation(token, result)
    }
}

/// Initial/refresh discovery. Safe to call repeatedly: the previous pass and
/// any activation it supersedes are cancelled and stale results are rejected.
pub fn refresh(cx: &mut App) {
    let process = crate::process_state::process_state(cx);
    let (token, cancel) = process.platform_locations.borrow_mut().begin_discovery();
    cx.spawn(async move |cx| {
        let worker_cancel = cancel.clone();
        let roots = cx
            .background_executor()
            .spawn(async move { SystemProvider.discover(&worker_cancel) })
            .await;
        cx.update(|cx| {
            let process = crate::process_state::process_state(cx);
            if process
                .platform_locations
                .borrow_mut()
                .apply_discovery(token, roots)
            {
                notify_shells(&process, cx);
            }
        });
    })
    .detach();
}

fn notify_shells(process: &crate::process_state::ProcessState, cx: &mut App) {
    for weak in process.live_shells() {
        if let Some(shell) = weak.upgrade() {
            shell.update(cx, |_shell, cx| cx.notify());
        }
    }
}

fn failure_message(kind: PlatformRootErrorKind) -> String {
    match kind {
        PlatformRootErrorKind::Unavailable => {
            tr!("This Linux distribution is unavailable.").to_string()
        }
        PlatformRootErrorKind::NotFound => {
            tr!("This Linux distribution is no longer installed.").to_string()
        }
        PlatformRootErrorKind::TimedOut => {
            tr!("Starting the Linux distribution timed out.").to_string()
        }
        PlatformRootErrorKind::Cancelled => {
            tr!("Starting the Linux distribution was cancelled.").to_string()
        }
        PlatformRootErrorKind::Failed => {
            tr!("The Linux distribution could not be started.").to_string()
        }
    }
}

impl Shell {
    /// Activate or navigate to one cached dynamic root. Ready roots take the
    /// ordinary path immediately; stopped roots enter a coalesced worker job.
    pub fn open_path_backed_platform_root(
        &mut self,
        id: PlatformRootId,
        open_in_new_tab: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let ready = self
            .process
            .platform_locations
            .borrow()
            .store
            .ready_path(&id)
            .cloned();
        if let Some(path) = ready {
            if open_in_new_tab {
                self.open_path_in_new_tab(path, window, cx);
            } else {
                self.navigate(path, cx);
            }
            return;
        }

        let Some((token, cancel)) = self
            .process
            .platform_locations
            .borrow_mut()
            .begin_activation(&id)
        else {
            return;
        };
        let origin_tab = self.active_tab().id;
        let origin_generation = self.active_tab().load_generation;
        self.active_tab_mut().platform_root_activation_cancel = Some(cancel.clone());
        let shell = cx.weak_entity();
        let win = window.window_handle();
        // This method is entered from `Shell::update`, so notifying every
        // registered shell here would try to update this same entity again.
        // GPUI rejects that nested mutable lease (and the panic crosses the
        // Windows callback boundary, where it must abort). Repaint the current
        // shell directly; the completion path below runs from `App::update`
        // and can safely fan the final state out to every window.
        cx.notify();

        cx.spawn(async move |_this, cx| {
            let worker_id = id.clone();
            let worker_cancel = cancel.clone();
            let result = cx
                .background_executor()
                .spawn(async move { SystemProvider.activate(&worker_id, &worker_cancel) })
                .await;
            let result_for_state = result.clone();
            let ready = cx.update(|cx| {
                let process = crate::process_state::process_state(cx);
                let ready = process
                    .platform_locations
                    .borrow_mut()
                    .finish_activation(&token, result_for_state);
                notify_shells(&process, cx);
                ready
            });
            let _ = win.update(cx, |_, window, cx| {
                let Some(shell) = shell.upgrade() else { return };
                let owns_request = shell.update(cx, |this, _cx| {
                    let Some(index) = this.tabs.iter().position(|tab| tab.id == origin_tab) else {
                        return false;
                    };
                    let tab = &mut this.tabs[index];
                    let owns = tab
                        .platform_root_activation_cancel
                        .as_ref()
                        .is_some_and(|current| Arc::ptr_eq(current, &cancel));
                    if owns {
                        tab.platform_root_activation_cancel = None;
                    }
                    owns && index == this.active
                        && tab.load_generation == origin_generation
                        && !cancel.load(Ordering::Relaxed)
                });
                if !owns_request {
                    return;
                }
                match result {
                    Err(PlatformRootErrorKind::Cancelled) => {}
                    Err(kind) => {
                        window.push_notification(Notification::error(failure_message(kind)), cx);
                    }
                    Ok(_) => {
                        let Some(path) = ready else { return };
                        shell.update(cx, |this, cx| {
                            if open_in_new_tab {
                                this.open_path_in_new_tab(path, window, cx);
                            } else {
                                this.navigate(path, cx);
                            }
                        });
                    }
                }
            });
        })
        .detach();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ferail_core::platform_locations::PathBackedRootState;
    use std::sync::atomic::AtomicUsize;

    struct FakeProvider {
        discoveries: AtomicUsize,
    }

    impl PathBackedPlatformRootProvider for FakeProvider {
        fn discover(&self, _cancel: &AtomicBool) -> Vec<PathBackedPlatformRoot> {
            self.discoveries.fetch_add(1, Ordering::Relaxed);
            vec![PathBackedPlatformRoot::stopped(
                PlatformRootId::new("opaque"),
                "Ubuntu",
            )]
        }

        fn activate(
            &self,
            _id: &PlatformRootId,
            cancel: &AtomicBool,
        ) -> Result<PathBuf, PlatformRootErrorKind> {
            if cancel.load(Ordering::Relaxed) {
                Err(PlatformRootErrorKind::Cancelled)
            } else {
                Ok(PathBuf::from(r"\\wsl.localhost\Ubuntu"))
            }
        }
    }

    #[test]
    fn fake_provider_drives_stopped_starting_ready_contract() {
        let provider = FakeProvider {
            discoveries: AtomicUsize::new(0),
        };
        let mut model = PlatformLocations::default();
        let (discovery, cancel) = model.begin_discovery();
        assert!(model.apply_discovery(discovery, provider.discover(&cancel)));
        assert_eq!(provider.discoveries.load(Ordering::Relaxed), 1);

        let id = PlatformRootId::new("opaque");
        let (activation, cancel) = model.begin_activation(&id).unwrap();
        assert!(matches!(
            model.roots()[0].state,
            PathBackedRootState::Starting
        ));
        let ready = model.finish_activation(&activation, provider.activate(&id, &cancel));
        assert_eq!(ready, Some(PathBuf::from(r"\\wsl.localhost\Ubuntu")));
        assert!(matches!(
            model.roots()[0].state,
            PathBackedRootState::Ready(_)
        ));
    }

    #[test]
    fn refresh_cancels_inflight_activation() {
        let mut model = PlatformLocations::default();
        let (discovery, _) = model.begin_discovery();
        let id = PlatformRootId::new("opaque");
        model.apply_discovery(
            discovery,
            vec![PathBackedPlatformRoot::stopped(id.clone(), "Ubuntu")],
        );
        let (_activation, cancel) = model.begin_activation(&id).unwrap();
        let _ = model.begin_discovery();
        assert!(cancel.load(Ordering::Relaxed));
    }

    #[test]
    fn stale_completion_cannot_drop_new_activation_cancel_handle() {
        let mut model = PlatformLocations::default();
        let id = PlatformRootId::new("opaque");

        let (first_discovery, _) = model.begin_discovery();
        model.apply_discovery(
            first_discovery,
            vec![PathBackedPlatformRoot::stopped(id.clone(), "Ubuntu")],
        );
        let (stale_activation, stale_cancel) = model.begin_activation(&id).unwrap();

        let (second_discovery, _) = model.begin_discovery();
        model.apply_discovery(
            second_discovery,
            vec![PathBackedPlatformRoot::stopped(id.clone(), "Ubuntu")],
        );
        let (current_activation, current_cancel) = model.begin_activation(&id).unwrap();

        assert!(stale_cancel.load(Ordering::Relaxed));
        assert_eq!(
            model.finish_activation(&stale_activation, Err(PlatformRootErrorKind::Cancelled)),
            None
        );
        let stored = model.activation_cancels.get(&id).unwrap();
        assert_eq!(stored.0, current_activation);
        assert!(Arc::ptr_eq(&stored.1, &current_cancel));
    }

    #[test]
    fn explicit_activation_cancels_and_invalidates_inflight_discovery() {
        let mut model = PlatformLocations::default();
        let id = PlatformRootId::new("opaque");
        let (initial, _) = model.begin_discovery();
        model.apply_discovery(
            initial,
            vec![PathBackedPlatformRoot::stopped(id.clone(), "Ubuntu")],
        );

        let (refresh, refresh_cancel) = model.begin_discovery();
        let (activation, _) = model.begin_activation(&id).unwrap();
        assert!(refresh_cancel.load(Ordering::Relaxed));
        assert!(!model.apply_discovery(refresh, Vec::new()));
        assert_eq!(
            model.finish_activation(&activation, Ok(PathBuf::from(r"\\wsl.localhost\Ubuntu"))),
            Some(PathBuf::from(r"\\wsl.localhost\Ubuntu"))
        );
    }
}
