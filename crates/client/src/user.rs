use super::{Client, Status, TypedEnvelope, proto};
use anyhow::{Context as _, Result};
use chrono::{DateTime, Utc};
use cloud_api_client::websocket_protocol::MessageToClient;
use cloud_api_client::{GetAuthenticatedUserResponse, PlanInfo};
use collections::{HashMap, HashSet, hash_map::Entry};
use derive_more::Deref;
use feature_flags::FeatureFlagAppExt;
use futures::{Future, StreamExt, channel::mpsc};
use gpui::{
    App, AsyncApp, Context, Entity, EventEmitter, SharedString, SharedUri, Task, WeakEntity,
};
use http_client::http::{HeaderMap, HeaderValue};
use postage::{sink::Sink, watch};
use rpc::proto::{RequestMessage, UsersResponse};
use std::{
    str::FromStr as _,
    sync::{Arc, Weak},
};
use text::ReplicaId;
use util::{ResultExt, TryFutureExt as _};

pub type UserId = u64;

#[derive(
    Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Clone, Copy, serde::Serialize, serde::Deserialize,
)]
pub struct ChannelId(pub u64);

impl std::fmt::Display for ChannelId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Clone, Copy)]
pub struct ProjectId(pub u64);

impl ProjectId {
    pub fn to_proto(self) -> u64 {
        self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParticipantIndex(pub u32);

#[derive(Default, Debug)]
pub struct User {
    pub id: UserId,
    pub github_login: SharedString,
    pub avatar_uri: SharedUri,
    pub name: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Collaborator {
    pub peer_id: proto::PeerId,
    pub replica_id: ReplicaId,
    pub user_id: UserId,
    pub is_host: bool,
    pub committer_name: Option<String>,
    pub committer_email: Option<String>,
}

impl PartialOrd for User {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for User {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.github_login.cmp(&other.github_login)
    }
}

impl PartialEq for User {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id && self.github_login == other.github_login
    }
}

impl Eq for User {}

#[derive(Debug, PartialEq)]
pub struct Contact {
    pub user: Arc<User>,
    pub online: bool,
    pub busy: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContactRequestStatus {
    None,
    RequestSent,
    RequestReceived,
    RequestAccepted,
}

#[derive(Clone)]
pub struct InviteInfo {
    pub count: u32,
    pub url: Arc<str>,
}

pub enum Event {
    Contact {
        user: Arc<User>,
        kind: ContactEventKind,
    },
    ShowContacts,
    ParticipantIndicesChanged,
    PrivateUserInfoUpdated,
    PlanUpdated,
}

#[derive(Clone, Copy)]
pub enum ContactEventKind {
    Requested,
    Accepted,
    Cancelled,
}

impl User {
    fn new(message: proto::User) -> Arc<Self> {
        Arc::new(User {
            id: message.id,
            github_login: message.github_login.into(),
            avatar_uri: message.avatar_url.into(),
            name: message.name,
        })
    }
}

impl Collaborator {
    pub fn from_proto(message: proto::Collaborator) -> Result<Self> {
        Ok(Self {
            peer_id: message.peer_id.context("invalid peer id")?,
            replica_id: ReplicaId::new(message.replica_id as u16),
            user_id: message.user_id as UserId,
            is_host: message.is_host,
            committer_name: message.committer_name,
            committer_email: message.committer_email,
        })
    }
}
