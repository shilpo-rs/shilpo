//! Pure card state machine: types, two-channel state, and reducer.
//!
//! This module is intentionally free of GPUI runtime dependencies so that all
//! transition logic can be covered by plain `#[test]` cases without a display
//! server or app context.

use std::collections::HashMap;

use gpui::{Bounds, DisplayId, Pixels, SharedString};

// ────────────────────────────────────────────────────────────────
// Core identity / configuration types
// ────────────────────────────────────────────────────────────────

/// Stable identity key for a card provider (e.g. `"battery"`, `"workspaces"`).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct CardOwnerId(pub SharedString);

impl std::fmt::Display for CardOwnerId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

impl CardOwnerId {
    #[allow(
        dead_code,
        reason = "constructed by built-in providers added after the coordinator foundation"
    )]
    pub fn new(id: impl Into<SharedString>) -> Self {
        Self(id.into())
    }
}

impl From<&str> for CardOwnerId {
    fn from(s: &str) -> Self {
        Self(SharedString::from(s.to_string()))
    }
}

impl From<String> for CardOwnerId {
    fn from(s: String) -> Self {
        Self(SharedString::from(s))
    }
}

impl From<SharedString> for CardOwnerId {
    fn from(s: SharedString) -> Self {
        Self(s)
    }
}

impl From<shilpo_ext_api::CanonicalId> for CardOwnerId {
    fn from(id: shilpo_ext_api::CanonicalId) -> Self {
        Self(SharedString::from(id.to_string()))
    }
}

impl From<&shilpo_ext_api::CanonicalId> for CardOwnerId {
    fn from(id: &shilpo_ext_api::CanonicalId) -> Self {
        Self(SharedString::from(id.to_string()))
    }
}

/// Fully qualified identity key for a rendered card source instance.
///
/// Separates provider ownership (`owner`) from a specific rendered instance (`instance_id`)
/// and its optional content discriminator (`content_key`).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct CardSourceId {
    pub owner: CardOwnerId,
    pub instance_id: SharedString,
    pub content_key: Option<SharedString>,
}

impl CardSourceId {
    pub fn singleton(owner: impl Into<CardOwnerId>) -> Self {
        let owner = owner.into();
        let instance_id = owner.0.clone();
        Self {
            owner,
            instance_id,
            content_key: None,
        }
    }

    pub fn new(
        owner: impl Into<CardOwnerId>,
        instance_id: impl Into<SharedString>,
        content_key: Option<impl Into<SharedString>>,
    ) -> Self {
        Self {
            owner: owner.into(),
            instance_id: instance_id.into(),
            content_key: content_key.map(Into::into),
        }
    }
}

impl std::fmt::Display for CardSourceId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if let Some(key) = &self.content_key {
            write!(f, "{}:{}:{}", self.owner, self.instance_id, key)
        } else {
            write!(f, "{}:{}", self.owner, self.instance_id)
        }
    }
}

/// Which surface channel carries a card.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CardChannel {
    /// User-clicked card — accepts keyboard focus, globally exclusive.
    Persistent,
    /// Hover-preview card — non-interactive, globally exclusive.
    Preview,
}

/// Independently declared interaction capabilities for a provider.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CardCapabilities {
    /// Widget participates in hover-preview.
    pub hover: bool,
    /// Widget participates in persistent-click.
    pub click: bool,
    /// Whether opening this card as a persistent channel should capture keyboard focus.
    /// A card with no focusable content (no text input) has nothing to gain from grabbing
    /// focus, and doing so anyway has a real cost: moving keyboard/activation focus away
    /// from the bar without an accompanying pointer-move event leaves the bar's own cursor
    /// styling stuck until the next real mouse movement recomputes it, which reads as the
    /// pointer icon getting stuck on "default" right after a click. Cards that do need
    /// focus (anything with a `TextInput`, or that wants Escape-to-dismiss/click-outside
    /// semantics with actual keyboard interaction inside) should set this `true`.
    pub needs_focus: bool,
}

/// Maximum logical-pixel card dimensions per tier.
///
/// Content may shrink below its tier maximum. All dimensions are later
/// clamped to available monitor space by the placement engine.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
#[allow(
    dead_code,
    reason = "all tiers are part of the built-in provider contract"
)]
pub enum CardSizeTier {
    /// 280 × 240
    Compact,
    /// 360 × 280 — desktop cards with a wide summary and short detail list.
    WideCompact,
    /// 360 × 480
    #[default]
    Standard,
    /// 480 × 640
    Expanded,
}

impl CardSizeTier {
    /// Maximum width in logical pixels for this tier.
    pub const fn max_width(self) -> f32 {
        match self {
            CardSizeTier::Compact => 280.0,
            CardSizeTier::WideCompact => 360.0,
            CardSizeTier::Standard => 360.0,
            CardSizeTier::Expanded => 480.0,
        }
    }

    /// Maximum height in logical pixels for this tier.
    pub const fn max_height(self) -> f32 {
        match self {
            CardSizeTier::Compact => 240.0,
            CardSizeTier::WideCompact => 280.0,
            CardSizeTier::Standard => 480.0,
            CardSizeTier::Expanded => 640.0,
        }
    }
}

/// Source-widget state visible to later widget rendering code.
///
/// Widgets can use this to render their icon/text/ghost-button with the
/// correct selection / active cues without knowing placement internals.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum CardSourceState {
    #[default]
    Idle,
    HoverPending,
    PreviewOpen,
    PersistentOpen,
}

/// Why a channel was dismissed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CardDismissReason {
    SourceToggle,
    Escape,
    FocusLost,
    #[allow(dead_code)]
    OutsideClick,
    OverviewOpened,
    BarClosed,
    DisplayRemoved,
    Shutdown,
    OwnerRemoved,
    SourceDisappeared,
    Explicit,
}

// ────────────────────────────────────────────────────────────────
// State machine input
// ────────────────────────────────────────────────────────────────

/// Semantic events the adapter dispatches into the state machine.
#[derive(Clone, Debug, PartialEq)]
#[allow(
    dead_code,
    reason = "source events are the integration seam for follow-up widget providers"
)]
pub enum CardRequest {
    // ── pointer / source events ──────────────────────────────────
    SourceEnter {
        source: CardSourceId,
    },
    SourceLeave {
        source: CardSourceId,
    },
    /// Pointer left a source group such as the complete workspace pill.
    SourceGroupLeave {
        owner: CardOwnerId,
        instance_id: SharedString,
    },
    /// Pointer entered a source group; cancels dismissal while traversing
    /// between the group and its preview surface.
    SourceGroupEnter {
        owner: CardOwnerId,
        instance_id: SharedString,
    },
    PreviewEnter {
        source: CardSourceId,
    },
    PreviewLeave {
        source: CardSourceId,
    },
    PersistentToggle {
        source: CardSourceId,
    },
    PersistentToggleAt {
        source: CardSourceId,
        bounds: Bounds<Pixels>,
        display_id: DisplayId,
    },

    // ── anchor geometry update ───────────────────────────────────
    AnchorUpdate {
        source: CardSourceId,
        bounds: Bounds<Pixels>,
        display_id: DisplayId,
    },
    AnchorRemoved {
        source: CardSourceId,
    },
    PlacementUpdated {
        source: CardSourceId,
        channel: CardChannel,
        bounds: Bounds<Pixels>,
        display_id: DisplayId,
    },
    Reposition {
        source: CardSourceId,
    },

    // ── system lifecycle ─────────────────────────────────────────
    Dismiss {
        channel: CardChannel,
        reason: CardDismissReason,
    },
    DisplayRemoved {
        display_id: DisplayId,
    },
    OwnerRemoved {
        owner: CardOwnerId,
    },
    OverviewOpened,
    BarClosed,
    Shutdown,

    // ── timer completions (generation guarded) ───────────────────
    PreviewOpenTimer {
        generation: u64,
    },
    PreviewCloseTimer {
        generation: u64,
    },
    CloseAnimationFinished {
        channel: CardChannel,
        generation: u64,
    },
}

