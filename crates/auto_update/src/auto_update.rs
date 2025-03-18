use anyhow::{Context as _, Result};
use db::kvp::KEY_VALUE_STORE;
use gpui::{
    App, AppContext as _, AsyncApp, Context, Entity, Global, SemanticVersion, Task, Window, actions,
};
use http_client::{AsyncBody, HttpClient, HttpClientWithUrl};
use paths::remote_servers_dir;
use release_channel::{AppCommitSha, ReleaseChannel};
use serde::{Deserialize, Serialize};
use smol::fs::File;
use smol::io::AsyncReadExt;
use std::{path::PathBuf, sync::Arc};
use workspace::Workspace;

const SHOULD_SHOW_UPDATE_NOTIFICATION_KEY: &str = "auto-updater-should-show-updated-notification";

actions!(
    auto_update,
    [
        /// Checks for available updates.
        Check,
        /// Dismisses the update error message.
        DismissMessage,
        /// Opens the release notes for the current version in a browser.
        ViewReleaseNotes,
    ]
);

#[derive(Serialize)]
struct UpdateRequestBody {
    installation_id: Option<Arc<str>>,
    release_channel: Option<&'static str>,
    telemetry: bool,
    is_staff: Option<bool>,
    destination: &'static str,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum VersionCheckType {
    Sha(AppCommitSha),
    Semantic(SemanticVersion),
}

#[derive(Clone)]
pub enum AutoUpdateStatus {
    Idle,
    Checking,
    Downloading { version: VersionCheckType },
    Installing { version: VersionCheckType },
    Updated { version: VersionCheckType },
    Errored { error: Arc<anyhow::Error> },
}

impl AutoUpdateStatus {
    pub fn is_updated(&self) -> bool {
        matches!(self, Self::Updated { .. })
    }
}

pub struct AutoUpdater {
    status: AutoUpdateStatus,
    current_version: SemanticVersion,
    http_client: Arc<HttpClientWithUrl>,
    pending_poll: Option<Task<Option<()>>>,
    quit_subscription: Option<gpui::Subscription>,
}

#[derive(Deserialize, Clone, Debug)]
pub struct JsonRelease {
    pub version: String,
    pub url: String,
}

#[cfg(target_os = "darwin")]
struct MacOsUnmounter<'a> {
    mount_path: PathBuf,
    background_executor: &'a BackgroundExecutor,
}

#[cfg(target_os = "darwin")]
impl Drop for MacOsUnmounter<'_> {
    fn drop(&mut self) {
        let mount_path = mem::take(&mut self.mount_path);
        self.background_executor
            .spawn(async move {
                let unmount_output = Command::new("hdiutil")
                    .args(["detach", "-force"])
                    .arg(&mount_path)
                    .output()
                    .await;
                match unmount_output {
                    Ok(output) if output.status.success() => {
                        log::info!("Successfully unmounted the disk image");
                    }
                    Ok(output) => {
                        log::error!(
                            "Failed to unmount disk image: {:?}",
                            String::from_utf8_lossy(&output.stderr)
                        );
                    }
                    Err(error) => {
                        log::error!("Error while trying to unmount disk image: {:?}", error);
                    }
                }
            })
            .detach();
    }
}

/// Whether or not to automatically check for updates.
///
/// Default: true
#[derive(Clone, Copy, Default, Deserialize, Serialize)]
#[serde(transparent)]
struct AutoUpdateSettingContent(bool);

#[derive(Default)]
struct GlobalAutoUpdate(Option<Entity<AutoUpdater>>);

impl Global for GlobalAutoUpdate {}

pub fn init(_: Arc<HttpClientWithUrl>, cx: &mut App) {
    cx.observe_new(|workspace: &mut Workspace, _window, _cx| {
        workspace.register_action(|_, action, window, cx| check(action, window, cx));

        workspace.register_action(|_, action, _, cx| {
            view_release_notes(action, cx);
        });
    })
    .detach();

    cx.set_global(GlobalAutoUpdate(None));
}

