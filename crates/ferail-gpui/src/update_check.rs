//! Update check — "is there a newer Ferail on GitHub Releases?"
//! (docs/features/UPDATES.md)
//!
//! Three surfaces, one state machine:
//!
//!  - **Manual**: Ferail ▸ Check for Updates… (`app.check_updates`) opens
//!    the Software Update dialog and starts a check. Always available —
//!    the setting below gates only the automatic path.
//!  - **Automatic**: an opt-in daily background check
//!    (Settings ▸ About ▸ Updates, **off by default**). When it finds a
//!    newer release it posts one notification per version per session;
//!    up-to-date / failed checks stay silent. Nothing downloads on its
//!    own, ever.
//!  - **Update**: from the dialog, Download fetches the platform's asset
//!    (macOS `.dmg`, Windows win zip, Linux `.deb` for the running arch)
//!    into ~/Downloads, then offers Open / Show in Folder. Installing
//!    stays a user step — Ferail doesn't replace its own binary.
//!
//! Network: gpui's `cx.http_client()` (a real `ReqwestClient` installed at
//! boot; gpui's own default is a `NullHttpClient` that errors). The check
//! reuses zed's `http_client::github::latest_github_release`, which walks
//! `/repos/{repo}/releases` and picks the newest non-prerelease with
//! assets. This module's only requests are that one API call and the
//! user-initiated asset download — there is no telemetry channel here; an
//! update check necessarily tells GitHub an app instance asked, which is
//! why the automatic path ships opt-in.
//!
//! Prime Directive: every HTTP call and every filesystem touch (Downloads
//! probe, `.part` write, rename) runs on the background executor; results
//! come back over an `async_channel` and land in the [`UpdateState`]
//! global on the foreground executor. The dialog is a gpui-component
//! `Dialog` whose builder re-reads the global each frame, so state
//! changes (checking → found → downloading → done) animate live without
//! bespoke plumbing.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use gpui::http_client::github::{GithubRelease, GithubReleaseAsset, latest_github_release};
use gpui::http_client::{AsyncBody, HttpClient, HttpRequestExt as _, RedirectPolicy, http};
use gpui::{
    App, AppContext as _, ClickEvent, ElementId, Global, InteractiveElement as _, IntoElement,
    ParentElement as _, StatefulInteractiveElement as _, Styled as _, div, px,
};
use gpui_component::{
    ActiveTheme as _, Sizable as _, WindowExt as _,
    button::{Button, ButtonVariants as _},
    dialog::{Dialog, DialogFooter},
    h_flex,
    notification::Notification,
    v_flex,
};

use crate::text::TextScale as _;

/// Repository whose Releases page is the source of truth.
const REPO: &str = "jonx/Ferail";
/// Seconds after startup before the automatic path's first check — keep
/// the boot path free of network traffic while windows are still opening.
const AUTO_FIRST_DELAY: Duration = Duration::from_secs(20);
/// Cadence between automatic checks while the app stays running.
const AUTO_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);

// ============================================================================
// State
// ============================================================================

#[derive(Clone, Debug, Default, PartialEq)]
pub enum CheckStatus {
    /// Never checked this session.
    #[default]
    Idle,
    Checking,
    /// The latest non-prerelease is not newer than the running build.
    UpToDate {
        latest: String,
    },
    Available(ReleaseInfo),
    Failed(String),
}

