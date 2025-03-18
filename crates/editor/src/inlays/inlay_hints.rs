use std::{
    collections::hash_map,
    ops::{ControlFlow, Range},
    time::Duration,
};

use clock::Global;
use collections::{HashMap, HashSet};
use futures::future::join_all;
use gpui::{App, Entity, Task};
use language::{
    BufferRow,
    language_settings::{InlayHintKind, InlayHintSettings, language_settings},
};
use lsp::LanguageServerId;
use multi_buffer::{Anchor, ExcerptId, MultiBufferSnapshot};
use project::{
    HoverBlock, HoverBlockKind, InlayHintLabel, InlayHintLabelPartTooltip, InlayHintTooltip,
    InvalidationStrategy, ResolveState,
    lsp_store::{CacheInlayHints, ResolvedHint},
};
use text::{Bias, BufferId};
use ui::{Context, Window};
use util::debug_panic;

use super::{Inlay, InlayId};
use crate::{
    Editor, EditorSnapshot, PointForPosition, ToggleInlayHints, ToggleInlineValues, debounce_value,
    hover_links::{InlayHighlight, TriggerPoint, show_link_definition},
    hover_popover::{self, InlayHover},
    inlays::InlaySplice,
};

pub fn inlay_hint_settings(
    location: Anchor,
    snapshot: &MultiBufferSnapshot,
    cx: &mut Context<Editor>,
) -> InlayHintSettings {
    let file = snapshot.file_at(location);
    let language = snapshot.language_at(location).map(|l| l.name());
    language_settings(language, file, cx).inlay_hints
}

#[derive(Debug)]
pub struct LspInlayHintData {
    enabled: bool,
    modifiers_override: bool,
    enabled_in_settings: bool,
    allowed_hint_kinds: HashSet<Option<InlayHintKind>>,
    invalidate_debounce: Option<Duration>,
    append_debounce: Option<Duration>,
    hint_refresh_tasks: HashMap<BufferId, HashMap<Vec<Range<BufferRow>>, Vec<Task<()>>>>,
    hint_chunk_fetched: HashMap<BufferId, (Global, HashSet<Range<BufferRow>>)>,
    invalidate_hints_for_buffers: HashSet<BufferId>,
    pub added_hints: HashMap<InlayId, Option<InlayHintKind>>,
}

impl LspInlayHintData {
    pub fn new(settings: InlayHintSettings) -> Self {
        Self {
            modifiers_override: false,
            enabled: settings.enabled,
            enabled_in_settings: settings.enabled,
            hint_refresh_tasks: HashMap::default(),
            added_hints: HashMap::default(),
            hint_chunk_fetched: HashMap::default(),
            invalidate_hints_for_buffers: HashSet::default(),
            invalidate_debounce: debounce_value(settings.edit_debounce_ms),
            append_debounce: debounce_value(settings.scroll_debounce_ms),
            allowed_hint_kinds: settings.enabled_inlay_hint_kinds(),
        }
    }

    pub fn modifiers_override(&mut self, new_override: bool) -> Option<bool> {
        if self.modifiers_override == new_override {
            return None;
        }
        self.modifiers_override = new_override;
        if (self.enabled && self.modifiers_override) || (!self.enabled && !self.modifiers_override)
        {
            self.clear();
            Some(false)
        } else {
            Some(true)
        }
    }

    pub fn toggle(&mut self, enabled: bool) -> bool {
        if self.enabled == enabled {
            return false;
        }
        self.enabled = enabled;
        self.modifiers_override = false;
        if !enabled {
            self.clear();
        }
        true
    }

    pub fn clear(&mut self) {
        self.hint_refresh_tasks.clear();
        self.hint_chunk_fetched.clear();
        self.added_hints.clear();
        self.invalidate_hints_for_buffers.clear();
    }

