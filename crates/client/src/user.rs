use crate::Subscription;

use super::{Client, proto};
use anyhow::{Context as _, Result};
use chrono::{DateTime, Utc};
use collections::{HashMap, hash_map::Entry};
use futures::{Future, StreamExt, channel::mpsc};
use gpui::{App, Context, EventEmitter, SharedString, SharedUri, Task, WeakEntity};
use postage::watch;
use rpc::proto::{RequestMessage, UsersResponse};
use std::sync::{Arc, Weak};
use text::ReplicaId;

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
    pub fn to_proto(&self) -> u64 {
        self.0
    }
}

#[derive(
    Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Clone, Copy, serde::Serialize, serde::Deserialize,
)]
pub struct DevServerProjectId(pub u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParticipantIndex(pub u32);

#[derive(Default, Debug)]
pub struct User {
    pub id: UserId,
    pub github_login: String,
    pub avatar_uri: SharedUri,
    pub name: Option<String>,
    pub email: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Collaborator {
    pub peer_id: proto::PeerId,
    pub replica_id: ReplicaId,
    pub user_id: UserId,
    pub is_host: bool,
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

pub struct UserStore {
    users: HashMap<u64, Arc<User>>,
    by_github_login: HashMap<String, u64>,
    participant_indices: HashMap<u64, ParticipantIndex>,
    update_contacts_tx: mpsc::UnboundedSender<UpdateContacts>,
    current_plan: Option<proto::Plan>,
    subscription_period: Option<(DateTime<Utc>, DateTime<Utc>)>,
    trial_started_at: Option<DateTime<Utc>>,
    model_request_usage_amount: Option<u32>,
    model_request_usage_limit: Option<proto::UsageLimit>,
    edit_predictions_usage_amount: Option<u32>,
    edit_predictions_usage_limit: Option<proto::UsageLimit>,
    is_usage_based_billing_enabled: Option<bool>,
    account_too_young: Option<bool>,
    has_overdue_invoices: Option<bool>,
    current_user: watch::Receiver<Option<Arc<User>>>,
    accepted_tos_at: Option<Option<DateTime<Utc>>>,
    contacts: Vec<Arc<Contact>>,
    incoming_contact_requests: Vec<Arc<User>>,
    outgoing_contact_requests: Vec<Arc<User>>,
    pending_contact_requests: HashMap<u64, usize>,
    invite_info: Option<InviteInfo>,
    client: Weak<Client>,
    _maintain_contacts: Task<()>,
    _maintain_current_user: Task<Result<()>>,
    weak_self: WeakEntity<Self>,
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
}

#[derive(Clone, Copy)]
pub enum ContactEventKind {
    Requested,
    Accepted,
    Cancelled,
}

impl EventEmitter<Event> for UserStore {}

enum UpdateContacts {
    Wait(postage::barrier::Sender),
    Clear(postage::barrier::Sender),
}

impl UserStore {
    pub fn new(client: Arc<Client>, cx: &Context<Self>) -> Self {
        let (mut current_user_tx, current_user_rx) = watch::channel();
        let (update_contacts_tx, mut _update_contacts_rx) = mpsc::unbounded();
        let rpc_subscriptions: Vec<Subscription> = vec![];
        Self {
            users: Default::default(),
            by_github_login: Default::default(),
            current_user: current_user_rx,
            current_plan: None,
            subscription_period: None,
            trial_started_at: None,
            model_request_usage_amount: None,
            model_request_usage_limit: None,
            edit_predictions_usage_amount: None,
            edit_predictions_usage_limit: None,
            is_usage_based_billing_enabled: None,
            account_too_young: None,
            has_overdue_invoices: None,
            accepted_tos_at: None,
            contacts: Default::default(),
            incoming_contact_requests: Default::default(),
            participant_indices: Default::default(),
            outgoing_contact_requests: Default::default(),
            invite_info: None,
            client: Arc::downgrade(&client),
            update_contacts_tx,
            _maintain_contacts: Task::ready(()),
            _maintain_current_user: cx.spawn(async move |this, cx| {
                let mut status = client.status();
                let weak = Arc::downgrade(&client);
                drop(client);
                while let Some(status) = status.next().await {
                    // if the client is dropped, the app is shutting down.
                    let Some(client) = weak.upgrade() else {
                        return Ok(());
                    };
                    match status {
                        _ => {}
                    }
                }
                Ok(())
            }),
            pending_contact_requests: Default::default(),
            weak_self: cx.weak_entity(),
        }
    }

    #[cfg(feature = "test-support")]
    pub fn clear_cache(&mut self) {
        self.users.clear();
        self.by_github_login.clear();
    }

    pub fn invite_info(&self) -> Option<&InviteInfo> {
        self.invite_info.as_ref()
    }

    pub fn contacts(&self) -> &[Arc<Contact>] {
        &self.contacts
    }

    pub fn has_contact(&self, user: &Arc<User>) -> bool {
        self.contacts
            .binary_search_by_key(&&user.github_login, |contact| &contact.user.github_login)
            .is_ok()
    }

    pub fn incoming_contact_requests(&self) -> &[Arc<User>] {
        &self.incoming_contact_requests
    }

    pub fn outgoing_contact_requests(&self) -> &[Arc<User>] {
        &self.outgoing_contact_requests
    }

    pub fn is_contact_request_pending(&self, user: &User) -> bool {
        self.pending_contact_requests.contains_key(&user.id)
    }

    pub fn contact_request_status(&self, user: &User) -> ContactRequestStatus {
        if self
            .contacts
            .binary_search_by_key(&&user.github_login, |contact| &contact.user.github_login)
            .is_ok()
        {
            ContactRequestStatus::RequestAccepted
        } else if self
            .outgoing_contact_requests
            .binary_search_by_key(&&user.github_login, |user| &user.github_login)
            .is_ok()
        {
            ContactRequestStatus::RequestSent
        } else if self
            .incoming_contact_requests
            .binary_search_by_key(&&user.github_login, |user| &user.github_login)
            .is_ok()
        {
            ContactRequestStatus::RequestReceived
        } else {
            ContactRequestStatus::None
        }
    }

    pub fn request_contact(
        &mut self,
        responder_id: u64,
        cx: &mut Context<Self>,
    ) -> Task<Result<()>> {
        self.perform_contact_request(responder_id, proto::RequestContact { responder_id }, cx)
    }

    pub fn remove_contact(&mut self, _user_id: u64, _cx: &mut Context<Self>) {}

    pub fn has_incoming_contact_request(&self, user_id: u64) -> bool {
        self.incoming_contact_requests
            .iter()
            .any(|user| user.id == user_id)
    }

    pub fn respond_to_contact_request(
        &mut self,
        requester_id: u64,
        accept: bool,
        cx: &mut Context<Self>,
    ) -> Task<Result<()>> {
        self.perform_contact_request(
            requester_id,
            proto::RespondToContactRequest {
                requester_id,
                response: if accept {
                    proto::ContactRequestResponse::Accept
                } else {
                    proto::ContactRequestResponse::Decline
                } as i32,
            },
            cx,
        )
    }

    pub fn dismiss_contact_request(
        &self,
        requester_id: u64,
        cx: &Context<Self>,
    ) -> Task<Result<()>> {
        let client = self.client.upgrade();
        cx.spawn(async move |_, _| {
            client
                .context("can't upgrade client reference")?
                .request(proto::RespondToContactRequest {
                    requester_id,
                    response: proto::ContactRequestResponse::Dismiss as i32,
                })
                .await?;
            Ok(())
        })
    }

    fn perform_contact_request<T: RequestMessage>(
        &mut self,
        user_id: u64,
        request: T,
        cx: &mut Context<Self>,
    ) -> Task<Result<()>> {
        let client = self.client.upgrade();
        *self.pending_contact_requests.entry(user_id).or_insert(0) += 1;
        cx.notify();

        cx.spawn(async move |this, cx| {
            let response = client
                .context("can't upgrade client reference")?
                .request(request)
                .await;
            this.update(cx, |this, cx| {
                if let Entry::Occupied(mut request_count) =
                    this.pending_contact_requests.entry(user_id)
                {
                    *request_count.get_mut() -= 1;
                    if *request_count.get() == 0 {
                        request_count.remove();
                    }
                }
                cx.notify();
            })?;
            response?;
            Ok(())
        })
    }

    pub fn clear_contacts(&self) -> impl Future<Output = ()> + use<> {
        let (tx, mut rx) = postage::barrier::channel();
        self.update_contacts_tx
            .unbounded_send(UpdateContacts::Clear(tx))
            .unwrap();
        async move {
            rx.next().await;
        }
    }

    pub fn contact_updates_done(&self) -> impl Future<Output = ()> {
        let (tx, mut rx) = postage::barrier::channel();
        self.update_contacts_tx
            .unbounded_send(UpdateContacts::Wait(tx))
            .unwrap();
        async move {
            rx.next().await;
        }
    }

    pub fn get_users(
        &self,
        user_ids: Vec<u64>,
        cx: &Context<Self>,
    ) -> Task<Result<Vec<Arc<User>>>> {
        let mut user_ids_to_fetch = user_ids.clone();
        user_ids_to_fetch.retain(|id| !self.users.contains_key(id));

        cx.spawn(async move |this, cx| {
            if !user_ids_to_fetch.is_empty() {
                this.update(cx, |this, cx| {
                    this.load_users(
                        proto::GetUsers {
                            user_ids: user_ids_to_fetch,
                        },
                        cx,
                    )
                })?
                .await?;
            }

            this.read_with(cx, |this, _| {
                user_ids
                    .iter()
                    .map(|user_id| {
                        this.users
                            .get(user_id)
                            .cloned()
                            .with_context(|| format!("user {user_id} not found"))
                    })
                    .collect()
            })?
        })
    }

    pub fn fuzzy_search_users(
        &self,
        query: String,
        cx: &Context<Self>,
    ) -> Task<Result<Vec<Arc<User>>>> {
        self.load_users(proto::FuzzySearchUsers { query }, cx)
    }

    pub fn get_cached_user(&self, user_id: u64) -> Option<Arc<User>> {
        self.users.get(&user_id).cloned()
    }

    pub fn get_user_optimistic(&self, user_id: u64, cx: &Context<Self>) -> Option<Arc<User>> {
        if let Some(user) = self.users.get(&user_id).cloned() {
            return Some(user);
        }

        self.get_user(user_id, cx).detach_and_log_err(cx);
        None
    }

    pub fn get_user(&self, user_id: u64, cx: &Context<Self>) -> Task<Result<Arc<User>>> {
        if let Some(user) = self.users.get(&user_id).cloned() {
            return Task::ready(Ok(user));
        }

        let load_users = self.get_users(vec![user_id], cx);
        cx.spawn(async move |this, cx| {
            load_users.await?;
            this.read_with(cx, |this, _| {
                this.users
                    .get(&user_id)
                    .cloned()
                    .context("server responded with no users")
            })?
        })
    }

    pub fn cached_user_by_github_login(&self, github_login: &str) -> Option<Arc<User>> {
        self.by_github_login
            .get(github_login)
            .and_then(|id| self.users.get(id).cloned())
    }

    pub fn current_user(&self) -> Option<Arc<User>> {
        self.current_user.borrow().clone()
    }

    pub fn current_plan(&self) -> Option<proto::Plan> {
        self.current_plan
    }

    pub fn subscription_period(&self) -> Option<(DateTime<Utc>, DateTime<Utc>)> {
        self.subscription_period
    }

    pub fn trial_started_at(&self) -> Option<DateTime<Utc>> {
        self.trial_started_at
    }

    pub fn usage_based_billing_enabled(&self) -> Option<bool> {
        self.is_usage_based_billing_enabled
    }

    pub fn model_request_usage_amount(&self) -> Option<u32> {
        self.model_request_usage_amount
    }

    pub fn model_request_usage_limit(&self) -> Option<proto::UsageLimit> {
        self.model_request_usage_limit.clone()
    }

    pub fn edit_predictions_usage_amount(&self) -> Option<u32> {
        self.edit_predictions_usage_amount
    }

    pub fn edit_predictions_usage_limit(&self) -> Option<proto::UsageLimit> {
        self.edit_predictions_usage_limit.clone()
    }

    pub fn watch_current_user(&self) -> watch::Receiver<Option<Arc<User>>> {
        self.current_user.clone()
    }

    /// Returns whether the user's account is too new to use the service.
    pub fn account_too_young(&self) -> bool {
        self.account_too_young.unwrap_or(false)
    }

    /// Returns whether the current user has overdue invoices and usage should be blocked.
    pub fn has_overdue_invoices(&self) -> bool {
        self.has_overdue_invoices.unwrap_or(false)
    }

    pub fn current_user_has_accepted_terms(&self) -> Option<bool> {
        Some(true)
    }

    fn load_users(
        &self,
        request: impl RequestMessage<Response = UsersResponse>,
        cx: &Context<Self>,
    ) -> Task<Result<Vec<Arc<User>>>> {
        let client = self.client.clone();
        cx.spawn(async move |this, cx| {
            if let Some(rpc) = client.upgrade() {
                let response = rpc.request(request).await.context("error loading users")?;
                let users = response.users;

                this.update(cx, |this, _| this.insert(users))
            } else {
                Ok(Vec::new())
            }
        })
    }

    pub fn insert(&mut self, users: Vec<proto::User>) -> Vec<Arc<User>> {
        let mut ret = Vec::with_capacity(users.len());
        for user in users {
            let user = User::new(user);
            if let Some(old) = self.users.insert(user.id, user.clone()) {
                if old.github_login != user.github_login {
                    self.by_github_login.remove(&old.github_login);
                }
            }
            self.by_github_login
                .insert(user.github_login.clone(), user.id);
            ret.push(user)
        }
        ret
    }

    pub fn set_participant_indices(
        &mut self,
        participant_indices: HashMap<u64, ParticipantIndex>,
        cx: &mut Context<Self>,
    ) {
        if participant_indices != self.participant_indices {
            self.participant_indices = participant_indices;
            cx.emit(Event::ParticipantIndicesChanged);
        }
    }

    pub fn participant_indices(&self) -> &HashMap<u64, ParticipantIndex> {
        &self.participant_indices
    }

    pub fn participant_names(
        &self,
        user_ids: impl Iterator<Item = u64>,
        cx: &App,
    ) -> HashMap<u64, SharedString> {
        let mut ret = HashMap::default();
        let mut missing_user_ids = Vec::new();
        for id in user_ids {
            if let Some(github_login) = self.get_cached_user(id).map(|u| u.github_login.clone()) {
                ret.insert(id, github_login.into());
            } else {
                missing_user_ids.push(id)
            }
        }
        if !missing_user_ids.is_empty() {
            let this = self.weak_self.clone();
            cx.spawn(async move |cx| {
                this.update(cx, |this, cx| this.get_users(missing_user_ids, cx))?
                    .await
            })
            .detach_and_log_err(cx);
        }
        ret
    }
}

impl User {
    fn new(message: proto::User) -> Arc<Self> {
        Arc::new(User {
            id: message.id,
            github_login: message.github_login,
            avatar_uri: message.avatar_url.into(),
            name: message.name,
            email: message.email,
        })
    }
}

impl Collaborator {
    pub fn from_proto(message: proto::Collaborator) -> Result<Self> {
        Ok(Self {
            peer_id: message.peer_id.context("invalid peer id")?,
            replica_id: message.replica_id as ReplicaId,
            user_id: message.user_id as UserId,
            is_host: message.is_host,
        })
    }
}