#[derive(Clone, Debug, PartialEq)]
pub struct ReleaseInfo {
    /// Normalized "0.5.0" (tag minus the leading `v`).
    pub version: String,
    /// The literal tag, for the release-page URL.
    pub tag: String,
    /// This platform's downloadable asset, when the release carries one.
    pub asset: Option<AssetInfo>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct AssetInfo {
    pub name: String,
    pub url: String,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub enum DownloadStatus {
    #[default]
    None,
    /// Fraction in 0..=1, or `None` while the total is unknown.
    InProgress(Option<f32>),
    Done(PathBuf),
    Failed(String),
}

/// Process-wide update-check state. All mutation happens on the
/// foreground executor; the dialog and settings read it per frame.
#[derive(Clone, Default)]
struct UpdateState {
    status: CheckStatus,
    download: DownloadStatus,
    /// Version the automatic path already announced this session, so a
    /// daily re-check doesn't re-toast the same release.
    notified: Option<String>,
    /// Dialog singleton guard (same rationale as About's).
    dialog_open: bool,
}
impl Global for UpdateState {}

fn snapshot(cx: &App) -> UpdateState {
    cx.try_global::<UpdateState>().cloned().unwrap_or_default()
}

fn mutate(cx: &mut App, f: impl FnOnce(&mut UpdateState)) {
    let mut st = snapshot(cx);
    f(&mut st);
    cx.set_global(st);
}

/// Is the automatic daily check enabled? Off unless the user opted in
/// (Settings ▸ About ▸ Updates).
pub fn auto_check_enabled() -> bool {
    crate::app_state::load().update_check.unwrap_or(false)
}

// ============================================================================
// Version + asset selection (pure; unit-tested)
// ============================================================================

/// Parse "1.2.3" / "v1.2.3" into a comparable triple. Anything that
/// isn't three dot-separated integers (pre-release suffixes, garbage)
/// returns None and is treated as not-newer — a malformed remote tag
/// must never produce an update prompt.
fn parse_version(s: &str) -> Option<(u64, u64, u64)> {
    let s = s.trim().trim_start_matches('v');
    let mut it = s.split('.');
    let maj = it.next()?.parse().ok()?;
    let min = it.next()?.parse().ok()?;
    let pat = it.next()?.parse().ok()?;
    if it.next().is_some() {
        return None;
    }
    Some((maj, min, pat))
}

fn is_newer_than_current(remote_tag: &str) -> bool {
    match (
        parse_version(remote_tag),
        parse_version(env!("CARGO_PKG_VERSION")),
    ) {
        (Some(r), Some(c)) => r > c,
        _ => false,
    }
}

/// Debian architecture string for the running build, matching the
/// cargo-deb asset names CI publishes.
fn deb_arch() -> &'static str {
    match std::env::consts::ARCH {
        "x86_64" => "amd64",
        "aarch64" => "arm64",
        other => other,
    }
}

/// Index of the asset a user on this platform should download, from the
/// release's asset names. Mirrors what CI publishes per release:
/// `Ferail-<v>.dmg`, `Ferail-<v>-win-x64.zip`, `ferail_<v>-1_<arch>.deb`.
fn pick_asset_index(names: &[&str]) -> Option<usize> {
    let wanted: Box<dyn Fn(&str) -> bool> = match std::env::consts::OS {
        "macos" => Box::new(|n: &str| n.ends_with(".dmg")),
        "windows" => Box::new(|n: &str| n.ends_with(".zip") && n.contains("win")),
        "linux" => {
            let suffix = format!("_{}.deb", deb_arch());
            Box::new(move |n: &str| n.ends_with(&suffix))
        }
        _ => return None,
    };
    names.iter().position(|n| wanted(n))
}

fn release_info(release: &GithubRelease) -> ReleaseInfo {
    let names: Vec<&str> = release.assets.iter().map(|a| a.name.as_str()).collect();
    let asset = pick_asset_index(&names).map(|ix| {
        let a: &GithubReleaseAsset = &release.assets[ix];
        AssetInfo {
            name: a.name.clone(),
            url: a.browser_download_url.clone(),
        }
    });
    ReleaseInfo {
        version: release.tag_name.trim_start_matches('v').to_string(),
        tag: release.tag_name.clone(),
        asset,
    }
}

fn tag_url(tag: &str) -> String {
    format!("https://github.com/{REPO}/releases/tag/{tag}")
}

// ============================================================================
// Checking
// ============================================================================

/// Menu entry point: open the dialog and refresh the state behind it.
/// A check already in flight, or a download in progress / completed, is
/// left alone — reopening the dialog must not clobber a 90%-done
/// download with a fresh "Checking…".
pub fn manual_check(cx: &mut App) {
    open_update_dialog(cx);
    let st = snapshot(cx);
    let busy = st.status == CheckStatus::Checking
        || !matches!(
            st.download,
            DownloadStatus::None | DownloadStatus::Failed(_)
        );
    if !busy {
        start_check(true, cx);
    }
}

/// The automatic path: kicked from boot (skipped in safe mode), then
/// daily. Re-reads the setting every wake so the Settings toggle takes
/// effect without a relaunch.
pub fn start_auto_loop(cx: &mut App) {
    cx.spawn(async move |cx| {
        cx.background_executor().timer(AUTO_FIRST_DELAY).await;
        loop {
            if auto_check_enabled() {
                cx.update(|cx| {
                    // Don't fight a manual check/download the user is
                    // looking at right now.
                    let st = snapshot(cx);
                    if st.status != CheckStatus::Checking
                        && matches!(st.download, DownloadStatus::None)
                    {
                        start_check(false, cx);
                    }
                });
            }
            cx.background_executor().timer(AUTO_INTERVAL).await;
        }
    })
    .detach();
}

/// Settings entry point: the user just opted in to automatic checks —
/// answer now instead of at tomorrow's daily wake. Auto-style surfacing
/// (a notification only if something newer exists).
pub fn start_check_background(cx: &mut App) {
    let st = snapshot(cx);
    if st.status != CheckStatus::Checking && matches!(st.download, DownloadStatus::None) {
        start_check(false, cx);
    }
}

/// Fire one release-list request and fold the answer into the global.
/// `manual` only affects surfacing: manual results land in the (open)
/// dialog, automatic ones notify — and only for a new version.
fn start_check(manual: bool, cx: &mut App) {
    mutate(cx, |st| {
        st.status = CheckStatus::Checking;
        st.download = DownloadStatus::None;
    });
    let client = cx.http_client();
    cx.spawn(async move |cx| {
        let result = cx
            .background_executor()
            .spawn(async move { latest_github_release(REPO, true, false, client).await })
            .await;
        cx.update(|cx| {
            let status = match &result {
                Ok(release) if is_newer_than_current(&release.tag_name) => {
                    CheckStatus::Available(release_info(release))
                }
                Ok(release) => CheckStatus::UpToDate {
                    latest: release.tag_name.trim_start_matches('v').to_string(),
                },
                Err(e) => {
                    crate::log_warn!(90, "update check failed: {e:#}");
                    // First line only: the anyhow chain can quote a whole
                    // HTML error page and the dialog is 430px wide.
                    let brief = format!("{e:#}");
                    let brief = brief.lines().next().unwrap_or("request failed").to_string();
                    CheckStatus::Failed(brief)
                }
            };
            let announce = match (&status, manual) {
                (CheckStatus::Available(info), false) => Some(info.version.clone()),
                _ => None,
            };
            mutate(cx, |st| st.status = status);
            if let Some(version) = announce {
                notify_available(version, cx);
            }
            cx.refresh_windows();
        });
    })
    .detach();
}

/// One toast per newly-seen version: "Ferail X is available", with a
/// View button that opens the Software Update dialog.
fn notify_available(version: String, cx: &mut App) {
    let already = snapshot(cx).notified.as_deref() == Some(version.as_str());
    if already {
        return;
    }
    mutate(cx, |st| st.notified = Some(version.clone()));
    let Some(host) = cx
        .active_window()
        .or_else(|| cx.windows().into_iter().next())
    else {
        return;
    };
    let msg = format!(
        "Ferail {version} is available (you have {}).",
        env!("CARGO_PKG_VERSION")
    );
    let _ = host.update(cx, |_, window, cx| {
        window.push_notification(
            Notification::info(msg)
                .title("Update available")
                .action(|_this, _window, cx| {
                    Button::new("update-view")
                        .label("View…")
                        .small()
                        .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                            this.dismiss(window, cx);
                            let cx: &mut App = cx;
                            open_update_dialog(cx);
                        }))
                }),
            cx,
        );
    });
}