    /// Checks inlay hint settings for enabled hint kinds and general enabled state.
    /// Generates corresponding inlay_map splice updates on settings changes.
    /// Does not update inlay hint cache state on disabling or inlay hint kinds change: only reenabling forces new LSP queries.
    fn update_settings(
        &mut self,
        new_hint_settings: InlayHintSettings,
        visible_hints: Vec<Inlay>,
    ) -> ControlFlow<Option<InlaySplice>, Option<InlaySplice>> {
        let old_enabled = self.enabled;
        // If the setting for inlay hints has changed, update `enabled`. This condition avoids inlay
        // hint visibility changes when other settings change (such as theme).
        //
        // Another option might be to store whether the user has manually toggled inlay hint
        // visibility, and prefer this. This could lead to confusion as it means inlay hint
        // visibility would not change when updating the setting if they were ever toggled.
        if new_hint_settings.enabled != self.enabled_in_settings {
            self.enabled = new_hint_settings.enabled;
            self.enabled_in_settings = new_hint_settings.enabled;
            self.modifiers_override = false;
        };
        self.invalidate_debounce = debounce_value(new_hint_settings.edit_debounce_ms);
        self.append_debounce = debounce_value(new_hint_settings.scroll_debounce_ms);
        let new_allowed_hint_kinds = new_hint_settings.enabled_inlay_hint_kinds();
        match (old_enabled, self.enabled) {
            (false, false) => {
                self.allowed_hint_kinds = new_allowed_hint_kinds;
                ControlFlow::Break(None)
            }
            (true, true) => {
                if new_allowed_hint_kinds == self.allowed_hint_kinds {
                    ControlFlow::Break(None)
                } else {
                    self.allowed_hint_kinds = new_allowed_hint_kinds;
                    ControlFlow::Continue(
                        Some(InlaySplice {
                            to_remove: visible_hints
                                .iter()
                                .filter_map(|inlay| {
                                    let inlay_kind = self.added_hints.get(&inlay.id).copied()?;
                                    if !self.allowed_hint_kinds.contains(&inlay_kind) {
                                        Some(inlay.id)
                                    } else {
                                        None
                                    }
                                })
                                .collect(),
                            to_insert: Vec::new(),
                        })
                        .filter(|splice| !splice.is_empty()),
                    )
                }
            }
            (true, false) => {
                self.modifiers_override = false;
                self.allowed_hint_kinds = new_allowed_hint_kinds;
                if visible_hints.is_empty() {
                    ControlFlow::Break(None)
                } else {
                    self.clear();
                    ControlFlow::Break(Some(InlaySplice {
                        to_remove: visible_hints.iter().map(|inlay| inlay.id).collect(),
                        to_insert: Vec::new(),
                    }))
                }
            }
            (false, true) => {
                self.modifiers_override = false;
                self.allowed_hint_kinds = new_allowed_hint_kinds;
                ControlFlow::Continue(
                    Some(InlaySplice {
                        to_remove: visible_hints
                            .iter()
                            .filter_map(|inlay| {
                                let inlay_kind = self.added_hints.get(&inlay.id).copied()?;
                                if !self.allowed_hint_kinds.contains(&inlay_kind) {
                                    Some(inlay.id)
                                } else {
                                    None
                                }
                            })
                            .collect(),
                        to_insert: Vec::new(),
                    })
                    .filter(|splice| !splice.is_empty()),
                )
            }
        }
    }

    pub(crate) fn remove_inlay_chunk_data<'a>(
        &'a mut self,
        removed_buffer_ids: impl IntoIterator<Item = &'a BufferId> + 'a,
    ) {
        for buffer_id in removed_buffer_ids {
            self.hint_refresh_tasks.remove(buffer_id);
            self.hint_chunk_fetched.remove(buffer_id);
        }
    }
}

#[derive(Debug, Clone)]
pub enum InlayHintRefreshReason {
    ModifiersChanged(bool),
    Toggle(bool),
    SettingsChange(InlayHintSettings),
    NewLinesShown,
    BufferEdited(BufferId),
    RefreshRequested(LanguageServerId),
    ExcerptsRemoved(Vec<ExcerptId>),
}

impl Editor {
    pub fn supports_inlay_hints(&self, cx: &mut App) -> bool {
        let Some(provider) = self.semantics_provider.as_ref() else {
            return false;
        };

        let mut supports = false;
        self.buffer().update(cx, |this, cx| {
            this.for_each_buffer(|buffer| {
                supports |= provider.supports_inlay_hints(buffer, cx);
            });
        });

        supports
    }

    pub fn toggle_inline_values(
        &mut self,
        _: &ToggleInlineValues,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.inline_value_cache.enabled = !self.inline_value_cache.enabled;

        self.refresh_inline_values(cx);
    }

