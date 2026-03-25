use std::rc::Rc;
use std::sync::Arc;

use client::{User, proto::PeerId};
use gpui::{
    AnyElement, Hsla, IntoElement, MouseButton, Path, ScreenCaptureSource, Styled, TaskExt,
    WeakEntity, canvas, point,
};
use gpui::{App, Task, Window};
use icons::IconName;
use project::WorktreeSettings;
use remote_connection::RemoteConnectionModal;
use rpc::proto::{self};
use settings::{Settings as _, SettingsLocation};
use theme::ActiveTheme;
use ui::{
    Avatar, AvatarAudioStatusIndicator, ContextMenu, ContextMenuItem, Divider, DividerColor,
    Facepile, PopoverMenu, SplitButton, SplitButtonStyle, TintColor, Tooltip, prelude::*,
};
use util::rel_path::RelPath;
use workspace::{ParticipantLocation, notifications::DetachAndPromptErr};
use zed_actions::ShowCallStats;

use crate::TitleBar;

fn format_stat(value: Option<f64>, format: impl Fn(f64) -> String) -> String {
    match value {
        Some(v) => format(v),
        None => "—".to_string(),
    }
}

pub fn toggle_screen_sharing(
    screen: anyhow::Result<Option<Rc<dyn ScreenCaptureSource>>>,
    window: &mut Window,
    cx: &mut App,
) {
}

pub fn toggle_mute(cx: &mut App) {}

pub fn toggle_deafen(cx: &mut App) {}

fn render_color_ribbon(color: Hsla) -> impl Element {
    canvas(
        move |_, _, _| {},
        move |bounds, _, window, _| {
            let height = bounds.size.height;
            let horizontal_offset = height;
            let vertical_offset = height / 2.0;
            let mut path = Path::new(bounds.bottom_left());
            path.curve_to(
                bounds.origin + point(horizontal_offset, vertical_offset),
                bounds.origin + point(px(0.0), vertical_offset),
            );
            path.line_to(bounds.top_right() + point(-horizontal_offset, vertical_offset));
            path.curve_to(
                bounds.bottom_right(),
                bounds.top_right() + point(px(0.0), vertical_offset),
            );
            path.line_to(bounds.bottom_left());
            window.paint_path(path, color);
        },
    )
    .h_1()
    .w_full()
}

impl TitleBar {
    pub(crate) fn render_collaborator_list(
        &self,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let current_user = self.user_store.read(cx).current_user();
        let client = self.client.clone();
        let project_id = self.project.read(cx).remote_id();
        let workspace = self.workspace.upgrade();

        h_flex()
            .id("collaborator-list")
            .w_full()
            .gap_1()
            .overflow_x_scroll()
    }

    fn render_collaborator(
        &self,
        user: &Arc<User>,
        peer_id: PeerId,
        is_present: bool,
        is_speaking: bool,
        is_muted: bool,
        leader_selection_color: Option<Hsla>,
        room: (),
        project_id: Option<u64>,
        current_user: &Arc<User>,
        cx: &App,
    ) -> Option<Div> {
        None
    }

    pub(crate) fn render_call_controls(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        vec![]
    }

    fn render_screen_list(&self) -> impl IntoElement {
        div()
    }
}

/// Picks the screen to share when clicking on the main screen sharing button.
fn pick_default_screen(cx: &App) -> Task<anyhow::Result<Option<Rc<dyn ScreenCaptureSource>>>> {
    let source = cx.screen_capture_sources();
    cx.spawn(async move |_| {
        let available_sources = source.await??;
        Ok(available_sources
            .iter()
            .find(|it| {
                it.as_ref()
                    .metadata()
                    .is_ok_and(|meta| meta.is_main.unwrap_or_default())
            })
            .or_else(|| available_sources.first())
            .cloned())
    })
}