// ============================================================================
// Downloading
// ============================================================================

enum DlMsg {
    Progress(Option<f32>),
    Done(PathBuf),
    Failed(String),
}

fn start_download(info: &ReleaseInfo, cx: &mut App) {
    let Some(asset) = info.asset.clone() else {
        // No platform asset on this release — the release page is the
        // download surface then.
        let url = tag_url(&info.tag);
        cx.background_spawn(async move { crate::platform_shell::open_url(&url) })
            .detach();
        return;
    };
    mutate(cx, |st| st.download = DownloadStatus::InProgress(None));
    let client = cx.http_client();
    let (tx, rx) = async_channel::unbounded::<DlMsg>();
    cx.background_executor()
        .spawn(async move {
            let result = download_worker(client, &asset.url, &asset.name, &tx).await;
            let _ = match result {
                Ok(path) => tx.send(DlMsg::Done(path)).await,
                Err(e) => {
                    crate::log_warn!(90, "update download failed: {e:#}");
                    let brief = format!("{e:#}");
                    let brief = brief
                        .lines()
                        .next()
                        .unwrap_or("download failed")
                        .to_string();
                    tx.send(DlMsg::Failed(brief)).await
                }
            };
        })
        .detach();
    cx.spawn(async move |cx| {
        while let Ok(msg) = rx.recv().await {
            cx.update(|cx| {
                match msg {
                    DlMsg::Progress(p) => {
                        mutate(cx, |st| st.download = DownloadStatus::InProgress(p))
                    }
                    DlMsg::Done(path) => {
                        surface_download_done(&path, cx);
                        mutate(cx, |st| st.download = DownloadStatus::Done(path));
                    }
                    DlMsg::Failed(e) => mutate(cx, |st| st.download = DownloadStatus::Failed(e)),
                }
                cx.refresh_windows();
            });
        }
    })
    .detach();
}