    pub fn toggle_inlay_hints(
        &mut self,
        _: &ToggleInlayHints,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.refresh_inlay_hints(
            InlayHintRefreshReason::Toggle(!self.inlay_hints_enabled()),
            cx,
        );
    }

    pub fn inlay_hints_enabled(&self) -> bool {
        self.inlay_hints.as_ref().is_some_and(|cache| cache.enabled)
    }

    /// Updates inlay hints for the visible ranges of the singleton buffer(s).
    /// Based on its parameters, either invalidates the previous data, or appends to it.
    pub(crate) fn refresh_inlay_hints(
        &mut self,
        reason: InlayHintRefreshReason,
        cx: &mut Context<Self>,
    ) {
        if !self.mode.is_full() || self.inlay_hints.is_none() {
            return;
        }
        let Some(semantics_provider) = self.semantics_provider() else {
            return;
        };
        let Some(invalidate_cache) = self.refresh_editor_data(&reason, cx) else {
            return;
        };

        let debounce = match &reason {
            InlayHintRefreshReason::SettingsChange(_)
            | InlayHintRefreshReason::Toggle(_)
            | InlayHintRefreshReason::ExcerptsRemoved(_)
            | InlayHintRefreshReason::ModifiersChanged(_) => None,
            _may_need_lsp_call => self.inlay_hints.as_ref().and_then(|inlay_hints| {
                if invalidate_cache.should_invalidate() {
                    inlay_hints.invalidate_debounce
                } else {
                    inlay_hints.append_debounce
                }
            }),
        };

        let mut visible_excerpts = self.visible_excerpts(cx);
        let mut invalidate_hints_for_buffers = HashSet::default();
        let ignore_previous_fetches = match reason {
            InlayHintRefreshReason::ModifiersChanged(_)
            | InlayHintRefreshReason::Toggle(_)
            | InlayHintRefreshReason::SettingsChange(_) => true,
            InlayHintRefreshReason::NewLinesShown
            | InlayHintRefreshReason::RefreshRequested(_)
            | InlayHintRefreshReason::ExcerptsRemoved(_) => false,
            InlayHintRefreshReason::BufferEdited(buffer_id) => {
                let Some(affected_language) = self
                    .buffer()
                    .read(cx)
                    .buffer(buffer_id)
                    .and_then(|buffer| buffer.read(cx).language().cloned())
                else {
                    return;
                };

                invalidate_hints_for_buffers.extend(
                    self.buffer()
                        .read(cx)
                        .all_buffers()
                        .into_iter()
                        .filter_map(|buffer| {
                            let buffer = buffer.read(cx);
                            if buffer.language() == Some(&affected_language) {
                                Some(buffer.remote_id())
                            } else {
                                None
                            }
                        }),
                );

                semantics_provider.invalidate_inlay_hints(&invalidate_hints_for_buffers, cx);
                visible_excerpts.retain(|_, (visible_buffer, _, _)| {
                    visible_buffer.read(cx).language() == Some(&affected_language)
                });
                false
            }
        };

        let multi_buffer = self.buffer().clone();
        let Some(inlay_hints) = self.inlay_hints.as_mut() else {
            return;
        };

        if invalidate_cache.should_invalidate() {
            inlay_hints.clear();
        }
        inlay_hints
            .invalidate_hints_for_buffers
            .extend(invalidate_hints_for_buffers);

        let mut buffers_to_query = HashMap::default();
        for (_, (buffer, buffer_version, visible_range)) in visible_excerpts {
            let buffer_id = buffer.read(cx).remote_id();
            if !self.registered_buffers.contains_key(&buffer_id) {
                continue;
            }

            let buffer_snapshot = buffer.read(cx).snapshot();
            let buffer_anchor_range = buffer_snapshot.anchor_before(visible_range.start)
                ..buffer_snapshot.anchor_after(visible_range.end);

            let visible_excerpts =
                buffers_to_query
                    .entry(buffer_id)
                    .or_insert_with(|| VisibleExcerpts {
                        ranges: Vec::new(),
                        buffer_version: buffer_version.clone(),
                        buffer: buffer.clone(),
                    });
            visible_excerpts.buffer_version = buffer_version;
            visible_excerpts.ranges.push(buffer_anchor_range);
        }

        for (buffer_id, visible_excerpts) in buffers_to_query {
            let Some(buffer) = multi_buffer.read(cx).buffer(buffer_id) else {
                continue;
            };
            let fetched_tasks = inlay_hints.hint_chunk_fetched.entry(buffer_id).or_default();
            if visible_excerpts
                .buffer_version
                .changed_since(&fetched_tasks.0)
            {
                fetched_tasks.1.clear();
                fetched_tasks.0 = visible_excerpts.buffer_version.clone();
                inlay_hints.hint_refresh_tasks.remove(&buffer_id);
            }

            let applicable_chunks =
                semantics_provider.applicable_inlay_chunks(&buffer, &visible_excerpts.ranges, cx);

            match inlay_hints
                .hint_refresh_tasks
                .entry(buffer_id)
                .or_default()
                .entry(applicable_chunks)
            {
                hash_map::Entry::Occupied(mut o) => {
                    if invalidate_cache.should_invalidate() || ignore_previous_fetches {
                        o.get_mut().push(spawn_editor_hints_refresh(
                            buffer_id,
                            invalidate_cache,
                            ignore_previous_fetches,
                            debounce,
                            visible_excerpts,
                            cx,
                        ));
                    }
                }
                hash_map::Entry::Vacant(v) => {
                    v.insert(Vec::new()).push(spawn_editor_hints_refresh(
                        buffer_id,
                        invalidate_cache,
                        ignore_previous_fetches,
                        debounce,
                        visible_excerpts,
                        cx,
                    ));
                }
            }
        }
    }