// ────────────────────────────────────────────────────────────────
// Pure side-effects emitted by the reducer
// ────────────────────────────────────────────────────────────────

/// Signals the GPUI adapter must execute after each `CardState::reduce` call.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CardEffect {
    OpenChannel {
        channel: CardChannel,
        source: CardSourceId,
        generation: u64,
    },
    CloseChannel {
        channel: CardChannel,
        reason: CardDismissReason,
        display_id: Option<DisplayId>,
        generation: u64,
        source: Option<CardSourceId>,
    },
    RepositionChannel {
        channel: CardChannel,
        source: CardSourceId,
    },
    /// Replace and move an already-visible preview without an exit/enter cycle.
    RetargetChannel {
        channel: CardChannel,
        source: CardSourceId,
        generation: u64,
    },
    StartTimer {
        kind: TimerKind,
        generation: u64,
    },
    CancelTimers {
        kind: TimerKind,
    },
    CaptureFocus,
    RestoreFocus,
    UpdateBarHold {
        hold: bool,
    },
    Diagnostic(CardDiagnostic),
}

/// Which timer class to start or cancel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TimerKind {
    PreviewOpen,
    PreviewClose,
}

/// Structured diagnostic payload (never contains sensitive domain values).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CardDiagnostic {
    pub kind: DiagnosticKind,
    pub source: Option<CardSourceId>,
    pub channel: Option<CardChannel>,
    pub generation: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DiagnosticKind {
    ChannelOpened,
    ChannelClosed,
    TimerStarted,
    TimerStale,
    FocusCaptured,
    FocusRestored,
}

// ────────────────────────────────────────────────────────────────
// Channel slot
// ────────────────────────────────────────────────────────────────

/// Lifecycle of a single channel slot.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum ChannelLifecycle {
    #[default]
    Closed,
    Open,
    Closing,
}

/// State for one channel (persistent or preview).
#[derive(Clone, Debug, Default)]
pub struct ChannelSlot {
    pub source: Option<CardSourceId>,
    pub lifecycle: ChannelLifecycle,
    /// Monotonically increasing generation token for staleness checks.
    pub generation: u64,
    pub display_id: Option<DisplayId>,
    pub anchor_bounds: Option<Bounds<Pixels>>,
    pub card_bounds: Option<Bounds<Pixels>>,
    /// Set when a preview-open timer is in flight.
    pub pending_open_generation: Option<u64>,
    /// Prospective hover source while the open-intent timer is in flight.
    pub pending_open_source: Option<CardSourceId>,
    /// Set when a preview-close timer is in flight.
    pub pending_close_generation: Option<u64>,
    /// Whether completing this close should restore the focus captured before
    /// the persistent card opened.
    pub restore_focus_after_close: bool,
}

impl ChannelSlot {
    pub fn is_open(&self) -> bool {
        self.lifecycle == ChannelLifecycle::Open
    }

    pub fn is_closed(&self) -> bool {
        self.lifecycle == ChannelLifecycle::Closed
    }

    fn holds_visibility(&self) -> bool {
        self.lifecycle != ChannelLifecycle::Closed
    }
}

// ────────────────────────────────────────────────────────────────
// Card state + reducer
// ────────────────────────────────────────────────────────────────

/// Full two-channel card state owned by `CardCoordinator`.
#[derive(Default)]
pub struct CardState {
    pub persistent: ChannelSlot,
    pub preview: ChannelSlot,
    /// Known bounds and display IDs for card sources registered via `AnchorUpdate`.
    pub anchors: HashMap<CardSourceId, (Bounds<Pixels>, DisplayId)>,
    /// Whether a compositor focus capture has been performed for the current
    /// persistent card session (cleared after final persistent dismiss).
    pub focus_captured: bool,
    /// Holds bar visibility whenever any channel is non-Closed.
    pub bar_visibility_hold: bool,
    /// Monotonically incrementing counter shared across all generations.
    generation_counter: u64,
}

impl CardState {
    fn next_generation(&mut self) -> u64 {
        self.generation_counter = self.generation_counter.wrapping_add(1);
        self.generation_counter
    }

