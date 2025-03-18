use std::sync::Arc;

use gpui::Entity;

use crate::manager::ContextServerManager;
use crate::types;

pub struct ContextServerTool {
    server_manager: Entity<ContextServerManager>,
    server_id: Arc<str>,
    tool: types::Tool,
}

impl ContextServerTool {
    pub fn new(
        server_manager: Entity<ContextServerManager>,
        server_id: impl Into<Arc<str>>,
        tool: types::Tool,
    ) -> Self {
        Self {
            server_manager,
            server_id: server_id.into(),
            tool,
        }
    }
}