    pub fn clear_inlay_hints(&mut self, cx: &mut Context<Self>) {
        let to_remove = self
            .visible_inlay_hints(cx)
            .into_iter()
            .map(|inlay| {
                let inlay_id = inlay.id;
                if let Some(inlay_hints) = &mut self.inlay_hints {
                    inlay_hints.added_hints.remove(&inlay_id);
                }
                inlay_id
            })
            .collect::<Vec<_>>();
        self.splice_inlays(&to_remove, Vec::new(), cx);
    }

    fn refresh_editor_data(
        &mut self,
        reason: &InlayHintRefreshReason,
        cx: &mut Context<'_, Editor>,
    ) -> Option<InvalidationStrategy> {
        let visible_inlay_hints = self.visible_inlay_hints(cx);
        let Some(inlay_hints) = self.inlay_hints.as_mut() else {
            return None;
        };

        let invalidate_cache = match reason {
            InlayHintRefreshReason::ModifiersChanged(enabled) => {
                match inlay_hints.modifiers_override(*enabled) {
                    Some(enabled) => {
                        if enabled {
                            InvalidationStrategy::None
                        } else {
                            self.clear_inlay_hints(cx);
                            return None;
                        }
                    }
                    None => return None,
                }
            }
            InlayHintRefreshReason::Toggle(enabled) => {
                if inlay_hints.toggle(*enabled) {
                    if *enabled {
                        InvalidationStrategy::None
                    } else {
                        self.clear_inlay_hints(cx);
                        return None;
                    }
                } else {
                    return None;
                }
            }
            InlayHintRefreshReason::SettingsChange(new_settings) => {
                match inlay_hints.update_settings(*new_settings, visible_inlay_hints) {
                    ControlFlow::Break(Some(InlaySplice {
                        to_remove,
                        to_insert,
                    })) => {
                        self.splice_inlays(&to_remove, to_insert, cx);
                        return None;
                    }
                    ControlFlow::Break(None) => return None,
                    ControlFlow::Continue(splice) => {
                        if let Some(InlaySplice {
                            to_remove,
                            to_insert,
                        }) = splice
                        {
                            self.splice_inlays(&to_remove, to_insert, cx);
                        }
                        InvalidationStrategy::None
                    }
                }
            }
            InlayHintRefreshReason::ExcerptsRemoved(excerpts_removed) => {
                let to_remove = self
                    .display_map
                    .read(cx)
                    .current_inlays()
                    .filter_map(|inlay| {
                        if excerpts_removed.contains(&inlay.position.excerpt_id) {
                            Some(inlay.id)
                        } else {
                            None
                        }
                    })
                    .collect::<Vec<_>>();
                self.splice_inlays(&to_remove, Vec::new(), cx);
                return None;
            }
            InlayHintRefreshReason::NewLinesShown => InvalidationStrategy::None,
            InlayHintRefreshReason::BufferEdited(_) => InvalidationStrategy::BufferEdited,
            InlayHintRefreshReason::RefreshRequested(server_id) => {
                InvalidationStrategy::RefreshRequested(*server_id)
            }
        };

        match &mut self.inlay_hints {
            Some(inlay_hints) => {
                if !inlay_hints.enabled
                    && !matches!(reason, InlayHintRefreshReason::ModifiersChanged(_))
                {
                    return None;
                }
            }
            None => return None,
        }

        Some(invalidate_cache)
    }