/// Success toast, so the outcome is visible even with the dialog closed.
fn surface_download_done(path: &Path, cx: &mut App) {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let folder = path
        .parent()
        .and_then(|p| p.file_name())
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "Downloads".to_string());
    let Some(host) = cx
        .active_window()
        .or_else(|| cx.windows().into_iter().next())
    else {
        return;
    };
    let _ = host.update(cx, |_, window, cx| {
        window.push_notification(
            Notification::success(format!("Downloaded {name} to {folder}.")),
            cx,
        );
    });
}

/// The blocking half: stream the asset to `<Downloads>/<name>.part`,
/// rename into place when complete. Background executor only.
async fn download_worker(
    client: Arc<dyn HttpClient>,
    url: &str,
    name: &str,
    tx: &async_channel::Sender<DlMsg>,
) -> anyhow::Result<PathBuf> {
    use futures_lite::io::AsyncReadExt as _;

    let request = http::Request::get(url)
        .header("Accept", "application/octet-stream")
        .follow_redirects(RedirectPolicy::FollowAll)
        .body(AsyncBody::default())?;
    let response = client.send(request).await?;
    anyhow::ensure!(
        response.status().is_success(),
        "download failed: HTTP {}",
        response.status().as_u16()
    );
    let total: Option<u64> = response
        .headers()
        .get(http::header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse().ok());

    let dir = {
        let d = ferail_fs_native::home_dir().join("Downloads");
        if d.is_dir() {
            d
        } else {
            ferail_fs_native::home_dir()
        }
    };
    let dest = uniquify(&dir, name);
    let part = dest.with_file_name(format!(
        "{}.part",
        dest.file_name().unwrap_or_default().to_string_lossy()
    ));

    {
        use std::io::Write as _;
        let mut file = std::fs::File::create(&part)?;
        let mut body = response.into_body();
        let mut buf = vec![0u8; 128 * 1024];
        let mut got: u64 = 0;
        let mut last_emit: u64 = 0;
        loop {
            let n = body.read(&mut buf).await?;
            if n == 0 {
                break;
            }
            file.write_all(&buf[..n])?;
            got += n as u64;
            // ~4 repaints/MB is plenty for a progress label.
            if got - last_emit >= 256 * 1024 {
                last_emit = got;
                let frac = total.map(|t| (got as f32 / t as f32).clamp(0.0, 1.0));
                let _ = tx.send(DlMsg::Progress(frac)).await;
            }
        }
        file.flush()?;
        if let Some(t) = total {
            anyhow::ensure!(got == t, "download truncated: got {got} of {t} bytes");
        }
    }
    std::fs::rename(&part, &dest)?;
    Ok(dest)
}

