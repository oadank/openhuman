//! Shared Composio execute path: prepare args and error mapping.

use super::client::{direct_execute, ComposioClientKind};
use super::error_mapping::{format_provider_error, remap_transport_error};
use super::execute_prepare::prepare_execute_arguments;
use super::types::ComposioExecuteResponse;

/// Direct tenant variant used by the per-action tool surface,
/// dispatcher tool, and RPC `composio_execute` op.
pub async fn execute_composio_action_kind(
    kind: ComposioClientKind,
    tool: &str,
    arguments: Option<serde_json::Value>,
    entity_id: &str,
) -> Result<ComposioExecuteResponse, String> {
    let tool_trim = tool.trim();
    if tool_trim.is_empty() {
        return Err("composio: tool slug must not be empty".to_string());
    }

    let prepared = match prepare_execute_arguments(tool_trim, arguments) {
        Ok(args) => args,
        Err(msg) => {
            tracing::debug!(
                tool = %tool_trim,
                error = %msg,
                "[composio][prepare] local validation rejected execute"
            );
            return Err(format_provider_error(tool_trim, &msg));
        }
    };

    let ComposioClientKind::Direct(direct) = kind;
    tracing::debug!(tool = %tool_trim, "[composio][dispatch] direct variant");
    match direct_execute(&direct, tool_trim, Some(prepared), entity_id).await {
        Ok(resp) => Ok(format_response(tool_trim, resp)),
        Err(e) => Err(remap_transport_error(tool_trim, &e.to_string())),
    }
}

fn format_response(tool: &str, resp: ComposioExecuteResponse) -> ComposioExecuteResponse {
    if resp.successful {
        return resp;
    }
    let raw_err = resp
        .error
        .clone()
        .unwrap_or_else(|| "provider reported failure".to_string());
    ComposioExecuteResponse {
        error: Some(format_provider_error(tool, &raw_err)),
        ..resp
    }
}
