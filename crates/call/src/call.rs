use std::sync::Arc;

use client::{Client, UserStore};
use gpui::{App, Entity};

pub fn init(_client: Arc<Client>, _user_store: Entity<UserStore>, _cx: &mut App) {}