    /// Process one `CardRequest` and return all resulting `CardEffect`s.
    pub fn reduce(&mut self, request: CardRequest) -> Vec<CardEffect> {
        let mut effects: Vec<CardEffect> = Vec::new();

        match request {
            // ─── Pointer / source events ──────────────────────────────
            CardRequest::SourceEnter { source } => {
                // If persistent for this same source is open, suppress preview.
                let persistent_blocks = self
                    .persistent
                    .source
                    .as_ref()
                    .is_some_and(|s| s == &source)
                    && self.persistent.is_open();

                if persistent_blocks {
                    return effects;
                }

                if self.preview.is_open() {
                    if self.preview.source.as_ref() == Some(&source) {
                        if self.preview.pending_close_generation.take().is_some() {
                            effects.push(CardEffect::CancelTimers {
                                kind: TimerKind::PreviewClose,
                            });
                        }
                        return effects;
                    }

                    if self.preview.pending_open_generation.take().is_some() {
                        effects.push(CardEffect::CancelTimers {
                            kind: TimerKind::PreviewOpen,
                        });
                    }
                    self.preview.pending_open_source = None;
                    if self.preview.pending_close_generation.take().is_some() {
                        effects.push(CardEffect::CancelTimers {
                            kind: TimerKind::PreviewClose,
                        });
                    }
                    let generation = self.next_generation();
                    self.preview.source = Some(source.clone());
                    self.preview.generation = generation;
                    self.preview.card_bounds = None;
                    if let Some(&(bounds, display_id)) = self.anchors.get(&source) {
                        self.preview.anchor_bounds = Some(bounds);
                        self.preview.display_id = Some(display_id);
                    }
                    effects.push(CardEffect::RetargetChannel {
                        channel: CardChannel::Preview,
                        source,
                        generation,
                    });
                    return effects;
                }

                // Only schedule preview if preview channel is closed and not already pending for this source
                if self.preview.is_closed() || self.preview.source.as_ref() != Some(&source) {
                    if self.preview.pending_open_source.as_ref() == Some(&source)
                        && self.preview.pending_open_generation.is_some()
                    {
                        return effects;
                    }
                    let tok = self.next_generation();
                    self.preview.pending_open_generation = Some(tok);
                    self.preview.pending_open_source = Some(source.clone());
                    self.preview.pending_close_generation = None;
                    effects.push(CardEffect::CancelTimers {
                        kind: TimerKind::PreviewClose,
                    });
                    effects.push(CardEffect::StartTimer {
                        kind: TimerKind::PreviewOpen,
                        generation: tok,
                    });
                    effects.append(&mut self.update_hold());
                    effects.push(CardEffect::Diagnostic(CardDiagnostic {
                        kind: DiagnosticKind::TimerStarted,
                        source: Some(source),
                        channel: Some(CardChannel::Preview),
                        generation: tok,
                    }));
                }
            }

            CardRequest::SourceLeave { source } => {
                // Cancel pending open if it's for this source.
                if self.preview.pending_open_generation.is_some()
                    && self.preview.pending_open_source.as_ref() == Some(&source)
                {
                    self.preview.pending_open_generation = None;
                    self.preview.pending_open_source = None;
                    effects.push(CardEffect::CancelTimers {
                        kind: TimerKind::PreviewOpen,
                    });
                    effects.append(&mut self.update_hold());
                }

                // If preview is open for this source, start close timer.
                if self.preview.source.as_ref() == Some(&source) && self.preview.is_open() {
                    let tok = self.next_generation();
                    self.preview.pending_close_generation = Some(tok);
                    effects.push(CardEffect::StartTimer {
                        kind: TimerKind::PreviewClose,
                        generation: tok,
                    });
                }
            }

            CardRequest::SourceGroupLeave { owner, instance_id } => {
                let belongs_to_group = |source: &CardSourceId| {
                    source.owner == owner && source.instance_id == instance_id
                };
                if self
                    .preview
                    .pending_open_source
                    .as_ref()
                    .is_some_and(&belongs_to_group)
                {
                    self.preview.pending_open_generation = None;
                    self.preview.pending_open_source = None;
                    effects.push(CardEffect::CancelTimers {
                        kind: TimerKind::PreviewOpen,
                    });
                    effects.append(&mut self.update_hold());
                }
                if self.preview.source.as_ref().is_some_and(belongs_to_group)
                    && self.preview.is_open()
                {
                    let generation = self.next_generation();
                    self.preview.pending_close_generation = Some(generation);
                    effects.push(CardEffect::StartTimer {
                        kind: TimerKind::PreviewClose,
                        generation,
                    });
                }
            }

            CardRequest::SourceGroupEnter { owner, instance_id } => {
                let active_belongs_to_group = self.preview.source.as_ref().is_some_and(|source| {
                    source.owner == owner && source.instance_id == instance_id
                });
                if active_belongs_to_group && self.preview.pending_close_generation.take().is_some()
                {
                    effects.push(CardEffect::CancelTimers {
                        kind: TimerKind::PreviewClose,
                    });
                }
            }

            CardRequest::PreviewEnter { source } => {
                // Pointer entered the preview card surface — cancel pending close.
                if self.preview.source.as_ref() == Some(&source)
                    && self.preview.pending_close_generation.is_some()
                {
                    self.preview.pending_close_generation = None;
                    effects.push(CardEffect::CancelTimers {
                        kind: TimerKind::PreviewClose,
                    });
                }
                // Also cancel any pending open (already open).
                if self.preview.pending_open_source.as_ref() == Some(&source)
                    && self.preview.pending_open_generation.is_some()
                {
                    self.preview.pending_open_generation = None;
                    self.preview.pending_open_source = None;
                    effects.push(CardEffect::CancelTimers {
                        kind: TimerKind::PreviewOpen,
                    });
                    effects.append(&mut self.update_hold());
                }
            }

            CardRequest::PreviewLeave { source } => {
                // Treat like source leave — start a close timer.
                if self.preview.source.as_ref() == Some(&source) && self.preview.is_open() {
                    let tok = self.next_generation();
                    self.preview.pending_close_generation = Some(tok);
                    effects.push(CardEffect::StartTimer {
                        kind: TimerKind::PreviewClose,
                        generation: tok,
                    });
                }
            }

            CardRequest::PersistentToggle { source } => {
                if self.persistent.source.as_ref() == Some(&source) {
                    if self.persistent.is_open() {
                        effects.append(&mut self.close_persistent(CardDismissReason::SourceToggle));
                    }
                } else {
                    effects.append(&mut self.open_persistent(source));
                }
            }
            CardRequest::PersistentToggleAt {
                source,
                bounds,
                display_id,
            } => {
                if self.persistent.source.as_ref() == Some(&source) {
                    if self.persistent.is_open() {
                        effects.append(&mut self.close_persistent(CardDismissReason::SourceToggle));
                    }
                } else {
                    self.persistent.anchor_bounds = Some(bounds);
                    self.persistent.display_id = Some(display_id);
                    effects.append(&mut self.open_persistent(source));
                }
            }

            // ─── Anchor geometry update ───────────────────────────────
            CardRequest::AnchorUpdate {
                source,
                bounds,
                display_id,
            } => {
                self.anchors.insert(source.clone(), (bounds, display_id));
                if self.persistent.source.as_ref() == Some(&source) {
                    let changed = self.persistent.anchor_bounds != Some(bounds)
                        || self.persistent.display_id != Some(display_id);
                    self.persistent.anchor_bounds = Some(bounds);
                    self.persistent.display_id = Some(display_id);
                    if changed && self.persistent.is_open() {
                        effects.push(CardEffect::RepositionChannel {
                            channel: CardChannel::Persistent,
                            source: source.clone(),
                        });
                    }
                }
                if self.preview.source.as_ref() == Some(&source)
                    || self.preview.pending_open_source.as_ref() == Some(&source)
                {
                    let changed = self.preview.anchor_bounds != Some(bounds)
                        || self.preview.display_id != Some(display_id);
                    self.preview.anchor_bounds = Some(bounds);
                    self.preview.display_id = Some(display_id);
                    if changed && self.preview.is_open() {
                        effects.push(CardEffect::RepositionChannel {
                            channel: CardChannel::Preview,
                            source,
                        });
                    }
                }
            }

            CardRequest::PlacementUpdated {
                source,
                channel,
                bounds,
                display_id,
            } => {
                let slot = match channel {
                    CardChannel::Persistent => &mut self.persistent,
                    CardChannel::Preview => &mut self.preview,
                };
                if slot.source.as_ref() == Some(&source) && slot.is_open() {
                    slot.card_bounds = Some(bounds);
                    slot.display_id = Some(display_id);
                }
            }

            CardRequest::Reposition { source } => {
                if self.persistent.source.as_ref() == Some(&source) && self.persistent.is_open() {
                    effects.push(CardEffect::RepositionChannel {
                        channel: CardChannel::Persistent,
                        source,
                    });
                }
            }

            CardRequest::AnchorRemoved { source } => {
                self.anchors.remove(&source);
                if self.persistent.source.as_ref() == Some(&source) {
                    effects
                        .append(&mut self.close_persistent(CardDismissReason::SourceDisappeared));
                }
                if self.preview.source.as_ref() == Some(&source)
                    || self.preview.pending_open_source.as_ref() == Some(&source)
                {
                    effects.append(&mut self.close_preview(CardDismissReason::SourceDisappeared));
                }
            }

            // ─── System lifecycle ─────────────────────────────────────
            CardRequest::Dismiss { channel, reason } => match channel {
                CardChannel::Persistent => {
                    effects.append(&mut self.close_persistent(reason));
                }
                CardChannel::Preview => {
                    effects.append(&mut self.close_preview(reason));
                }
            },

            CardRequest::DisplayRemoved { display_id } => {
                self.anchors
                    .retain(|_, (_, anchor_display_id)| *anchor_display_id != display_id);
                if self.persistent.display_id == Some(display_id) {
                    effects.append(&mut self.close_persistent(CardDismissReason::DisplayRemoved));
                }
                if self.preview.display_id == Some(display_id) {
                    effects.append(&mut self.close_preview(CardDismissReason::DisplayRemoved));
                }
            }

            CardRequest::OwnerRemoved { owner } => {
                self.anchors.retain(|source, _| source.owner != owner);
                if self.persistent.source.as_ref().map(|s| &s.owner) == Some(&owner) {
                    effects.append(&mut self.close_persistent(CardDismissReason::OwnerRemoved));
                }
                if self.preview.source.as_ref().map(|s| &s.owner) == Some(&owner)
                    || self.preview.pending_open_source.as_ref().map(|s| &s.owner) == Some(&owner)
                {
                    effects.append(&mut self.close_preview(CardDismissReason::OwnerRemoved));
                }
            }

            CardRequest::OverviewOpened => {
                effects.append(&mut self.close_all(CardDismissReason::OverviewOpened));
            }

            CardRequest::BarClosed => {
                effects.append(&mut self.close_all(CardDismissReason::BarClosed));
            }

            CardRequest::Shutdown => {
                effects.append(&mut self.close_all(CardDismissReason::Shutdown));
            }

            // ─── Timer completions ────────────────────────────────────
            CardRequest::PreviewOpenTimer { generation } => {
                if self.preview.pending_open_generation != Some(generation) {
                    effects.push(CardEffect::Diagnostic(CardDiagnostic {
                        kind: DiagnosticKind::TimerStale,
                        source: self.preview.source.clone(),
                        channel: Some(CardChannel::Preview),
                        generation,
                    }));
                    return effects;
                }
                self.preview.pending_open_generation = None;
                let pending_source = self.preview.pending_open_source.take();

                let source = match pending_source {
                    Some(s) => s,
                    None => {
                        effects.push(CardEffect::Diagnostic(CardDiagnostic {
                            kind: DiagnosticKind::TimerStale,
                            source: None,
                            channel: Some(CardChannel::Preview),
                            generation,
                        }));
                        return effects;
                    }
                };

                let persistent_blocks = self
                    .persistent
                    .source
                    .as_ref()
                    .is_some_and(|s| s == &source)
                    && self.persistent.is_open();

                if persistent_blocks {
                    return effects;
                }

                if self.preview.is_open() && self.preview.source.as_ref() != Some(&source) {
                    let old_display = self.preview.display_id;
                    let old_source = self.preview.source.clone();
                    self.preview.lifecycle = ChannelLifecycle::Closed;
                    self.preview.source = None;
                    self.preview.display_id = None;
                    self.preview.anchor_bounds = None;
                    self.preview.card_bounds = None;
                    effects.push(CardEffect::CloseChannel {
                        channel: CardChannel::Preview,
                        reason: CardDismissReason::Explicit,
                        display_id: old_display,
                        generation: self.preview.generation,
                        source: old_source,
                    });
                }

                let tok = self.next_generation();
                self.preview.source = Some(source.clone());
                self.preview.lifecycle = ChannelLifecycle::Open;
                self.preview.generation = tok;
                if let Some(&(bounds, display_id)) = self.anchors.get(&source) {
                    self.preview.anchor_bounds = Some(bounds);
                    self.preview.display_id = Some(display_id);
                }
                effects.push(CardEffect::OpenChannel {
                    channel: CardChannel::Preview,
                    source: source.clone(),
                    generation: tok,
                });
                effects.append(&mut self.update_hold());
                effects.push(CardEffect::Diagnostic(CardDiagnostic {
                    kind: DiagnosticKind::ChannelOpened,
                    source: Some(source),
                    channel: Some(CardChannel::Preview),
                    generation: tok,
                }));
            }

            CardRequest::PreviewCloseTimer { generation } => {
                if self.preview.pending_close_generation != Some(generation) {
                    effects.push(CardEffect::Diagnostic(CardDiagnostic {
                        kind: DiagnosticKind::TimerStale,
                        source: self.preview.source.clone(),
                        channel: Some(CardChannel::Preview),
                        generation,
                    }));
                    return effects;
                }
                self.preview.pending_close_generation = None;
                effects.append(&mut self.close_visible_preview(CardDismissReason::FocusLost));
            }
            CardRequest::CloseAnimationFinished {
                channel,
                generation,
            } => effects.append(&mut self.finish_close(channel, generation)),
        }

        effects
    }