/// First free "name", "name (2)", "name (3)", … in `dir`, so a re-download
/// never silently overwrites an earlier (possibly half-installed) copy.
fn uniquify(dir: &Path, name: &str) -> PathBuf {
    let candidate = dir.join(name);
    if !candidate.exists() {
        return candidate;
    }
    let (stem, ext) = match name.rsplit_once('.') {
        Some((s, e)) if !s.is_empty() => (s, Some(e)),
        _ => (name, None),
    };
    for n in 2u32.. {
        let alt = match ext {
            Some(e) => dir.join(format!("{stem} ({n}).{e}")),
            None => dir.join(format!("{stem} ({n})")),
        };
        if !alt.exists() {
            return alt;
        }
    }
    unreachable!()
}

// ============================================================================
// Dialog
// ============================================================================

/// Open the Software Update dialog (singleton, like About). Content is
/// rebuilt from the [`UpdateState`] global every frame, so it tracks the
/// state machine live.
pub fn open_update_dialog(cx: &mut App) {
    if snapshot(cx).dialog_open {
        return;
    }
    mutate(cx, |st| st.dialog_open = true);
    cx.defer(|cx| {
        let Some(host) = cx
            .active_window()
            .or_else(|| cx.windows().into_iter().next())
        else {
            mutate(cx, |st| st.dialog_open = false);
            return;
        };
        let opened = host
            .update(cx, |_, window, cx| {
                window.open_dialog(cx, move |dialog, _window, cx| build_dialog(dialog, cx));
            })
            .is_ok();
        if !opened {
            mutate(cx, |st| st.dialog_open = false);
        }
    });
}

fn build_dialog(dialog: Dialog, cx: &App) -> Dialog {
    let st = snapshot(cx);
    let dialog = dialog
        .title("Software Update")
        .w(px(420.0))
        .overlay_closable(true)
        .keyboard(true)
        .close_button(true)
        .on_close(|_, _window, cx: &mut App| {
            mutate(cx, |st| st.dialog_open = false);
        })
        .child(dialog_body(&st, cx));

    // Footer buttons per state. A `Dialog` only draws buttons it's
    // given a footer for — `button_props` alone renders nothing (same
    // lesson as shell.rs's Go to Folder dialog).
    match (&st.status, &st.download) {
        // Download finished: Show in Folder / Open (primary).
        (_, DownloadStatus::Done(path)) => {
            let open_path = path.clone();
            let reveal_path = path.clone();
            dialog.footer(
                DialogFooter::new()
                    .child(
                        Button::new("update-reveal")
                            .label("Show in Folder")
                            .small()
                            .on_click(move |_, window, cx| {
                                let p = reveal_path.clone();
                                cx.background_spawn(async move {
                                    crate::platform_shell::reveal_in_finder(&p);
                                })
                                .detach();
                                window.close_dialog(cx);
                            }),
                    )
                    .child(
                        Button::new("update-open")
                            .label("Open")
                            .primary()
                            .small()
                            .on_click(move |_, window, cx| {
                                let p = open_path.clone();
                                cx.background_spawn(async move {
                                    if let Err(e) = ferail_fs_native::open_with_default(&p) {
                                        crate::log_warn!(90, "open downloaded update failed: {e}");
                                    }
                                })
                                .detach();
                                window.close_dialog(cx);
                            }),
                    ),
            )
        }
        // Mid-download: no buttons; the close button still works and the
        // download continues in the background (the toast reports it).
        (_, DownloadStatus::InProgress(_)) => dialog,
        // Newer version, not yet (successfully) downloaded.
        (CheckStatus::Available(info), _) => {
            let label = match &info.asset {
                Some(asset) => format!("Download {}", asset.name),
                None => "Open Release Page".to_string(),
            };
            let info = info.clone();
            dialog.footer(
                DialogFooter::new()
                    .child(
                        Button::new("update-later")
                            .label("Later")
                            .small()
                            .on_click(|_, window, cx| window.close_dialog(cx)),
                    )
                    .child(
                        Button::new("update-download")
                            .label(label)
                            .primary()
                            .small()
                            .on_click(move |_, _window, cx| {
                                // Keep the dialog open: it is the
                                // progress UI.
                                start_download(&info, cx);
                            }),
                    ),
            )
        }
        // Idle / checking / up-to-date / failed: nothing to act on.
        _ => dialog,
    }
}