    pub(crate) fn visible_inlay_hints(&self, cx: &Context<Editor>) -> Vec<Inlay> {
        self.display_map
            .read(cx)
            .current_inlays()
            .filter(move |inlay| matches!(inlay.id, InlayId::Hint(_)))
            .cloned()
            .collect()
    }

    pub fn update_inlay_link_and_hover_points(
        &mut self,
        snapshot: &EditorSnapshot,
        point_for_position: PointForPosition,
        secondary_held: bool,
        shift_held: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(lsp_store) = self.project().map(|project| project.read(cx).lsp_store()) else {
            return;
        };
        let hovered_offset = if point_for_position.column_overshoot_after_line_end == 0 {
            Some(
                snapshot
                    .display_point_to_inlay_offset(point_for_position.exact_unclipped, Bias::Left),
            )
        } else {
            None
        };
        let mut go_to_definition_updated = false;
        let mut hover_updated = false;
        if let Some(hovered_offset) = hovered_offset {
            let buffer_snapshot = self.buffer().read(cx).snapshot(cx);
            let previous_valid_anchor = buffer_snapshot.anchor_at(
                point_for_position.previous_valid.to_point(snapshot),
                Bias::Left,
            );
            let next_valid_anchor = buffer_snapshot.anchor_at(
                point_for_position.next_valid.to_point(snapshot),
                Bias::Right,
            );
            if let Some(hovered_hint) = self
                .visible_inlay_hints(cx)
                .into_iter()
                .skip_while(|hint| {
                    hint.position
                        .cmp(&previous_valid_anchor, &buffer_snapshot)
                        .is_lt()
                })
                .take_while(|hint| {
                    hint.position
                        .cmp(&next_valid_anchor, &buffer_snapshot)
                        .is_le()
                })
                .max_by_key(|hint| hint.id)
            {
                if let Some(ResolvedHint::Resolved(cached_hint)) =
                    hovered_hint.position.buffer_id.and_then(|buffer_id| {
                        lsp_store.update(cx, |lsp_store, cx| {
                            lsp_store.resolved_hint(buffer_id, hovered_hint.id, cx)
                        })
                    })
                {
                    match cached_hint.resolve_state {
                        ResolveState::Resolved => {
                            let mut extra_shift_left = 0;
                            let mut extra_shift_right = 0;
                            if cached_hint.padding_left {
                                extra_shift_left += 1;
                                extra_shift_right += 1;
                            }
                            if cached_hint.padding_right {
                                extra_shift_right += 1;
                            }
                            match cached_hint.label {
                                InlayHintLabel::String(_) => {
                                    if let Some(tooltip) = cached_hint.tooltip {
                                        hover_popover::hover_at_inlay(
                                            self,
                                            InlayHover {
                                                tooltip: match tooltip {
                                                    InlayHintTooltip::String(text) => HoverBlock {
                                                        text,
                                                        kind: HoverBlockKind::PlainText,
                                                    },
                                                    InlayHintTooltip::MarkupContent(content) => {
                                                        HoverBlock {
                                                            text: content.value,
                                                            kind: content.kind,
                                                        }
                                                    }
                                                },
                                                range: InlayHighlight {
                                                    inlay: hovered_hint.id,
                                                    inlay_position: hovered_hint.position,
                                                    range: extra_shift_left
                                                        ..hovered_hint.text().len()
                                                            + extra_shift_right,
                                                },
                                            },
                                            window,
                                            cx,
                                        );
                                        hover_updated = true;
                                    }
                                }
                                InlayHintLabel::LabelParts(label_parts) => {
                                    let hint_start =
                                        snapshot.anchor_to_inlay_offset(hovered_hint.position);
                                    if let Some((hovered_hint_part, part_range)) =
                                        hover_popover::find_hovered_hint_part(
                                            label_parts,
                                            hint_start,
                                            hovered_offset,
                                        )
                                    {
                                        let highlight_start =
                                            (part_range.start - hint_start).0 + extra_shift_left;
                                        let highlight_end =
                                            (part_range.end - hint_start).0 + extra_shift_right;
                                        let highlight = InlayHighlight {
                                            inlay: hovered_hint.id,
                                            inlay_position: hovered_hint.position,
                                            range: highlight_start..highlight_end,
                                        };
                                        if let Some(tooltip) = hovered_hint_part.tooltip {
                                            hover_popover::hover_at_inlay(
                                                self,
                                                InlayHover {
                                                    tooltip: match tooltip {
                                                        InlayHintLabelPartTooltip::String(text) => {
                                                            HoverBlock {
                                                                text,
                                                                kind: HoverBlockKind::PlainText,
                                                            }
                                                        }
                                                        InlayHintLabelPartTooltip::MarkupContent(
                                                            content,
                                                        ) => HoverBlock {
                                                            text: content.value,
                                                            kind: content.kind,
                                                        },
                                                    },
                                                    range: highlight.clone(),
                                                },
                                                window,
                                                cx,
                                            );
                                            hover_updated = true;
                                        }
                                        if let Some((language_server_id, location)) =
                                            hovered_hint_part.location
                                            && secondary_held
                                            && !self.has_pending_nonempty_selection()
                                        {
                                            go_to_definition_updated = true;
                                            show_link_definition(
                                                shift_held,
                                                self,
                                                TriggerPoint::InlayHint(
                                                    highlight,
                                                    location,
                                                    language_server_id,
                                                ),
                                                snapshot,
                                                window,
                                                cx,
                                            );
                                        }
                                    }
                                }
                            };
                        }
                        ResolveState::CanResolve(_, _) => debug_panic!(
                            "Expected resolved_hint retrieval to return a resolved hint"
                        ),
                        ResolveState::Resolving => {}
                    }
                }
            }
        }

        if !go_to_definition_updated {
            self.hide_hovered_link(cx)
        }
        if !hover_updated {
            hover_popover::hover_at(self, None, window, cx);
        }
    }