    // ─── Internal helpers ─────────────────────────────────────────

    fn open_persistent(&mut self, source: CardSourceId) -> Vec<CardEffect> {
        let mut effects = Vec::new();

        if self.persistent.is_open() {
            let tok = self.persistent.generation;
            let display_id = self.persistent.display_id;
            let old_source = self.persistent.source.clone();
            self.persistent.lifecycle = ChannelLifecycle::Closed;
            self.persistent.source = None;
            self.persistent.display_id = None;
            self.persistent.anchor_bounds = None;
            self.persistent.card_bounds = None;
            effects.push(CardEffect::CloseChannel {
                channel: CardChannel::Persistent,
                reason: CardDismissReason::SourceToggle,
                display_id,
                generation: tok,
                source: old_source,
            });
            effects.push(CardEffect::Diagnostic(CardDiagnostic {
                kind: DiagnosticKind::ChannelClosed,
                source: None,
                channel: Some(CardChannel::Persistent),
                generation: tok,
            }));
        }

        if !self.focus_captured {
            self.focus_captured = true;
            effects.push(CardEffect::CaptureFocus);
            effects.push(CardEffect::Diagnostic(CardDiagnostic {
                kind: DiagnosticKind::FocusCaptured,
                source: Some(source.clone()),
                channel: Some(CardChannel::Persistent),
                generation: self.generation_counter,
            }));
        }

        let tok = self.next_generation();
        self.persistent.source = Some(source.clone());
        self.persistent.lifecycle = ChannelLifecycle::Open;
        self.persistent.generation = tok;
        self.persistent.restore_focus_after_close = false;
        if let Some(&(bounds, display_id)) = self.anchors.get(&source) {
            self.persistent.anchor_bounds = Some(bounds);
            self.persistent.display_id = Some(display_id);
        }

        effects.push(CardEffect::OpenChannel {
            channel: CardChannel::Persistent,
            source: source.clone(),
            generation: tok,
        });
        effects.append(&mut self.update_hold());
        effects.push(CardEffect::Diagnostic(CardDiagnostic {
            kind: DiagnosticKind::ChannelOpened,
            source: Some(source),
            channel: Some(CardChannel::Persistent),
            generation: tok,
        }));
        effects
    }

    fn close_persistent(&mut self, reason: CardDismissReason) -> Vec<CardEffect> {
        let mut effects = Vec::new();
        if !self.persistent.is_open() {
            return effects;
        }

        let tok = self.persistent.generation;
        let display_id = self.persistent.display_id;
        let source = self.persistent.source.clone();
        self.persistent.lifecycle = ChannelLifecycle::Closing;
        self.persistent.restore_focus_after_close = !matches!(
            reason,
            CardDismissReason::FocusLost | CardDismissReason::OverviewOpened
        );

        effects.push(CardEffect::CloseChannel {
            channel: CardChannel::Persistent,
            reason,
            display_id,
            generation: tok,
            source,
        });
        effects
    }

    fn close_preview(&mut self, reason: CardDismissReason) -> Vec<CardEffect> {
        let mut effects = Vec::new();
        if self.preview.is_closed() && self.preview.pending_open_generation.is_none() {
            return effects;
        }

        if self.preview.pending_open_generation.take().is_some() {
            effects.push(CardEffect::CancelTimers {
                kind: TimerKind::PreviewOpen,
            });
        }
        self.preview.pending_open_source = None;
        if self.preview.pending_close_generation.take().is_some() {
            effects.push(CardEffect::CancelTimers {
                kind: TimerKind::PreviewClose,
            });
        }

        if self.preview.is_open() {
            let tok = self.preview.generation;
            let display_id = self.preview.display_id;
            let source = self.preview.source.clone();
            self.preview.lifecycle = ChannelLifecycle::Closing;

            effects.push(CardEffect::CloseChannel {
                channel: CardChannel::Preview,
                reason,
                display_id,
                generation: tok,
                source,
            });
        }
        effects.append(&mut self.update_hold());
        effects
    }

    /// Close the currently visible preview without discarding a hover intent
    /// already queued for a different source.
    fn close_visible_preview(&mut self, reason: CardDismissReason) -> Vec<CardEffect> {
        let mut effects = Vec::new();
        if self.preview.is_open() {
            let generation = self.preview.generation;
            let display_id = self.preview.display_id;
            let source = self.preview.source.clone();
            self.preview.lifecycle = ChannelLifecycle::Closing;
            effects.push(CardEffect::CloseChannel {
                channel: CardChannel::Preview,
                reason,
                display_id,
                generation,
                source,
            });
        }
        effects.append(&mut self.update_hold());
        effects
    }

    fn close_all(&mut self, reason: CardDismissReason) -> Vec<CardEffect> {
        let mut effects = Vec::new();
        effects.append(&mut self.close_persistent(reason.clone()));
        effects.append(&mut self.close_preview(reason));
        effects
    }

    fn finish_close(&mut self, channel: CardChannel, generation: u64) -> Vec<CardEffect> {
        let slot = match channel {
            CardChannel::Persistent => &mut self.persistent,
            CardChannel::Preview => &mut self.preview,
        };
        if slot.lifecycle != ChannelLifecycle::Closing || slot.generation != generation {
            return Vec::new();
        }

        let source = slot.source.take();
        slot.lifecycle = ChannelLifecycle::Closed;
        slot.generation = 0;
        slot.display_id = None;
        slot.anchor_bounds = None;
        slot.card_bounds = None;

        let mut effects = Vec::new();
        let restore_focus = slot.restore_focus_after_close;
        slot.restore_focus_after_close = false;
        if channel == CardChannel::Persistent && self.focus_captured {
            self.focus_captured = false;
            if restore_focus {
                effects.push(CardEffect::RestoreFocus);
                effects.push(CardEffect::Diagnostic(CardDiagnostic {
                    kind: DiagnosticKind::FocusRestored,
                    source: source.clone(),
                    channel: Some(channel),
                    generation,
                }));
            }
        }
        effects.append(&mut self.update_hold());
        effects.push(CardEffect::Diagnostic(CardDiagnostic {
            kind: DiagnosticKind::ChannelClosed,
            source,
            channel: Some(channel),
            generation,
        }));
        effects
    }