fn dialog_body(st: &UpdateState, cx: &App) -> impl IntoElement {
    use gpui::IntoElement as _;
    let muted = cx.theme().muted_foreground;
    let fg = cx.theme().foreground;
    let current = env!("CARGO_PKG_VERSION");

    let status_line: gpui::AnyElement = match &st.status {
        CheckStatus::Idle | CheckStatus::Checking => div()
            .text_scale_sm()
            .text_color(muted)
            .child("Checking GitHub for the latest release\u{2026}")
            .into_any_element(),
        CheckStatus::UpToDate { latest } => div()
            .text_scale_sm()
            .text_color(fg)
            .child(format!(
                "You're up to date — {latest} is the latest release."
            ))
            .into_any_element(),
        CheckStatus::Available(info) => v_flex()
            .gap_1()
            .child(
                div()
                    .text_scale_sm()
                    .text_color(fg)
                    .child(format!("Ferail {} is available.", info.version)),
            )
            .child(release_notes_row(info.tag.clone()))
            .into_any_element(),
        CheckStatus::Failed(e) => v_flex()
            .gap_1()
            .child(
                div()
                    .text_scale_sm()
                    .text_color(fg)
                    .child("Couldn't check for updates."),
            )
            .child(div().text_scale_xs().text_color(muted).child(e.clone()))
            .into_any_element(),
    };

    let download_line: Option<gpui::AnyElement> = match &st.download {
        DownloadStatus::None => None,
        DownloadStatus::InProgress(frac) => Some(
            div()
                .text_scale_xs()
                .text_color(muted)
                .child(match frac {
                    Some(f) => format!("Downloading\u{2026} {:.0}%", f * 100.0),
                    None => "Downloading\u{2026}".to_string(),
                })
                .into_any_element(),
        ),
        DownloadStatus::Done(path) => Some(
            div()
                .text_scale_xs()
                .text_color(muted)
                .child(format!("Downloaded to {}", path.display()))
                .into_any_element(),
        ),
        DownloadStatus::Failed(e) => Some(
            div()
                .text_scale_xs()
                .text_color(muted)
                .child(format!("Download failed: {e}"))
                .into_any_element(),
        ),
    };

    v_flex()
        .gap_2()
        .py_2()
        .child(
            h_flex()
                .gap_2()
                .child(div().text_scale_xs().text_color(muted).child("Installed"))
                .child(div().text_scale_xs().text_color(fg).child(current)),
        )
        .child(status_line)
        .children(download_line)
}

/// Clickable "Release notes" link → the tag's GitHub page.
fn release_notes_row(tag: String) -> impl IntoElement {
    div()
        .id(ElementId::Name("update-release-notes".into()))
        .cursor_pointer()
        .text_scale_xs()
        .underline()
        .child("Release notes")
        .on_click(move |_: &ClickEvent, _window, cx| {
            let url = tag_url(&tag);
            // LaunchServices/xdg-open can stall — worker, not UI thread.
            cx.background_spawn(async move {
                crate::platform_shell::open_url(&url);
            })
            .detach();
        })
}

// ============================================================================
// Screenshot harness
// ============================================================================