pub fn check(_: &Check, window: &mut Window, cx: &mut App) {
    if let Some(message) = option_env!("ZED_UPDATE_EXPLANATION") {
        drop(window.prompt(
            gpui::PromptLevel::Info,
            "Zed was installed via a package manager.",
            Some(message),
            &["Ok"],
            cx,
        ));
        return;
    }

    drop(window.prompt(
        gpui::PromptLevel::Info,
        "Could not check for updates",
        Some("Auto-updates disabled for non-bundled app."),
        &["Ok"],
        cx,
    ));
}

pub fn view_release_notes(_: &ViewReleaseNotes, cx: &mut App) -> Option<()> {
    let auto_updater = AutoUpdater::get(cx)?;
    let release_channel = ReleaseChannel::try_global(cx)?;

    match release_channel {
        ReleaseChannel::Stable | ReleaseChannel::Preview => {
            let auto_updater = auto_updater.read(cx);
            let current_version = auto_updater.current_version;
            let release_channel = release_channel.dev_name();
            let path = format!("/releases/{release_channel}/{current_version}");
            let url = &auto_updater.http_client.build_url(&path);
            cx.open_url(url);
        }
        ReleaseChannel::Nightly => {
            cx.open_url("https://github.com/zed-industries/zed/commits/nightly/");
        }
        ReleaseChannel::Dev => {
            cx.open_url("https://github.com/zed-industries/zed/commits/main/");
        }
    }
    None
}

#[cfg(target_os = "windows")]
struct InstallerDir(PathBuf);

#[cfg(target_os = "windows")]
impl InstallerDir {
    async fn new() -> Result<Self> {
        let installer_dir = std::env::current_exe()?
            .parent()
            .context("No parent dir for Zed.exe")?
            .join("updates");
        if smol::fs::metadata(&installer_dir).await.is_ok() {
            smol::fs::remove_dir_all(&installer_dir).await?;
        }
        smol::fs::create_dir(&installer_dir).await?;
        Ok(Self(installer_dir))
    }

    fn path(&self) -> &Path {
        self.0.as_path()
    }
}

pub enum UpdateCheckType {
    Automatic,
    Manual,
}

impl AutoUpdater {
    pub fn get(cx: &mut App) -> Option<Entity<Self>> {
        cx.default_global::<GlobalAutoUpdate>().0.clone()
    }

    pub fn current_version(&self) -> SemanticVersion {
        self.current_version
    }

    pub fn status(&self) -> AutoUpdateStatus {
        self.status.clone()
    }

    pub fn dismiss(&mut self, cx: &mut Context<Self>) -> bool {
        if let AutoUpdateStatus::Idle = self.status {
            return false;
        }
        self.status = AutoUpdateStatus::Idle;
        cx.notify();
        true
    }

    // If you are packaging Zed and need to override the place it downloads SSH remotes from,
    // you can override this function. You should also update get_remote_server_release_url to return
    // Ok(None).
    pub async fn download_remote_server_release(
        os: &str,
        arch: &str,
        release_channel: ReleaseChannel,
        version: Option<SemanticVersion>,
        cx: &mut AsyncApp,
    ) -> Result<PathBuf> {
        let this = cx.update(|cx| {
            cx.default_global::<GlobalAutoUpdate>()
                .0
                .clone()
                .context("auto-update not initialized")
        })??;

        let release = Self::get_release(
            &this,
            "zed-remote-server",
            os,
            arch,
            version,
            Some(release_channel),
            cx,
        )
        .await?;

        let servers_dir = paths::remote_servers_dir();
        let channel_dir = servers_dir.join(release_channel.dev_name());
        let platform_dir = channel_dir.join(format!("{}-{}", os, arch));
        let version_path = platform_dir.join(format!("{}.gz", release.version));
        smol::fs::create_dir_all(&platform_dir).await.ok();

        let client = this.read_with(cx, |this, _| this.http_client.clone())?;

        if smol::fs::metadata(&version_path).await.is_err() {
            log::info!(
                "downloading zed-remote-server {os} {arch} version {}",
                release.version
            );
            download_remote_server_binary(&version_path, release, client, cx).await?;
        }

        Ok(version_path)
    }