    fn update_hold(&mut self) -> Vec<CardEffect> {
        let hold = self.persistent.holds_visibility()
            || self.preview.holds_visibility()
            || self.preview.pending_open_generation.is_some();

        if hold != self.bar_visibility_hold {
            self.bar_visibility_hold = hold;
            return vec![CardEffect::UpdateBarHold { hold }];
        }
        vec![]
    }

    // ─── Public queries ───────────────────────────────────────────

    /// Current source-widget state for a given source.
    pub fn source_state(&self, source: &CardSourceId) -> CardSourceState {
        if self.persistent.source.as_ref() == Some(source) && self.persistent.is_open() {
            return CardSourceState::PersistentOpen;
        }
        if self.preview.source.as_ref() == Some(source) && self.preview.is_open() {
            return CardSourceState::PreviewOpen;
        }
        if self.preview.pending_open_generation.is_some()
            && self.preview.pending_open_source.as_ref() == Some(source)
        {
            return CardSourceState::HoverPending;
        }
        CardSourceState::Idle
    }

    /// Whether the card system currently requires bar visibility to be held.
    pub fn holds_bar_visibility(&self) -> bool {
        self.bar_visibility_hold
    }

    /// Test helper for directly priming a pending hover source.
    #[cfg(test)]
    pub fn set_pending_preview_source(&mut self, source: CardSourceId) {
        self.preview.pending_open_source = Some(source);
    }
}