/// Seed the global with a named state and open the dialog — the
/// `--update-dialog <state>` screenshot flag's backend. Pure UI: no
/// network, no filesystem. States mirror the machine above.
pub fn seed_dialog_for_screenshot(state: &str, cx: &mut App) {
    // "live" runs the real check against GitHub — the one state that
    // needs the network; used to verify the whole pipe end to end.
    if state == "live" {
        manual_check(cx);
        return;
    }
    let sample = ReleaseInfo {
        version: "9.9.9".to_string(),
        tag: "v9.9.9".to_string(),
        asset: Some(AssetInfo {
            name: "Ferail-9.9.9.dmg".to_string(),
            url: String::new(),
        }),
    };
    let (status, download) = match state {
        "uptodate" => (
            CheckStatus::UpToDate {
                latest: env!("CARGO_PKG_VERSION").to_string(),
            },
            DownloadStatus::None,
        ),
        "available" => (CheckStatus::Available(sample), DownloadStatus::None),
        "noasset" => (
            CheckStatus::Available(ReleaseInfo {
                asset: None,
                ..sample
            }),
            DownloadStatus::None,
        ),
        "downloading" => (
            CheckStatus::Available(sample),
            DownloadStatus::InProgress(Some(0.42)),
        ),
        "done" => (
            CheckStatus::Available(sample),
            DownloadStatus::Done(
                ferail_fs_native::home_dir()
                    .join("Downloads")
                    .join("Ferail-9.9.9.dmg"),
            ),
        ),
        "failed" => (
            CheckStatus::Failed("status error 403, response: rate limited".to_string()),
            DownloadStatus::None,
        ),
        // "checking" and anything else
        _ => (CheckStatus::Checking, DownloadStatus::None),
    };
    mutate(cx, |st| {
        st.status = status;
        st.download = download;
    });
    open_update_dialog(cx);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_parsing() {
        assert_eq!(parse_version("v0.4.0"), Some((0, 4, 0)));
        assert_eq!(parse_version("1.2.3"), Some((1, 2, 3)));
        assert_eq!(parse_version("v1.2"), None);
        assert_eq!(parse_version("v1.2.3.4"), None);
        assert_eq!(parse_version("v1.2.3-rc1"), None);
        assert_eq!(parse_version("abc"), None);
    }

    #[test]
    fn newer_comparison_uses_numeric_order() {
        // 0.10.0 > 0.9.0 must hold numerically, not lexically.
        assert!(parse_version("v0.10.0") > parse_version("v0.9.0"));
        // Malformed remote tags never count as newer.
        assert!(!is_newer_than_current("not-a-version"));
        // The running version is never "newer" than itself.
        assert!(!is_newer_than_current(env!("CARGO_PKG_VERSION")));
    }

    #[test]
    fn asset_pick_matches_ci_names() {
        let names = [
            "Ferail-0.5.0-win-x64.zip",
            "Ferail-0.5.0.dmg",
            "ferail_0.5.0-1_amd64.deb",
            "ferail_0.5.0-1_arm64.deb",
        ];
        let picked = pick_asset_index(&names);
        match std::env::consts::OS {
            "macos" => assert_eq!(picked, Some(1)),
            "windows" => assert_eq!(picked, Some(0)),
            "linux" => {
                let expect = if deb_arch() == "amd64" { 2 } else { 3 };
                assert_eq!(picked, Some(expect));
            }
            _ => assert_eq!(picked, None),
        }
    }

    /// Real network + real release asset — run explicitly with
    /// `cargo test -p ferail-gpui update_check -- --ignored`.
    /// Exercises the whole download path: redirect-following GET,
    /// content-length accounting, `.part` write, rename.
    #[test]
    #[ignore = "network: downloads a real GitHub release asset"]
    fn download_worker_fetches_a_real_asset() {
        let client: Arc<dyn HttpClient> =
            Arc::new(reqwest_client::ReqwestClient::user_agent("Ferail-test").unwrap());
        // Smallest stable asset: the 0.4.0 arm64 .deb (~14 MB).
        let url =
            format!("https://github.com/{REPO}/releases/download/v0.4.0/ferail_0.4.0-1_arm64.deb");
        // Progress messages pile up unread in the unbounded channel; fine.
        let (tx, _rx) = async_channel::unbounded::<DlMsg>();
        let path = futures_lite::future::block_on(download_worker(
            client,
            &url,
            "ferail-update-test.deb",
            &tx,
        ))
        .unwrap();
        // download_worker writes to ~/Downloads; clean up after ourselves.
        let meta = std::fs::metadata(&path).unwrap();
        assert!(
            meta.len() > 10_000_000,
            "suspiciously small: {}",
            meta.len()
        );
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn uniquify_appends_counter_before_extension() {
        let dir = std::env::temp_dir().join(format!("ferail-uniq-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let first = uniquify(&dir, "a.dmg");
        assert_eq!(first, dir.join("a.dmg"));
        std::fs::write(&first, b"x").unwrap();
        assert_eq!(uniquify(&dir, "a.dmg"), dir.join("a (2).dmg"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
