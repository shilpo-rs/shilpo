use std::fmt;
use std::path::{Component, Path};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ViewLimits {
    pub max_depth: usize,
    pub max_nodes: usize,
    pub max_text_bytes: usize,
    pub max_list_items: usize,
}

impl Default for ViewLimits {
    fn default() -> Self {
        Self {
            max_depth: 32,
            max_nodes: 1_024,
            max_text_bytes: 64 * 1_024,
            max_list_items: 256,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ViewValidationError(String);

impl fmt::Display for ViewValidationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl std::error::Error for ViewValidationError {}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ViewTree {
    pub root: ViewNode,
}

impl ViewTree {
    pub fn new(root: ViewNode) -> Self {
        Self { root }
    }

    pub fn validate(&self, limits: ViewLimits) -> Result<(), ViewValidationError> {
        let mut state = ValidationState::default();
        validate_node(&self.root, 1, limits, &mut state)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ViewNode {
    Container(ContainerNode),
    Text(TextNode),
    Icon(IconNode),
    Image(ImageNode),
    Button(ButtonNode),
    IconButton(IconButtonNode),
    Toggle(ToggleNode),
    Slider(SliderNode),
    TextInput(TextInputNode),
    List(ListNode),
    Spacer(SpacerNode),
    Divider,
    Badge(BadgeNode),
    Progress(ProgressNode),
    LoadingIndicator(LoadingIndicatorNode),
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ContainerDirection {
    Row,
    Column,
    Stack,
    Grid { columns: u16 },
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Alignment {
    Start,
    Center,
    End,
    Stretch,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Justification {
    Start,
    Center,
    End,
    SpaceBetween,
    SpaceAround,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TextAlign {
    Start,
    Center,
    End,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Overflow {
    Visible,
    Hidden,
    Scroll,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SemanticColorToken {
    Surface,
    OnSurface,
    SurfaceDim,
    SurfaceBright,
    SurfaceContainerLowest,
    SurfaceContainerLow,
    SurfaceContainer,
    SurfaceContainerHigh,
    SurfaceContainerHighest,
    SurfaceVariant,
    OnSurfaceVariant,
    InverseSurface,
    InverseOnSurface,
    Outline,
    OutlineVariant,
    Shadow,
    Scrim,
    SurfaceTint,
    Primary,
    OnPrimary,
    PrimaryContainer,
    OnPrimaryContainer,
    InversePrimary,
    PrimaryFixed,
    PrimaryFixedDim,
    OnPrimaryFixed,
    OnPrimaryFixedVariant,
    Secondary,
    OnSecondary,
    SecondaryContainer,
    OnSecondaryContainer,
    SecondaryFixed,
    SecondaryFixedDim,
    OnSecondaryFixed,
    OnSecondaryFixedVariant,
    Tertiary,
    OnTertiary,
    TertiaryContainer,
    OnTertiaryContainer,
    TertiaryFixed,
    TertiaryFixedDim,
    OnTertiaryFixed,
    OnTertiaryFixedVariant,
    Error,
    OnError,
    ErrorContainer,
    OnErrorContainer,
}

/// Four independent per-side values. Used for padding and border widths, where a design may
/// legitimately want e.g. a divider only on the bottom edge rather than all four.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct EdgeInsets {
    pub top: Option<f32>,
    pub right: Option<f32>,
    pub bottom: Option<f32>,
    pub left: Option<f32>,
}

/// Four independent corner radii. A design that groups several elements into one continuous
/// shape (a Material "button group", a card whose bottom edge sits flush against another
/// element) needs to round only the outer corners -- a single uniform `corner_radius` cannot
/// express that, no matter how many nested containers it's split across.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct CornerRadii {
    pub top_left: Option<f32>,
    pub top_right: Option<f32>,
    pub bottom_left: Option<f32>,
    pub bottom_right: Option<f32>,
}

/// A fill for a container's background. `Solid` covers the common case; `LinearGradient`
/// exists because a two-tone hero surface or a subtle depth cue is a real, common design need
/// that a single flat token can't express, and the renderer (GPUI) already supports two-stop
/// linear gradients as a native background primitive -- this just exposes that.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Fill {
    Solid {
        color: SemanticColorToken,
    },
    LinearGradient {
        /// Degrees, measured clockwise from pointing up (CSS `linear-gradient` convention).
        angle: f32,
        from: SemanticColorToken,
        to: SemanticColorToken,
    },
}

/// A single drop shadow or glow. Setting `blur_radius` with no `offset` and no `spread`
/// produces a soft, centered glow -- the mechanism behind any "halo" effect around an icon or
/// badge -- rather than a directional drop shadow, which is `offset` with little or no blur.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ShadowStyle {
    pub color: SemanticColorToken,
    #[serde(default)]
    pub offset_x: f32,
    #[serde(default)]
    pub offset_y: f32,
    #[serde(default)]
    pub blur_radius: f32,
    #[serde(default)]
    pub spread_radius: f32,
    #[serde(default)]
    pub inset: bool,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ViewStyle {
    /// Uniform padding. Ignored on any side overridden by `padding_edges`.
    pub padding: Option<f32>,
    pub padding_edges: Option<EdgeInsets>,
    pub margin: Option<f32>,
    pub margin_edges: Option<EdgeInsets>,
    pub width: Option<f32>,
    pub height: Option<f32>,
    /// Uniform corner radius. Ignored on any corner overridden by `corner_radii`.
    pub corner_radius: Option<f32>,
    pub corner_radii: Option<CornerRadii>,
    pub opacity: Option<f32>,
    pub color: Option<SemanticColorToken>,
    pub background: Option<Fill>,
    pub flex_grow: Option<f32>,
    /// Uniform border width. Ignored on any side overridden by `border_edges`.
    pub border_width: Option<f32>,
    pub border_edges: Option<EdgeInsets>,
    pub border_color: Option<SemanticColorToken>,
    pub shadows: Option<Vec<ShadowStyle>>,
    pub min_width: Option<f32>,
    pub max_width: Option<f32>,
    pub min_height: Option<f32>,
    pub max_height: Option<f32>,
    pub overflow: Option<Overflow>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ContainerNode {
    pub direction: ContainerDirection,
    #[serde(default)]
    pub children: Vec<ViewNode>,
    pub style: Option<ViewStyle>,
    pub gap: Option<f32>,
    pub align_items: Option<Alignment>,
    pub justify_content: Option<Justification>,
    #[serde(default)]
    pub wrap: bool,
    pub event_id: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct TextNode {
    pub content: String,
    pub font_size: Option<f32>,
    pub bold: Option<bool>,
    pub italic: Option<bool>,
    pub text_align: Option<TextAlign>,
    pub line_height: Option<f32>,
    pub letter_spacing: Option<f32>,
    /// Clamps to this many lines, eliding the remainder with an ellipsis. A single-line clamp
    /// is the common "truncate with ellipsis" case; anything greater is a real multi-line clamp.
    pub max_lines: Option<u32>,
    pub style: Option<ViewStyle>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct IconNode {
    pub name: String,
    pub size: Option<f32>,
    pub style: Option<ViewStyle>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ImageNode {
    pub asset_path: String,
    pub width: Option<f32>,
    pub height: Option<f32>,
    pub style: Option<ViewStyle>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ButtonNode {
    pub label: String,
    pub event_id: String,
    pub style: Option<ViewStyle>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct IconButtonNode {
    pub icon_name: String,
    pub event_id: String,
    pub style: Option<ViewStyle>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ToggleNode {
    pub value: bool,
    pub event_id: String,
    pub style: Option<ViewStyle>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct SliderNode {
    pub value: f32,
    pub min: f32,
    pub max: f32,
    pub event_id: String,
    pub style: Option<ViewStyle>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct TextInputNode {
    pub placeholder: Option<String>,
    pub value: String,
    pub event_id: String,
    pub style: Option<ViewStyle>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ListNode {
    pub items: Vec<ViewNode>,
    pub style: Option<ViewStyle>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct SpacerNode {
    pub size: Option<f32>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct BadgeNode {
    pub label: String,
    pub style: Option<ViewStyle>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ProgressNode {
    pub value: f32,
    pub style: Option<ViewStyle>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct LoadingIndicatorNode {
    pub size: Option<f32>,
    pub color: Option<SemanticColorToken>,
    pub style: Option<ViewStyle>,
}

#[derive(Default)]
struct ValidationState {
    nodes: usize,
    text_bytes: usize,
    seen_event_ids: std::collections::HashSet<String>,
}

fn validate_node(
    node: &ViewNode,
    depth: usize,
    limits: ViewLimits,
    state: &mut ValidationState,
) -> Result<(), ViewValidationError> {
    state.nodes += 1;
    if depth > limits.max_depth {
        return invalid("view tree exceeds the maximum depth");
    }
    if state.nodes > limits.max_nodes {
        return invalid("view tree exceeds the maximum node count");
    }

    let mut validate_text = |value: &str| {
        state.text_bytes += value.len();
        if state.text_bytes > limits.max_text_bytes {
            invalid("view tree exceeds the maximum text size")
        } else {
            Ok(())
        }
    };

    match node {
        ViewNode::Container(container) => {
            match container.direction {
                ContainerDirection::Grid { columns } => {
                    if !(1..=64).contains(&columns) {
                        return invalid("grid columns must be between 1 and 64");
                    }
                    if container.wrap {
                        return invalid("wrap is not supported for grid direction");
                    }
                }
                ContainerDirection::Stack => {
                    if container.wrap {
                        return invalid("wrap is not supported for stack direction");
                    }
                    // Alignment and justification are meaningful for a stack: they control
                    // where each layered child sits within the stack's bounds (e.g. centering
                    // a badge over a halo behind it) without every child needing to match the
                    // stack's own size. Gap is not: stacked children overlap by definition, so
                    // there is no "between" for a gap to apply to.
                    if container.gap.is_some_and(|g| g > 0.0) {
                        return invalid("gap is not supported for stack direction");
                    }
                }
                ContainerDirection::Row | ContainerDirection::Column => {}
            }

            validate_nonnegative("container gap", container.gap)?;
            if let Some(event_id) = &container.event_id {
                validate_event_id(event_id)?;
                if !state.seen_event_ids.insert(event_id.clone()) {
                    return invalid(format!(
                        "duplicate event ID '{event_id}' found in view tree"
                    ));
                }
            }
            validate_style(container.style.as_ref())?;
            for child in &container.children {
                validate_node(child, depth + 1, limits, state)?;
            }
        }
        ViewNode::Text(text) => {
            validate_text(&text.content)?;
            validate_positive("font size", text.font_size)?;
            validate_positive("line height", text.line_height)?;
            if text.letter_spacing.is_some_and(|value| !value.is_finite()) {
                return invalid("letter spacing must be finite");
            }
            if text.max_lines == Some(0) {
                return invalid("max lines must be at least one");
            }
            validate_style(text.style.as_ref())?;
        }
        ViewNode::Icon(icon) => {
            validate_icon_name(&icon.name)?;
            validate_positive("icon size", icon.size)?;
            validate_style(icon.style.as_ref())?;
        }
        ViewNode::Image(image) => {
            validate_asset_path(&image.asset_path)?;
            validate_positive("image width", image.width)?;
            validate_positive("image height", image.height)?;
            validate_style(image.style.as_ref())?;
        }
        ViewNode::Button(button) => {
            validate_text(&button.label)?;
            validate_event_id(&button.event_id)?;
            if !state.seen_event_ids.insert(button.event_id.clone()) {
                return invalid(format!(
                    "duplicate event ID '{}' found in view tree",
                    button.event_id
                ));
            }
            validate_style(button.style.as_ref())?;
        }
        ViewNode::IconButton(button) => {
            validate_icon_name(&button.icon_name)?;
            validate_event_id(&button.event_id)?;
            if !state.seen_event_ids.insert(button.event_id.clone()) {
                return invalid(format!(
                    "duplicate event ID '{}' found in view tree",
                    button.event_id
                ));
            }
            validate_style(button.style.as_ref())?;
        }
        ViewNode::Toggle(toggle) => {
            validate_event_id(&toggle.event_id)?;
            if !state.seen_event_ids.insert(toggle.event_id.clone()) {
                return invalid(format!(
                    "duplicate event ID '{}' found in view tree",
                    toggle.event_id
                ));
            }
            validate_style(toggle.style.as_ref())?;
        }
        ViewNode::Slider(slider) => {
            if !slider.min.is_finite()
                || !slider.max.is_finite()
                || !slider.value.is_finite()
                || slider.min >= slider.max
                || !(slider.min..=slider.max).contains(&slider.value)
            {
                return invalid("slider range or value is invalid");
            }
            validate_event_id(&slider.event_id)?;
            if !state.seen_event_ids.insert(slider.event_id.clone()) {
                return invalid(format!(
                    "duplicate event ID '{}' found in view tree",
                    slider.event_id
                ));
            }
            validate_style(slider.style.as_ref())?;
        }
        ViewNode::TextInput(input) => {
            if let Some(placeholder) = &input.placeholder {
                validate_text(placeholder)?;
            }
            validate_text(&input.value)?;
            validate_event_id(&input.event_id)?;
            if !state.seen_event_ids.insert(input.event_id.clone()) {
                return invalid(format!(
                    "duplicate event ID '{}' found in view tree",
                    input.event_id
                ));
            }
            validate_style(input.style.as_ref())?;
        }
        ViewNode::List(list) => {
            if list.items.len() > limits.max_list_items {
                return invalid("list exceeds the maximum item count");
            }
            validate_style(list.style.as_ref())?;
            for item in &list.items {
                validate_node(item, depth + 1, limits, state)?;
            }
        }
        ViewNode::Spacer(spacer) => validate_nonnegative("spacer size", spacer.size)?,
        ViewNode::Divider => {}
        ViewNode::Badge(badge) => {
            validate_text(&badge.label)?;
            validate_style(badge.style.as_ref())?;
        }
        ViewNode::Progress(progress) => {
            if !progress.value.is_finite() || !(0.0..=1.0).contains(&progress.value) {
                return invalid("progress value must be between zero and one");
            }
            validate_style(progress.style.as_ref())?;
        }
        ViewNode::LoadingIndicator(indicator) => {
            if let Some(size) = indicator.size
                && (!size.is_finite() || size <= 0.0)
            {
                return invalid("loading indicator size must be positive");
            }
            validate_style(indicator.style.as_ref())?;
        }
    }
    Ok(())
}

fn validate_style(style: Option<&ViewStyle>) -> Result<(), ViewValidationError> {
    let Some(style) = style else {
        return Ok(());
    };
    validate_nonnegative("padding", style.padding)?;
    validate_nonnegative("margin", style.margin)?;
    validate_nonnegative("width", style.width)?;
    validate_nonnegative("height", style.height)?;
    validate_nonnegative("corner radius", style.corner_radius)?;
    validate_nonnegative("flex grow", style.flex_grow)?;
    validate_nonnegative("border width", style.border_width)?;
    validate_nonnegative("min width", style.min_width)?;
    validate_nonnegative("max width", style.max_width)?;
    validate_nonnegative("min height", style.min_height)?;
    validate_nonnegative("max height", style.max_height)?;
    validate_edge_insets("padding edges", style.padding_edges.as_ref())?;
    validate_edge_insets("margin edges", style.margin_edges.as_ref())?;
    validate_edge_insets("border edges", style.border_edges.as_ref())?;
    if let Some(radii) = &style.corner_radii {
        validate_nonnegative("top-left corner radius", radii.top_left)?;
        validate_nonnegative("top-right corner radius", radii.top_right)?;
        validate_nonnegative("bottom-left corner radius", radii.bottom_left)?;
        validate_nonnegative("bottom-right corner radius", radii.bottom_right)?;
    }
    if let Some(Fill::LinearGradient { angle, .. }) = &style.background
        && !angle.is_finite()
    {
        return invalid("gradient angle must be finite");
    }
    for shadow in style.shadows.iter().flatten() {
        if !shadow.blur_radius.is_finite() || shadow.blur_radius < 0.0 {
            return invalid("shadow blur radius must be finite and non-negative");
        }
        if !shadow.spread_radius.is_finite() {
            return invalid("shadow spread radius must be finite");
        }
        if !shadow.offset_x.is_finite() || !shadow.offset_y.is_finite() {
            return invalid("shadow offset must be finite");
        }
    }

    if let (Some(min_w), Some(max_w)) = (style.min_width, style.max_width)
        && min_w > max_w
    {
        return invalid("min width cannot exceed max width");
    }
    if let (Some(min_h), Some(max_h)) = (style.min_height, style.max_height)
        && min_h > max_h
    {
        return invalid("min height cannot exceed max height");
    }

    if let (Some(w), Some(min_w)) = (style.width, style.min_width)
        && w < min_w
    {
        return invalid("width cannot be less than min width");
    }
    if let (Some(w), Some(max_w)) = (style.width, style.max_width)
        && w > max_w
    {
        return invalid("width cannot exceed max width");
    }
    if let (Some(h), Some(min_h)) = (style.height, style.min_height)
        && h < min_h
    {
        return invalid("height cannot be less than min height");
    }
    if let (Some(h), Some(max_h)) = (style.height, style.max_height)
        && h > max_h
    {
        return invalid("height cannot exceed max height");
    }

    if style
        .opacity
        .is_some_and(|value| !value.is_finite() || !(0.0..=1.0).contains(&value))
    {
        return invalid("opacity must be between zero and one");
    }
    Ok(())
}

fn validate_nonnegative(field: &str, value: Option<f32>) -> Result<(), ViewValidationError> {
    if value.is_some_and(|value| !value.is_finite() || value < 0.0) {
        return invalid(format!("{field} must be finite and non-negative"));
    }
    Ok(())
}

fn validate_edge_insets(field: &str, insets: Option<&EdgeInsets>) -> Result<(), ViewValidationError> {
    let Some(insets) = insets else {
        return Ok(());
    };
    for (side, value) in [
        ("top", insets.top),
        ("right", insets.right),
        ("bottom", insets.bottom),
        ("left", insets.left),
    ] {
        if value.is_some_and(|value| !value.is_finite() || value < 0.0) {
            return invalid(format!("{field} {side} must be finite and non-negative"));
        }
    }
    Ok(())
}

fn validate_positive(field: &str, value: Option<f32>) -> Result<(), ViewValidationError> {
    if value.is_some_and(|value| !value.is_finite() || value <= 0.0) {
        return invalid(format!("{field} must be finite and positive"));
    }
    Ok(())
}

fn validate_event_id(value: &str) -> Result<(), ViewValidationError> {
    if value.is_empty()
        || value.len() > 128
        || value
            .bytes()
            .any(|byte| !(byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.')))
    {
        return invalid("event ID has an invalid format");
    }
    Ok(())
}

fn validate_icon_name(value: &str) -> Result<(), ViewValidationError> {
    if value.is_empty()
        || value.len() > 64
        || value.bytes().any(|byte| {
            !(byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'-' | b'_'))
        })
    {
        return invalid("icon name has an invalid format");
    }
    Ok(())
}

fn validate_asset_path(value: &str) -> Result<(), ViewValidationError> {
    let path = Path::new(value);
    if value.is_empty()
        || path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return invalid("image asset path must be a safe relative path");
    }
    Ok(())
}

fn invalid<T>(message: impl Into<String>) -> Result<T, ViewValidationError> {
    Err(ViewValidationError(message.into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_enums_have_stable_snake_case_serde_and_schema_coverage() {
        let dir_json = serde_json::to_string(&ContainerDirection::Grid { columns: 4 }).unwrap();
        assert_eq!(dir_json, r#"{"grid":{"columns":4}}"#);
        let parsed_dir: ContainerDirection = serde_json::from_str(&dir_json).unwrap();
        assert_eq!(parsed_dir, ContainerDirection::Grid { columns: 4 });

        let align_json = serde_json::to_string(&Alignment::Center).unwrap();
        assert_eq!(align_json, r#""center""#);

        let just_json = serde_json::to_string(&Justification::SpaceBetween).unwrap();
        assert_eq!(just_json, r#""space_between""#);

        let overflow_json = serde_json::to_string(&Overflow::Scroll).unwrap();
        assert_eq!(overflow_json, r#""scroll""#);

        let _schema_dir = schemars::schema_for!(ContainerDirection);
        let _schema_align = schemars::schema_for!(Alignment);
        let _schema_just = schemars::schema_for!(Justification);
        let _schema_overflow = schemars::schema_for!(Overflow);
    }

    #[test]
    fn grid_column_validation_accepts_1_and_64_rejects_0_and_65() {
        let tree_1 = ViewTree::new(ViewNode::Container(ContainerNode {
            direction: ContainerDirection::Grid { columns: 1 },
            children: vec![],
            style: None,
            gap: None,
            align_items: None,
            justify_content: None,
            wrap: false,
            event_id: None,
        }));
        assert!(tree_1.validate(ViewLimits::default()).is_ok());

        let tree_64 = ViewTree::new(ViewNode::Container(ContainerNode {
            direction: ContainerDirection::Grid { columns: 64 },
            children: vec![],
            style: None,
            gap: None,
            align_items: None,
            justify_content: None,
            wrap: false,
            event_id: None,
        }));
        assert!(tree_64.validate(ViewLimits::default()).is_ok());

        let tree_0 = ViewTree::new(ViewNode::Container(ContainerNode {
            direction: ContainerDirection::Grid { columns: 0 },
            children: vec![],
            style: None,
            gap: None,
            align_items: None,
            justify_content: None,
            wrap: false,
            event_id: None,
        }));
        assert!(tree_0.validate(ViewLimits::default()).is_err());

        let tree_65 = ViewTree::new(ViewNode::Container(ContainerNode {
            direction: ContainerDirection::Grid { columns: 65 },
            children: vec![],
            style: None,
            gap: None,
            align_items: None,
            justify_content: None,
            wrap: false,
            event_id: None,
        }));
        assert!(tree_65.validate(ViewLimits::default()).is_err());
    }

    #[test]
    fn numeric_fields_reject_nan_infinity_and_negatives() {
        let invalid_styles = vec![
            ViewStyle {
                border_width: Some(f32::NAN),
                ..ViewStyle::default()
            },
            ViewStyle {
                border_width: Some(f32::INFINITY),
                ..ViewStyle::default()
            },
            ViewStyle {
                border_width: Some(-1.0),
                ..ViewStyle::default()
            },
            ViewStyle {
                min_width: Some(-5.0),
                ..ViewStyle::default()
            },
            ViewStyle {
                max_width: Some(f32::NAN),
                ..ViewStyle::default()
            },
            ViewStyle {
                min_height: Some(f32::NEG_INFINITY),
                ..ViewStyle::default()
            },
            ViewStyle {
                max_height: Some(-0.1),
                ..ViewStyle::default()
            },
        ];

        for style in invalid_styles {
            let tree = ViewTree::new(ViewNode::Container(ContainerNode {
                direction: ContainerDirection::Row,
                children: vec![],
                style: Some(style),
                gap: None,
                align_items: None,
                justify_content: None,
                wrap: false,
                event_id: None,
            }));
            assert!(tree.validate(ViewLimits::default()).is_err());
        }

        let invalid_gap_tree = ViewTree::new(ViewNode::Container(ContainerNode {
            direction: ContainerDirection::Row,
            children: vec![],
            style: None,
            gap: Some(-1.0),
            align_items: None,
            justify_content: None,
            wrap: false,
            event_id: None,
        }));
        assert!(invalid_gap_tree.validate(ViewLimits::default()).is_err());
    }

    #[test]
    fn every_new_numeric_field_rejects_all_non_finite_and_negative_values() {
        let invalid_values = [f32::NAN, f32::INFINITY, f32::NEG_INFINITY, -1.0];
        for value in invalid_values {
            for style in [
                ViewStyle {
                    border_width: Some(value),
                    ..ViewStyle::default()
                },
                ViewStyle {
                    min_width: Some(value),
                    ..ViewStyle::default()
                },
                ViewStyle {
                    max_width: Some(value),
                    ..ViewStyle::default()
                },
                ViewStyle {
                    min_height: Some(value),
                    ..ViewStyle::default()
                },
                ViewStyle {
                    max_height: Some(value),
                    ..ViewStyle::default()
                },
            ] {
                let tree = ViewTree::new(ViewNode::Container(ContainerNode {
                    direction: ContainerDirection::Row,
                    children: vec![],
                    style: Some(style),
                    gap: None,
                    align_items: None,
                    justify_content: None,
                    wrap: false,
                    event_id: None,
                }));
                assert!(tree.validate(ViewLimits::default()).is_err());
            }

            let tree = ViewTree::new(ViewNode::Container(ContainerNode {
                direction: ContainerDirection::Row,
                children: vec![],
                style: None,
                gap: Some(value),
                align_items: None,
                justify_content: None,
                wrap: false,
                event_id: None,
            }));
            assert!(tree.validate(ViewLimits::default()).is_err());
        }
    }

    #[test]
    fn min_max_and_exact_size_contradictions_are_rejected() {
        // min_width > max_width
        let tree1 = ViewTree::new(ViewNode::Container(ContainerNode {
            direction: ContainerDirection::Row,
            children: vec![],
            style: Some(ViewStyle {
                min_width: Some(100.0),
                max_width: Some(50.0),
                ..ViewStyle::default()
            }),
            gap: None,
            align_items: None,
            justify_content: None,
            wrap: false,
            event_id: None,
        }));
        assert!(tree1.validate(ViewLimits::default()).is_err());

        // min_height > max_height
        let tree2 = ViewTree::new(ViewNode::Container(ContainerNode {
            direction: ContainerDirection::Row,
            children: vec![],
            style: Some(ViewStyle {
                min_height: Some(200.0),
                max_height: Some(100.0),
                ..ViewStyle::default()
            }),
            gap: None,
            align_items: None,
            justify_content: None,
            wrap: false,
            event_id: None,
        }));
        assert!(tree2.validate(ViewLimits::default()).is_err());

        // width < min_width
        let tree3 = ViewTree::new(ViewNode::Container(ContainerNode {
            direction: ContainerDirection::Row,
            children: vec![],
            style: Some(ViewStyle {
                width: Some(30.0),
                min_width: Some(50.0),
                ..ViewStyle::default()
            }),
            gap: None,
            align_items: None,
            justify_content: None,
            wrap: false,
            event_id: None,
        }));
        assert!(tree3.validate(ViewLimits::default()).is_err());

        // width > max_width
        let tree4 = ViewTree::new(ViewNode::Container(ContainerNode {
            direction: ContainerDirection::Row,
            children: vec![],
            style: Some(ViewStyle {
                width: Some(150.0),
                max_width: Some(100.0),
                ..ViewStyle::default()
            }),
            gap: None,
            align_items: None,
            justify_content: None,
            wrap: false,
            event_id: None,
        }));
        assert!(tree4.validate(ViewLimits::default()).is_err());

        // height < min_height
        let tree5 = ViewTree::new(ViewNode::Container(ContainerNode {
            direction: ContainerDirection::Row,
            children: vec![],
            style: Some(ViewStyle {
                height: Some(20.0),
                min_height: Some(40.0),
                ..ViewStyle::default()
            }),
            gap: None,
            align_items: None,
            justify_content: None,
            wrap: false,
            event_id: None,
        }));
        assert!(tree5.validate(ViewLimits::default()).is_err());

        // height > max_height
        let tree6 = ViewTree::new(ViewNode::Container(ContainerNode {
            direction: ContainerDirection::Row,
            children: vec![],
            style: Some(ViewStyle {
                height: Some(120.0),
                max_height: Some(100.0),
                ..ViewStyle::default()
            }),
            gap: None,
            align_items: None,
            justify_content: None,
            wrap: false,
            event_id: None,
        }));
        assert!(tree6.validate(ViewLimits::default()).is_err());
    }

    #[test]
    fn stack_rejects_unsupported_properties_and_grid_stack_reject_wrap() {
        // Stack with align_items -- valid: it positions each layered child within the
        // stack's bounds (e.g. centering a badge over a halo behind it).
        let tree1 = ViewTree::new(ViewNode::Container(ContainerNode {
            direction: ContainerDirection::Stack,
            children: vec![],
            style: None,
            gap: None,
            align_items: Some(Alignment::Center),
            justify_content: None,
            wrap: false,
            event_id: None,
        }));
        assert!(tree1.validate(ViewLimits::default()).is_ok());

        // Stack with justify_content -- also valid, same reasoning.
        let tree2 = ViewTree::new(ViewNode::Container(ContainerNode {
            direction: ContainerDirection::Stack,
            children: vec![],
            style: None,
            gap: None,
            align_items: None,
            justify_content: Some(Justification::Center),
            wrap: false,
            event_id: None,
        }));
        assert!(tree2.validate(ViewLimits::default()).is_ok());

        // Stack with gap > 0 -- still rejected: stacked children overlap by definition, so
        // there is no "between" for a gap to apply to.
        let tree3 = ViewTree::new(ViewNode::Container(ContainerNode {
            direction: ContainerDirection::Stack,
            children: vec![],
            style: None,
            gap: Some(8.0),
            align_items: None,
            justify_content: None,
            wrap: false,
            event_id: None,
        }));
        assert!(tree3.validate(ViewLimits::default()).is_err());

        // Stack with wrap
        let tree4 = ViewTree::new(ViewNode::Container(ContainerNode {
            direction: ContainerDirection::Stack,
            children: vec![],
            style: None,
            gap: None,
            align_items: None,
            justify_content: None,
            wrap: true,
            event_id: None,
        }));
        assert!(tree4.validate(ViewLimits::default()).is_err());

        // Grid with wrap
        let tree5 = ViewTree::new(ViewNode::Container(ContainerNode {
            direction: ContainerDirection::Grid { columns: 3 },
            children: vec![],
            style: None,
            gap: None,
            align_items: None,
            justify_content: None,
            wrap: true,
            event_id: None,
        }));
        assert!(tree5.validate(ViewLimits::default()).is_err());
    }

    #[test]
    fn container_event_id_syntax_rules_and_duplicate_rejection() {
        // Valid container event ID
        let tree_valid = ViewTree::new(ViewNode::Container(ContainerNode {
            direction: ContainerDirection::Row,
            children: vec![],
            style: None,
            gap: None,
            align_items: None,
            justify_content: None,
            wrap: false,
            event_id: Some("card_click".into()),
        }));
        assert!(tree_valid.validate(ViewLimits::default()).is_ok());

        // Invalid container event ID syntax
        let tree_invalid_syntax = ViewTree::new(ViewNode::Container(ContainerNode {
            direction: ContainerDirection::Row,
            children: vec![],
            style: None,
            gap: None,
            align_items: None,
            justify_content: None,
            wrap: false,
            event_id: Some("invalid click!".into()),
        }));
        assert!(tree_invalid_syntax.validate(ViewLimits::default()).is_err());

        // Duplicate event IDs across container and button
        let tree_duplicate = ViewTree::new(ViewNode::Container(ContainerNode {
            direction: ContainerDirection::Row,
            children: vec![
                ViewNode::Button(ButtonNode {
                    label: "Click".into(),
                    event_id: "shared_id".into(),
                    style: None,
                }),
                ViewNode::Container(ContainerNode {
                    direction: ContainerDirection::Column,
                    children: vec![],
                    style: None,
                    gap: None,
                    align_items: None,
                    justify_content: None,
                    wrap: false,
                    event_id: Some("shared_id".into()),
                }),
            ],
            style: None,
            gap: None,
            align_items: None,
            justify_content: None,
            wrap: false,
            event_id: None,
        }));
        assert!(tree_duplicate.validate(ViewLimits::default()).is_err());

        // Distinct event IDs across container and button
        let tree_distinct = ViewTree::new(ViewNode::Container(ContainerNode {
            direction: ContainerDirection::Row,
            children: vec![
                ViewNode::Button(ButtonNode {
                    label: "Click".into(),
                    event_id: "btn_id".into(),
                    style: None,
                }),
                ViewNode::Container(ContainerNode {
                    direction: ContainerDirection::Column,
                    children: vec![],
                    style: None,
                    gap: None,
                    align_items: None,
                    justify_content: None,
                    wrap: false,
                    event_id: Some("card_id".into()),
                }),
            ],
            style: None,
            gap: None,
            align_items: None,
            justify_content: None,
            wrap: false,
            event_id: None,
        }));
        assert!(tree_distinct.validate(ViewLimits::default()).is_ok());
    }
}