// ────────────────────────────────────────────────────────────────
// Tests
// ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use gpui::{Bounds, DisplayId, Pixels, Point, Size, px};

    use super::*;

    fn source(id: &str) -> CardSourceId {
        CardSourceId::singleton(id)
    }

    fn source_ws(ws_id: u64) -> CardSourceId {
        CardSourceId::new("workspaces", "bar:1:ws", Some(ws_id.to_string()))
    }

    fn bounds() -> Bounds<Pixels> {
        Bounds {
            origin: Point {
                x: px(100.0),
                y: px(50.0),
            },
            size: Size {
                width: px(48.0),
                height: px(40.0),
            },
        }
    }

    fn has_effect(effects: &[CardEffect], predicate: impl Fn(&CardEffect) -> bool) -> bool {
        effects.iter().any(predicate)
    }

    fn open_timer_gen(effects: &[CardEffect]) -> Option<u64> {
        effects.iter().find_map(|e| {
            if let CardEffect::StartTimer {
                kind: TimerKind::PreviewOpen,
                generation,
            } = e
            {
                Some(*generation)
            } else {
                None
            }
        })
    }

    fn close_generation(effects: &[CardEffect]) -> u64 {
        effects
            .iter()
            .find_map(|effect| match effect {
                CardEffect::CloseChannel { generation, .. } => Some(*generation),
                _ => None,
            })
            .expect("close animation generation")
    }

    fn close_timer_gen(effects: &[CardEffect]) -> Option<u64> {
        effects.iter().find_map(|e| {
            if let CardEffect::StartTimer {
                kind: TimerKind::PreviewClose,
                generation,
            } = e
            {
                Some(*generation)
            } else {
                None
            }
        })
    }

    // ── Hover open timing ─────────────────────────────────────────

    #[test]
    fn source_enter_starts_preview_open_timer() {
        let mut state = CardState::default();
        state.set_pending_preview_source(source("battery"));
        let effects = state.reduce(CardRequest::SourceEnter {
            source: source("battery"),
        });
        assert!(
            open_timer_gen(&effects).is_some(),
            "SourceEnter should start a preview open timer"
        );
    }

    #[test]
    fn repeated_source_enter_keeps_the_existing_hover_intent_timer() {
        let mut state = CardState::default();
        let workspace = source_ws(1);
        let first = state.reduce(CardRequest::SourceEnter {
            source: workspace.clone(),
        });
        let generation = open_timer_gen(&first).expect("initial hover intent timer");

        let repeated = state.reduce(CardRequest::SourceEnter { source: workspace });

        assert!(repeated.is_empty());
        assert_eq!(state.preview.pending_open_generation, Some(generation));
    }

    #[test]
    fn sources_for_the_same_workspace_on_different_bars_are_distinct() {
        let first = CardSourceId::new("workspaces", "bar:display-1", Some("7"));
        let second = CardSourceId::new("workspaces", "bar:display-2", Some("7"));

        assert_ne!(first, second);
        let mut state = CardState::default();
        state.reduce(CardRequest::SourceEnter {
            source: first.clone(),
        });
        assert_eq!(state.source_state(&first), CardSourceState::HoverPending);
        assert_eq!(state.source_state(&second), CardSourceState::Idle);
    }

    #[test]
    fn entering_another_source_retargets_an_open_preview_without_reopening() {
        let mut state = CardState::default();
        let first = source_ws(1);
        let second = source_ws(2);
        let first_enter = state.reduce(CardRequest::SourceEnter {
            source: first.clone(),
        });
        state.reduce(CardRequest::PreviewOpenTimer {
            generation: open_timer_gen(&first_enter).expect("first open timer"),
        });

        let effects = state.reduce(CardRequest::SourceEnter {
            source: second.clone(),
        });

        assert!(has_effect(&effects, |effect| matches!(
            effect,
            CardEffect::RetargetChannel {
                channel: CardChannel::Preview,
                source,
                ..
            } if source == &second
        )));
        assert!(!has_effect(&effects, |effect| matches!(
            effect,
            CardEffect::StartTimer {
                kind: TimerKind::PreviewOpen,
                ..
            } | CardEffect::CloseChannel { .. }
                | CardEffect::OpenChannel { .. }
        )));
        assert_eq!(state.source_state(&first), CardSourceState::Idle);
        assert_eq!(state.source_state(&second), CardSourceState::PreviewOpen);
    }

    #[test]
    fn leaving_a_source_group_closes_its_visible_preview() {
        let mut state = CardState::default();
        let workspace = source_ws(1);
        let enter = state.reduce(CardRequest::SourceEnter { source: workspace });
        state.reduce(CardRequest::PreviewOpenTimer {
            generation: open_timer_gen(&enter).expect("open timer"),
        });

        let effects = state.reduce(CardRequest::SourceGroupLeave {
            owner: CardOwnerId::new("workspaces"),
            instance_id: "bar:1:ws".into(),
        });

        assert!(has_effect(&effects, |effect| matches!(
            effect,
            CardEffect::StartTimer {
                kind: TimerKind::PreviewClose,
                ..
            }
        )));
    }

    #[test]
    fn reentering_a_source_group_cancels_its_pending_close() {
        let mut state = CardState::default();
        let workspace = source_ws(1);
        let enter = state.reduce(CardRequest::SourceEnter { source: workspace });
        state.reduce(CardRequest::PreviewOpenTimer {
            generation: open_timer_gen(&enter).expect("open timer"),
        });
        state.reduce(CardRequest::SourceGroupLeave {
            owner: CardOwnerId::new("workspaces"),
            instance_id: "bar:1:ws".into(),
        });

        let effects = state.reduce(CardRequest::SourceGroupEnter {
            owner: CardOwnerId::new("workspaces"),
            instance_id: "bar:1:ws".into(),
        });

        assert!(has_effect(&effects, |effect| matches!(
            effect,
            CardEffect::CancelTimers {
                kind: TimerKind::PreviewClose
            }
        )));
        assert!(state.preview.pending_close_generation.is_none());
        assert!(state.preview.is_open());
    }

    #[test]
    fn preview_open_timer_with_correct_generation_opens_channel() {
        let mut state = CardState::default();
        state.set_pending_preview_source(source("battery"));
        let effects = state.reduce(CardRequest::SourceEnter {
            source: source("battery"),
        });
        let tok = open_timer_gen(&effects).unwrap();

        let open_effects = state.reduce(CardRequest::PreviewOpenTimer { generation: tok });
        assert!(
            has_effect(&open_effects, |e| matches!(
                e,
                CardEffect::OpenChannel {
                    channel: CardChannel::Preview,
                    ..
                }
            )),
            "Correct-generation timer should open Preview channel"
        );
    }

    #[test]
    fn preview_open_timer_stale_generation_is_noop() {
        let mut state = CardState::default();
        state.set_pending_preview_source(source("battery"));
        let effects = state.reduce(CardRequest::SourceEnter {
            source: source("battery"),
        });
        let tok = open_timer_gen(&effects).unwrap();

        let noop_effects = state.reduce(CardRequest::PreviewOpenTimer {
            generation: tok + 99,
        });
        assert!(
            !has_effect(&noop_effects, |e| matches!(
                e,
                CardEffect::OpenChannel { .. }
            )),
            "Stale generation timer should not open channel"
        );
        assert!(has_effect(&noop_effects, |e| matches!(
            e,
            CardEffect::Diagnostic(CardDiagnostic {
                kind: DiagnosticKind::TimerStale,
                ..
            })
        )));
    }

    // ── Hover close timing ────────────────────────────────────────

    #[test]
    fn source_leave_after_open_starts_preview_close_timer() {
        let mut state = CardState::default();
        state.set_pending_preview_source(source("battery"));
        let effects = state.reduce(CardRequest::SourceEnter {
            source: source("battery"),
        });
        let tok = open_timer_gen(&effects).unwrap();
        state.reduce(CardRequest::PreviewOpenTimer { generation: tok });

        let leave_effects = state.reduce(CardRequest::SourceLeave {
            source: source("battery"),
        });
        assert!(
            close_timer_gen(&leave_effects).is_some(),
            "SourceLeave after open should start a preview close timer"
        );
    }

    #[test]
    fn preview_close_timer_closes_channel() {
        let mut state = CardState::default();
        state.set_pending_preview_source(source("battery"));
        let effects = state.reduce(CardRequest::SourceEnter {
            source: source("battery"),
        });
        let tok = open_timer_gen(&effects).unwrap();
        state.reduce(CardRequest::PreviewOpenTimer { generation: tok });

        let leave_effects = state.reduce(CardRequest::SourceLeave {
            source: source("battery"),
        });
        let close_gen = close_timer_gen(&leave_effects).unwrap();

        let close_effects = state.reduce(CardRequest::PreviewCloseTimer {
            generation: close_gen,
        });
        assert!(
            has_effect(&close_effects, |e| matches!(
                e,
                CardEffect::CloseChannel {
                    channel: CardChannel::Preview,
                    ..
                }
            )),
            "Close timer should close Preview channel"
        );
    }

    #[test]
    fn stale_sibling_leave_does_not_close_a_retargeted_preview() {
        let mut state = CardState::default();
        let first = source_ws(1);
        let second = source_ws(2);

        let first_enter = state.reduce(CardRequest::SourceEnter {
            source: first.clone(),
        });
        state.reduce(CardRequest::PreviewOpenTimer {
            generation: open_timer_gen(&first_enter).expect("first open timer"),
        });
        let second_enter = state.reduce(CardRequest::SourceEnter {
            source: second.clone(),
        });
        // GPUI can deliver the new sibling's enter before the old sibling's leave.
        let first_leave = state.reduce(CardRequest::SourceLeave { source: first });

        assert!(has_effect(&second_enter, |effect| matches!(
            effect,
            CardEffect::RetargetChannel {
                channel: CardChannel::Preview,
                source,
                ..
            } if source == &second
        )));
        assert!(first_leave.is_empty());
        assert_eq!(state.source_state(&second), CardSourceState::PreviewOpen);
    }

    #[test]
    fn source_leave_before_timer_fires_cancels_open() {
        let mut state = CardState::default();
        state.set_pending_preview_source(source("battery"));
        let effects = state.reduce(CardRequest::SourceEnter {
            source: source("battery"),
        });
        let tok = open_timer_gen(&effects).unwrap();

        let leave_effects = state.reduce(CardRequest::SourceLeave {
            source: source("battery"),
        });
        assert!(
            has_effect(&leave_effects, |e| matches!(
                e,
                CardEffect::CancelTimers {
                    kind: TimerKind::PreviewOpen
                }
            )),
            "SourceLeave before timer fires should cancel open timer"
        );

        let late_effects = state.reduce(CardRequest::PreviewOpenTimer { generation: tok });
        assert!(!has_effect(&late_effects, |e| matches!(
            e,
            CardEffect::OpenChannel { .. }
        )));
    }

    // ── Pointer bridge (source → preview) ────────────────────────

    #[test]
    fn preview_enter_cancels_pending_close() {
        let mut state = CardState::default();
        state.set_pending_preview_source(source("battery"));
        let effects = state.reduce(CardRequest::SourceEnter {
            source: source("battery"),
        });
        let tok = open_timer_gen(&effects).unwrap();
        state.reduce(CardRequest::PreviewOpenTimer { generation: tok });

        state.reduce(CardRequest::SourceLeave {
            source: source("battery"),
        });

        let enter_effects = state.reduce(CardRequest::PreviewEnter {
            source: source("battery"),
        });
        assert!(
            has_effect(&enter_effects, |e| matches!(
                e,
                CardEffect::CancelTimers {
                    kind: TimerKind::PreviewClose
                }
            )),
            "PreviewEnter should cancel pending close timer"
        );
    }

    // ── Persistent toggle ─────────────────────────────────────────

    #[test]
    fn persistent_toggle_opens_channel() {
        let mut state = CardState::default();
        let effects = state.reduce(CardRequest::PersistentToggle {
            source: source_ws(1),
        });
        assert!(has_effect(&effects, |e| matches!(
            e,
            CardEffect::OpenChannel {
                channel: CardChannel::Persistent,
                ..
            }
        )));
    }

    #[test]
    fn persistent_toggle_closes_open_channel() {
        let mut state = CardState::default();
        state.reduce(CardRequest::PersistentToggle {
            source: source_ws(1),
        });
        let effects = state.reduce(CardRequest::PersistentToggle {
            source: source_ws(1),
        });
        assert!(has_effect(&effects, |e| matches!(
            e,
            CardEffect::CloseChannel {
                channel: CardChannel::Persistent,
                ..
            }
        )));
    }

    #[test]
    fn persistent_replacement_skips_focus_restore_between_cards() {
        let mut state = CardState::default();
        state.reduce(CardRequest::PersistentToggle {
            source: source("battery"),
        });
        let effects = state.reduce(CardRequest::PersistentToggle {
            source: source_ws(1),
        });
        assert!(
            !has_effect(&effects, |e| matches!(e, CardEffect::RestoreFocus)),
            "Replacement should not restore focus between cards"
        );
        assert!(has_effect(&effects, |e| matches!(
            e,
            CardEffect::OpenChannel {
                channel: CardChannel::Persistent,
                source: target_source,
                ..
            } if target_source == &source_ws(1)
        )));
    }

    // ── Preview suppression ───────────────────────────────────────

    #[test]
    fn same_source_persistent_suppresses_preview() {
        let mut state = CardState::default();
        state.reduce(CardRequest::PersistentToggle {
            source: source("battery"),
        });

        state.set_pending_preview_source(source("battery"));
        let effects = state.reduce(CardRequest::SourceEnter {
            source: source("battery"),
        });
        assert!(
            !has_effect(&effects, |e| matches!(
                e,
                CardEffect::StartTimer {
                    kind: TimerKind::PreviewOpen,
                    ..
                }
            )),
            "Same-source persistent should suppress preview"
        );
    }

    #[test]
    fn different_source_persistent_allows_preview() {
        let mut state = CardState::default();
        state.reduce(CardRequest::PersistentToggle {
            source: source("battery"),
        });

        state.set_pending_preview_source(source_ws(1));
        let effects = state.reduce(CardRequest::SourceEnter {
            source: source_ws(1),
        });
        assert!(
            has_effect(&effects, |e| matches!(
                e,
                CardEffect::StartTimer {
                    kind: TimerKind::PreviewOpen,
                    ..
                }
            )),
            "Different-source persistent should allow preview"
        );
    }

    // ── Focus capture / restore ───────────────────────────────────

    #[test]
    fn first_persistent_open_emits_capture_focus() {
        let mut state = CardState::default();
        let effects = state.reduce(CardRequest::PersistentToggle {
            source: source("battery"),
        });
        assert!(has_effect(&effects, |e| matches!(
            e,
            CardEffect::CaptureFocus
        )));
    }

    #[test]
    fn second_persistent_open_does_not_re_capture_focus() {
        let mut state = CardState::default();
        state.reduce(CardRequest::PersistentToggle {
            source: source("battery"),
        });
        let effects = state.reduce(CardRequest::PersistentToggle {
            source: source_ws(1),
        });
        assert!(
            !has_effect(&effects, |e| matches!(e, CardEffect::CaptureFocus)),
            "Second open should not re-capture focus"
        );
    }

    #[test]
    fn persistent_close_emits_restore_focus() {
        let mut state = CardState::default();
        state.reduce(CardRequest::PersistentToggle {
            source: source("battery"),
        });
        let effects = state.reduce(CardRequest::PersistentToggle {
            source: source("battery"),
        });
        assert!(!has_effect(&effects, |e| matches!(
            e,
            CardEffect::RestoreFocus
        )));
        let completion = state.reduce(CardRequest::CloseAnimationFinished {
            channel: CardChannel::Persistent,
            generation: close_generation(&effects),
        });
        assert!(has_effect(&completion, |e| matches!(
            e,
            CardEffect::RestoreFocus
        )));
    }

    #[test]
    fn focus_loss_close_does_not_steal_focus_back() {
        let mut state = CardState::default();
        state.reduce(CardRequest::PersistentToggle {
            source: source("battery"),
        });
        let effects = state.reduce(CardRequest::Dismiss {
            channel: CardChannel::Persistent,
            reason: CardDismissReason::FocusLost,
        });
        let completion = state.reduce(CardRequest::CloseAnimationFinished {
            channel: CardChannel::Persistent,
            generation: close_generation(&effects),
        });

        assert!(!has_effect(&completion, |effect| matches!(
            effect,
            CardEffect::RestoreFocus
        )));
    }

    // ── Dismissal paths ───────────────────────────────────────────

    #[test]
    fn overview_opened_closes_all_channels() {
        let mut state = CardState::default();
        state.reduce(CardRequest::PersistentToggle {
            source: source("battery"),
        });
        state.set_pending_preview_source(source_ws(1));
        state.reduce(CardRequest::SourceEnter {
            source: source_ws(1),
        });
        let open_gen = state.preview.pending_open_generation.unwrap();
        state.reduce(CardRequest::PreviewOpenTimer {
            generation: open_gen,
        });

        let effects = state.reduce(CardRequest::OverviewOpened);
        let close_persistent = effects.iter().any(|e| {
            matches!(
                e,
                CardEffect::CloseChannel {
                    channel: CardChannel::Persistent,
                    reason: CardDismissReason::OverviewOpened,
                    ..
                }
            )
        });
        let close_preview = effects.iter().any(|e| {
            matches!(
                e,
                CardEffect::CloseChannel {
                    channel: CardChannel::Preview,
                    reason: CardDismissReason::OverviewOpened,
                    ..
                }
            )
        });
        assert!(close_persistent, "OverviewOpened should close persistent");
        assert!(close_preview, "OverviewOpened should close preview");
    }

    #[test]
    fn bar_closed_closes_all_channels() {
        let mut state = CardState::default();
        state.reduce(CardRequest::PersistentToggle {
            source: source("battery"),
        });
        let effects = state.reduce(CardRequest::BarClosed);
        assert!(has_effect(&effects, |e| matches!(
            e,
            CardEffect::CloseChannel {
                channel: CardChannel::Persistent,
                reason: CardDismissReason::BarClosed,
                ..
            }
        )));
    }

    #[test]
    fn shutdown_closes_all_channels() {
        let mut state = CardState::default();
        state.reduce(CardRequest::PersistentToggle {
            source: source("battery"),
        });
        let effects = state.reduce(CardRequest::Shutdown);
        assert!(has_effect(&effects, |e| matches!(
            e,
            CardEffect::CloseChannel {
                channel: CardChannel::Persistent,
                reason: CardDismissReason::Shutdown,
                ..
            }
        )));
    }

    #[test]
    fn display_removed_closes_only_matching_display() {
        let mut state = CardState::default();
        state.reduce(CardRequest::PersistentToggle {
            source: source("battery"),
        });
        state.reduce(CardRequest::AnchorUpdate {
            source: source("battery"),
            bounds: bounds(),
            display_id: DisplayId::new(1),
        });

        let effects = state.reduce(CardRequest::DisplayRemoved {
            display_id: DisplayId::new(1),
        });
        assert!(has_effect(&effects, |e| matches!(
            e,
            CardEffect::CloseChannel {
                channel: CardChannel::Persistent,
                reason: CardDismissReason::DisplayRemoved,
                ..
            }
        )));
    }

    #[test]
    fn display_removed_different_display_noop_for_persistent() {
        let mut state = CardState::default();
        state.reduce(CardRequest::PersistentToggle {
            source: source("battery"),
        });
        state.reduce(CardRequest::AnchorUpdate {
            source: source("battery"),
            bounds: bounds(),
            display_id: DisplayId::new(1),
        });

        let effects = state.reduce(CardRequest::DisplayRemoved {
            display_id: DisplayId::new(2),
        });
        assert!(
            !has_effect(&effects, |e| matches!(
                e,
                CardEffect::CloseChannel {
                    channel: CardChannel::Persistent,
                    ..
                }
            )),
            "DisplayRemoved for a different display should not close persistent"
        );
    }

    #[test]
    fn display_removed_forgets_all_source_anchors_on_that_display() {
        let first = source_ws(1);
        let second = source_ws(2);
        let third = source("battery");
        let mut state = CardState::default();
        for (source, display_id) in [
            (first.clone(), DisplayId::new(1)),
            (second.clone(), DisplayId::new(1)),
            (third.clone(), DisplayId::new(2)),
        ] {
            state.reduce(CardRequest::AnchorUpdate {
                source,
                bounds: bounds(),
                display_id,
            });
        }

        state.reduce(CardRequest::DisplayRemoved {
            display_id: DisplayId::new(1),
        });

        assert!(!state.anchors.contains_key(&first));
        assert!(!state.anchors.contains_key(&second));
        assert!(state.anchors.contains_key(&third));
    }

    // ── Source state projections ──────────────────────────────────

    #[test]
    fn source_state_idle_when_nothing_open() {
        let state = CardState::default();
        assert_eq!(
            state.source_state(&source("battery")),
            CardSourceState::Idle
        );
    }

    #[test]
    fn source_state_persistent_open_when_persistent_active() {
        let mut state = CardState::default();
        state.reduce(CardRequest::PersistentToggle {
            source: source("battery"),
        });
        assert_eq!(
            state.source_state(&source("battery")),
            CardSourceState::PersistentOpen
        );
    }

    #[test]
    fn source_state_preview_open_when_preview_active() {
        let mut state = CardState::default();
        state.set_pending_preview_source(source("battery"));
        let effects = state.reduce(CardRequest::SourceEnter {
            source: source("battery"),
        });
        let tok = open_timer_gen(&effects).unwrap();
        state.reduce(CardRequest::PreviewOpenTimer { generation: tok });
        assert_eq!(
            state.source_state(&source("battery")),
            CardSourceState::PreviewOpen
        );
    }

    #[test]
    fn source_state_hover_pending_when_timer_queued() {
        let mut state = CardState::default();
        state.set_pending_preview_source(source("battery"));
        state.reduce(CardRequest::SourceEnter {
            source: source("battery"),
        });
        assert_eq!(
            state.source_state(&source("battery")),
            CardSourceState::HoverPending
        );
    }

    // ── Visibility hold ───────────────────────────────────────────

    #[test]
    fn hold_set_when_persistent_open() {
        let mut state = CardState::default();
        let effects = state.reduce(CardRequest::PersistentToggle {
            source: source("battery"),
        });
        assert!(
            has_effect(&effects, |e| matches!(
                e,
                CardEffect::UpdateBarHold { hold: true }
            )),
            "Opening persistent should set bar hold"
        );
        assert!(state.holds_bar_visibility());
    }

    #[test]
    fn hold_is_retained_until_close_animation_finishes() {
        let mut state = CardState::default();
        state.reduce(CardRequest::PersistentToggle {
            source: source("battery"),
        });
        let effects = state.reduce(CardRequest::PersistentToggle {
            source: source("battery"),
        });
        let generation = effects
            .iter()
            .find_map(|effect| match effect {
                CardEffect::CloseChannel { generation, .. } => Some(*generation),
                _ => None,
            })
            .expect("close animation generation");
        assert!(state.holds_bar_visibility());

        let completion = state.reduce(CardRequest::CloseAnimationFinished {
            channel: CardChannel::Persistent,
            generation,
        });
        assert!(has_effect(&completion, |effect| matches!(
            effect,
            CardEffect::UpdateBarHold { hold: false }
        )));
        assert!(!state.holds_bar_visibility());
    }

    #[test]
    fn hold_set_when_preview_timer_pending() {
        let mut state = CardState::default();
        state.set_pending_preview_source(source("battery"));
        let effects = state.reduce(CardRequest::SourceEnter {
            source: source("battery"),
        });
        assert!(
            has_effect(&effects, |e| matches!(
                e,
                CardEffect::UpdateBarHold { hold: true }
            )),
            "Pending preview timer should set bar hold"
        );
    }

    #[test]
    fn source_enter_records_source_without_test_priming() {
        let mut state = CardState::default();
        let effects = state.reduce(CardRequest::SourceEnter {
            source: source_ws(1),
        });
        let generation = open_timer_gen(&effects).expect("hover intent timer");
        assert_eq!(
            state.preview.pending_open_source.as_ref(),
            Some(&source_ws(1))
        );
        let effects = state.reduce(CardRequest::PreviewOpenTimer { generation });
        assert!(has_effect(&effects, |effect| matches!(
            effect,
            CardEffect::OpenChannel {
                channel: CardChannel::Preview,
                source,
                ..
            } if source == &source_ws(1)
        )));
    }

    #[test]
    fn close_effect_preserves_surface_display() {
        let mut state = CardState::default();
        let battery = source("battery");
        state.reduce(CardRequest::PersistentToggle {
            source: battery.clone(),
        });
        state.reduce(CardRequest::AnchorUpdate {
            source: battery.clone(),
            bounds: bounds(),
            display_id: DisplayId::new(7),
        });
        let effects = state.reduce(CardRequest::PersistentToggle { source: battery });
        assert!(has_effect(&effects, |effect| matches!(
            effect,
            CardEffect::CloseChannel {
                channel: CardChannel::Persistent,
                display_id: Some(id),
                ..
            } if *id == DisplayId::new(7)
        )));
    }

    #[test]
    fn anchor_change_requests_live_reposition() {
        let mut state = CardState::default();
        let battery = source("battery");
        state.reduce(CardRequest::PersistentToggle {
            source: battery.clone(),
        });
        let effects = state.reduce(CardRequest::AnchorUpdate {
            source: battery.clone(),
            bounds: bounds(),
            display_id: DisplayId::new(1),
        });
        assert!(has_effect(&effects, |effect| matches!(
            effect,
            CardEffect::RepositionChannel {
                channel: CardChannel::Persistent,
                source,
            } if source == &battery
        )));
    }

    #[test]
    fn removing_an_owner_does_not_close_another_owners_channels() {
        let mut state = CardState::default();
        let battery = source("battery");
        state.reduce(CardRequest::PersistentToggle {
            source: battery.clone(),
        });

        let effects = state.reduce(CardRequest::OwnerRemoved {
            owner: CardOwnerId::new("workspaces"),
        });

        assert!(effects.is_empty());
        assert_eq!(state.persistent.source.as_ref(), Some(&battery));
        assert!(state.persistent.is_open());
    }

    #[test]
    fn removing_an_owner_forgets_only_that_owners_anchors() {
        let workspace = source_ws(1);
        let battery = source("battery");
        let mut state = CardState::default();
        for source in [workspace.clone(), battery.clone()] {
            state.reduce(CardRequest::AnchorUpdate {
                source,
                bounds: bounds(),
                display_id: DisplayId::new(1),
            });
        }

        state.reduce(CardRequest::OwnerRemoved {
            owner: CardOwnerId::new("workspaces"),
        });

        assert!(!state.anchors.contains_key(&workspace));
        assert!(state.anchors.contains_key(&battery));
    }

    #[test]
    fn removing_pending_preview_owner_cancels_the_hold() {
        let mut state = CardState::default();
        let workspaces = source_ws(1);
        state.reduce(CardRequest::SourceEnter {
            source: workspaces.clone(),
        });

        let effects = state.reduce(CardRequest::OwnerRemoved {
            owner: CardOwnerId::new("workspaces"),
        });

        assert!(has_effect(&effects, |effect| matches!(
            effect,
            CardEffect::CancelTimers {
                kind: TimerKind::PreviewOpen
            }
        )));
        assert!(!state.holds_bar_visibility());
    }

    #[test]
    fn disappearing_anchor_closes_only_its_source() {
        let mut state = CardState::default();
        let battery = source("battery");
        state.reduce(CardRequest::PersistentToggle {
            source: battery.clone(),
        });

        let effects = state.reduce(CardRequest::AnchorRemoved {
            source: battery.clone(),
        });

        assert!(has_effect(&effects, |effect| matches!(
            effect,
            CardEffect::CloseChannel {
                channel: CardChannel::Persistent,
                reason: CardDismissReason::SourceDisappeared,
                ..
            }
        )));
        assert_eq!(state.source_state(&battery), CardSourceState::Idle);
        assert!(state.holds_bar_visibility());
        state.reduce(CardRequest::CloseAnimationFinished {
            channel: CardChannel::Persistent,
            generation: close_generation(&effects),
        });
        assert!(!state.holds_bar_visibility());
    }

    #[test]
    fn persistent_activation_captures_exact_source_anchor() {
        let mut state = CardState::default();
        let source_bounds = bounds();
        let display_id = DisplayId::new(9);

        state.reduce(CardRequest::PersistentToggleAt {
            source: source("battery"),
            bounds: source_bounds,
            display_id,
        });

        assert_eq!(state.persistent.anchor_bounds, Some(source_bounds));
        assert_eq!(state.persistent.display_id, Some(display_id));
        assert!(state.persistent.is_open());
    }

    #[test]
    fn same_source_activation_during_focus_loss_close_does_not_reopen() {
        let mut state = CardState::default();
        let battery = source("battery");
        state.reduce(CardRequest::PersistentToggleAt {
            source: battery.clone(),
            bounds: bounds(),
            display_id: DisplayId::new(9),
        });
        state.reduce(CardRequest::Dismiss {
            channel: CardChannel::Persistent,
            reason: CardDismissReason::FocusLost,
        });
        let closing_generation = state.persistent.generation;

        let effects = state.reduce(CardRequest::PersistentToggleAt {
            source: battery,
            bounds: bounds(),
            display_id: DisplayId::new(9),
        });

        assert!(effects.is_empty());
        assert_eq!(state.persistent.lifecycle, ChannelLifecycle::Closing);
        assert_eq!(state.persistent.generation, closing_generation);
    }
}
