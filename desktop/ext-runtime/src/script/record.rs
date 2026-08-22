use serde::{Deserialize, Serialize};
use shilpo_ext_api::{
    Alignment, ContainerDirection, ContainerNode, ContributionId, IconNode, SemanticColorToken,
    TextNode, ViewLimits, ViewNode, ViewStyle, ViewTree,
};

use super::manifest::ScriptManifest;

pub const MAX_RECORD_BYTES: usize = 1_048_576; // 1 MiB

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ScriptRecord {
    pub schema_version: u32,
    pub contribution: ContributionId,
    #[serde(flatten)]
    pub payload: ScriptRecordPayload,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ScriptRecordPayload {
    View {
        view: ViewTree,
    },
    Text {
        text: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tooltip: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        icon: Option<String>,
    },
}

pub fn decode_and_validate_record(
    json_bytes: &[u8],
    manifest: &ScriptManifest,
) -> Result<(ContributionId, ViewTree), String> {
    if json_bytes.len() > MAX_RECORD_BYTES {
        return Err(format!(
            "record size ({} bytes) exceeds the 1 MiB limit",
            json_bytes.len()
        ));
    }

    let json_str = std::str::from_utf8(json_bytes)
        .map_err(|e| format!("invalid UTF-8 in script record: {e}"))?;

    let record = serde_json::from_str::<ScriptRecord>(json_str)
        .map_err(|e| format!("invalid JSON script record: {e}"))?;

    if record.schema_version != 1 {
        return Err(format!(
            "unsupported record schema_version {}; expected 1",
            record.schema_version
        ));
    }

    let valid_contrib = manifest
        .contributions
        .bar_widgets
        .iter()
        .any(|w| w.id == record.contribution);
    if !valid_contrib {
        return Err(format!(
            "unknown contribution ID '{}' in script record",
            record.contribution
        ));
    }

    let view_tree = match record.payload {
        ScriptRecordPayload::View { view } => view,
        ScriptRecordPayload::Text {
            text,
            tooltip,
            icon,
        } => {
            if tooltip
                .as_ref()
                .is_some_and(|value| value.trim().is_empty())
            {
                return Err("tooltip cannot be empty when provided".into());
            }
            let semantic_style = Some(ViewStyle {
                color: Some(SemanticColorToken::OnSurface),
                ..ViewStyle::default()
            });
            let root = if let Some(icon_name) = icon {
                if icon_name.trim().is_empty() {
                    return Err("icon name cannot be empty".into());
                }
                ViewNode::Container(ContainerNode {
                    direction: ContainerDirection::Row,
                    children: vec![
                        ViewNode::Icon(IconNode {
                            name: icon_name,
                            size: None,
                            style: Some(ViewStyle {
                                color: Some(SemanticColorToken::OnSurfaceVariant),
                                ..ViewStyle::default()
                            }),
                        }),
                        ViewNode::Text(TextNode {
                            content: text,
                            style: semantic_style,
                            ..Default::default()
                        }),
                    ],
                    style: None,
                    gap: Some(6.0),
                    align_items: Some(Alignment::Center),
                    justify_content: None,
                    wrap: false,
                    event_id: None,
                })
            } else {
                ViewNode::Text(TextNode {
                    content: text,
                    style: semantic_style,
                    ..Default::default()
                })
            };
            ViewTree::new(root)
        }
    };

    view_tree
        .validate(ViewLimits::default())
        .map_err(|e| format!("invalid view tree in script record: {e}"))?;
    validate_read_only_node(&view_tree.root)?;

    Ok((record.contribution, view_tree))
}

fn validate_read_only_node(node: &ViewNode) -> Result<(), String> {
    match node {
        ViewNode::Button(_)
        | ViewNode::IconButton(_)
        | ViewNode::Toggle(_)
        | ViewNode::Slider(_)
        | ViewNode::TextInput(_) => {
            Err("script bar widgets are read-only in v1; interactive nodes are rejected".into())
        }
        ViewNode::Container(container) => {
            if container.event_id.is_some() {
                return Err(
                    "script bar widgets are read-only in v1; event IDs are rejected".into(),
                );
            }
            for child in &container.children {
                validate_read_only_node(child)?;
            }
            Ok(())
        }
        ViewNode::List(list) => {
            for item in &list.items {
                validate_read_only_node(item)?;
            }
            Ok(())
        }
        _ => Ok(()),
    }
}