    pub async fn get_remote_server_release_url(
        os: &str,
        arch: &str,
        release_channel: ReleaseChannel,
        version: Option<SemanticVersion>,
        cx: &mut AsyncApp,
    ) -> Result<Option<(String, String)>> {
        let this = cx.update(|cx| {
            cx.default_global::<GlobalAutoUpdate>()
                .0
                .clone()
                .context("auto-update not initialized")
        })??;

        let release = Self::get_release(
            &this,
            "zed-remote-server",
            os,
            arch,
            version,
            Some(release_channel),
            cx,
        )
        .await?;

        let update_request_body = build_remote_server_update_request_body(cx)?;
        let body = serde_json::to_string(&update_request_body)?;

        Ok(Some((release.url, body)))
    }

    async fn get_release(
        this: &Entity<Self>,
        asset: &str,
        os: &str,
        arch: &str,
        version: Option<SemanticVersion>,
        release_channel: Option<ReleaseChannel>,
        cx: &mut AsyncApp,
    ) -> Result<JsonRelease> {
        let client = this.read_with(cx, |this, _| this.http_client.clone())?;

        if let Some(version) = version {
            let channel = release_channel.map(|c| c.dev_name()).unwrap_or("stable");

            let url = format!("/api/releases/{channel}/{version}/{asset}-{os}-{arch}.gz?update=1",);

            Ok(JsonRelease {
                version: version.to_string(),
                url: client.build_url(&url),
            })
        } else {
            let mut url_string = client.build_url(&format!(
                "/api/releases/latest?asset={}&os={}&arch={}",
                asset, os, arch
            ));
            if let Some(param) = release_channel.and_then(|c| c.release_query_param()) {
                url_string += "&";
                url_string += param;
            }

            let mut response = client.get(&url_string, Default::default(), true).await?;
            let mut body = Vec::new();
            response.body_mut().read_to_end(&mut body).await?;

            anyhow::ensure!(
                response.status().is_success(),
                "failed to fetch release: {:?}",
                String::from_utf8_lossy(&body),
            );

            serde_json::from_slice(body.as_slice()).with_context(|| {
                format!(
                    "error deserializing release {:?}",
                    String::from_utf8_lossy(&body),
                )
            })
        }
    }

    pub fn should_show_update_notification(&self, cx: &App) -> Task<Result<bool>> {
        cx.background_spawn(async move {
            Ok(KEY_VALUE_STORE
                .read_kvp(SHOULD_SHOW_UPDATE_NOTIFICATION_KEY)?
                .is_some())
        })
    }
}

async fn download_remote_server_binary(
    target_path: &PathBuf,
    release: JsonRelease,
    client: Arc<HttpClientWithUrl>,
    cx: &AsyncApp,
) -> Result<()> {
    let temp = tempfile::Builder::new().tempfile_in(remote_servers_dir())?;
    let mut temp_file = File::create(&temp).await?;
    let update_request_body = build_remote_server_update_request_body(cx)?;
    let request_body = AsyncBody::from(serde_json::to_string(&update_request_body)?);

    let mut response = client.get(&release.url, request_body, true).await?;
    anyhow::ensure!(
        response.status().is_success(),
        "failed to download remote server release: {:?}",
        response.status()
    );
    smol::io::copy(response.body_mut(), &mut temp_file).await?;
    smol::fs::rename(&temp, &target_path).await?;

    Ok(())
}

fn build_remote_server_update_request_body(cx: &AsyncApp) -> Result<UpdateRequestBody> {
    let release_channel = cx.update(|cx| {
        let release_channel =
            ReleaseChannel::try_global(cx).map(|release_channel| release_channel.display_name());

        release_channel
    })?;

    Ok(UpdateRequestBody {
        installation_id: None,
        release_channel,
        telemetry: false,
        is_staff: None,
        destination: "remote",
    })
}

pub async fn finalize_auto_update_on_quit() {
    let Some(installer_path) = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|p| p.join("updates")))
    else {
        return;
    };

    // The installer will create a flag file after it finishes updating
    let flag_file = installer_path.join("versions.txt");
    if flag_file.exists()
        && let Some(helper) = installer_path
            .parent()
            .map(|p| p.join("tools").join("auto_update_helper.exe"))
    {
        let mut command = smol::process::Command::new(helper);
        command.arg("--launch");
        command.arg("false");
        if let Ok(mut cmd) = command.spawn() {
            _ = cmd.status().await;
        }
    }
}
