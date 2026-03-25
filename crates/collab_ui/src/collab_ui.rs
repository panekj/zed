use std::sync::Arc;

use std::{rc::Rc, sync::Arc};

pub use collab_panel::CollabPanel;
use gpui::{
    App, Pixels, PlatformDisplay, Size, WindowBackgroundAppearance, WindowBounds,
    WindowDecorations, WindowKind, WindowOptions, point,
};
pub use panel_settings::CollaborationPanelSettings;
use release_channel::ReleaseChannel;
use ui::px;
use workspace::AppState;

// Another comment, nice.
pub fn init(_app_state: &Arc<AppState>, _cx: &mut App) {}