    fn inlay_hints_for_buffer(
        &mut self,
        invalidate_cache: InvalidationStrategy,
        ignore_previous_fetches: bool,
        buffer_excerpts: VisibleExcerpts,
        cx: &mut Context<Self>,
    ) -> Option<Vec<Task<(Range<BufferRow>, anyhow::Result<CacheInlayHints>)>>> {
        let semantics_provider = self.semantics_provider()?;
        let inlay_hints = self.inlay_hints.as_mut()?;
        let buffer_id = buffer_excerpts.buffer.read(cx).remote_id();

        let new_hint_tasks = semantics_provider
            .inlay_hints(
                invalidate_cache,
                buffer_excerpts.buffer,
                buffer_excerpts.ranges,
                inlay_hints
                    .hint_chunk_fetched
                    .get(&buffer_id)
                    .filter(|_| !ignore_previous_fetches && !invalidate_cache.should_invalidate())
                    .cloned(),
                cx,
            )
            .unwrap_or_default();

        let (known_version, known_chunks) =
            inlay_hints.hint_chunk_fetched.entry(buffer_id).or_default();
        if buffer_excerpts.buffer_version.changed_since(known_version) {
            known_chunks.clear();
            *known_version = buffer_excerpts.buffer_version;
        }

        let mut hint_tasks = Vec::new();
        for (row_range, new_hints_task) in new_hint_tasks {
            let inserted = known_chunks.insert(row_range.clone());
            if inserted || ignore_previous_fetches || invalidate_cache.should_invalidate() {
                hint_tasks.push(cx.spawn(async move |_, _| (row_range, new_hints_task.await)));
            }
        }

        Some(hint_tasks)
    }

    fn apply_fetched_hints(
        &mut self,
        buffer_id: BufferId,
        query_version: Global,
        invalidate_cache: InvalidationStrategy,
        new_hints: Vec<(Range<BufferRow>, anyhow::Result<CacheInlayHints>)>,
        cx: &mut Context<Self>,
    ) {
        let visible_inlay_hint_ids = self
            .visible_inlay_hints(cx)
            .iter()
            .filter(|inlay| inlay.position.buffer_id == Some(buffer_id))
            .map(|inlay| inlay.id)
            .collect::<Vec<_>>();
        let Some(inlay_hints) = &mut self.inlay_hints else {
            return;
        };

        let mut hints_to_remove = Vec::new();
        let multi_buffer_snapshot = self.buffer.read(cx).snapshot(cx);

        // If we've received hints from the cache, it means `invalidate_cache` had invalidated whatever possible there,
        // and most probably there are no more hints with IDs from `visible_inlay_hint_ids` in the cache.
        // So, if we hover such hints, no resolve will happen.
        //
        // Another issue is in the fact that changing one buffer may lead to other buffers' hints changing, so more cache entries may be removed.
        // Hence, clear all excerpts' hints in the multi buffer: later, the invalidated ones will re-trigger the LSP query, the rest will be restored
        // from the cache.
        if invalidate_cache.should_invalidate() {
            hints_to_remove.extend(visible_inlay_hint_ids);
        }

        let excerpts = self.buffer.read(cx).excerpt_ids();
        let hints_to_insert = new_hints
            .into_iter()
            .filter_map(|(chunk_range, hints_result)| match hints_result {
                Ok(new_hints) => Some(new_hints),
                Err(e) => {
                    log::error!(
                        "Failed to query inlays for buffer row range {chunk_range:?}, {e:#}"
                    );
                    if let Some((for_version, chunks_fetched)) =
                        inlay_hints.hint_chunk_fetched.get_mut(&buffer_id)
                    {
                        if for_version == &query_version {
                            chunks_fetched.remove(&chunk_range);
                        }
                    }
                    None
                }
            })
            .flat_map(|hints| hints.into_values())
            .flatten()
            .filter_map(|(hint_id, lsp_hint)| {
                if inlay_hints.allowed_hint_kinds.contains(&lsp_hint.kind)
                    && inlay_hints
                        .added_hints
                        .insert(hint_id, lsp_hint.kind)
                        .is_none()
                {
                    let position = excerpts.iter().find_map(|excerpt_id| {
                        multi_buffer_snapshot.anchor_in_excerpt(*excerpt_id, lsp_hint.position)
                    })?;
                    return Some(Inlay::hint(hint_id, position, &lsp_hint));
                }
                None
            })
            .collect::<Vec<_>>();

        let invalidate_hints_for_buffers =
            std::mem::take(&mut inlay_hints.invalidate_hints_for_buffers);
        if !invalidate_hints_for_buffers.is_empty() {
            hints_to_remove.extend(
                self.visible_inlay_hints(cx)
                    .iter()
                    .filter(|inlay| {
                        inlay.position.buffer_id.is_none_or(|buffer_id| {
                            invalidate_hints_for_buffers.contains(&buffer_id)
                        })
                    })
                    .map(|inlay| inlay.id),
            );
        }

        self.splice_inlays(&hints_to_remove, hints_to_insert, cx);
    }
}

#[derive(Debug)]
struct VisibleExcerpts {
    ranges: Vec<Range<text::Anchor>>,
    buffer_version: Global,
    buffer: Entity<language::Buffer>,
}

fn spawn_editor_hints_refresh(
    buffer_id: BufferId,
    invalidate_cache: InvalidationStrategy,
    ignore_previous_fetches: bool,
    debounce: Option<Duration>,
    buffer_excerpts: VisibleExcerpts,
    cx: &mut Context<'_, Editor>,
) -> Task<()> {
    cx.spawn(async move |editor, cx| {
        if let Some(debounce) = debounce {
            cx.background_executor().timer(debounce).await;
        }

        let query_version = buffer_excerpts.buffer_version.clone();
        let Some(hint_tasks) = editor
            .update(cx, |editor, cx| {
                editor.inlay_hints_for_buffer(
                    invalidate_cache,
                    ignore_previous_fetches,
                    buffer_excerpts,
                    cx,
                )
            })
            .ok()
        else {
            return;
        };
        let hint_tasks = hint_tasks.unwrap_or_default();
        if hint_tasks.is_empty() {
            return;
        }
        let new_hints = join_all(hint_tasks).await;
        editor
            .update(cx, |editor, cx| {
                editor.apply_fetched_hints(
                    buffer_id,
                    query_version,
                    invalidate_cache,
                    new_hints,
                    cx,
                );
            })
            .ok();
    })
}
