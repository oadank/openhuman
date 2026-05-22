//! Controller schemas for the `tools` namespace.
//!
//! Exposes a small allowlist of tool-like operations to the Tauri shell
//! over JSON-RPC. The Tauri host needs these so the onboarding flow can
//! drive Composio and direct-provider tools itself. Anything **not** in
//! this file remains agent-only.

use serde_json::{json, Map, Value};

use crate::core::all::{ControllerFuture, RegisteredController};
use crate::core::{ControllerSchema, FieldSchema, TypeSchema};
use crate::openhuman::config::rpc as config_rpc;
use crate::openhuman::tools::traits::Tool;
use crate::rpc::RpcOutcome;

pub fn all_controller_schemas() -> Vec<ControllerSchema> {
    vec![
        tools_schemas("tools_composio_execute"),
        tools_schemas("tools_seltz_search"),
    ]
}

pub fn all_registered_controllers() -> Vec<RegisteredController> {
    vec![
        RegisteredController {
            schema: tools_schemas("tools_composio_execute"),
            handler: handle_composio_execute,
        },
        RegisteredController {
            schema: tools_schemas("tools_seltz_search"),
            handler: handle_seltz_search,
        },
    ]
}

pub fn tools_schemas(function: &str) -> ControllerSchema {
    match function {
        "tools_composio_execute" => ControllerSchema {
            namespace: "tools",
            function: "composio_execute",
            description: "Execute a Composio action. Routes through the mode-aware \
                          factory: backend mode proxies via the OpenHuman backend; \
                          direct mode calls backend.composio.dev with the user's own \
                          API key. Exposed for Tauri-driven flows (e.g. onboarding) \
                          that orchestrate tool calls themselves.",
            inputs: vec![
                FieldSchema {
                    name: "action",
                    ty: TypeSchema::String,
                    comment: "Composio action slug (e.g. `GMAIL_FETCH_EMAILS`).",
                    required: true,
                },
                FieldSchema {
                    name: "params",
                    ty: TypeSchema::Json,
                    comment: "Action parameters object passed straight through to Composio.",
                    required: false,
                },
            ],
            outputs: vec![
                FieldSchema {
                    name: "successful",
                    ty: TypeSchema::Bool,
                    comment: "Whether the upstream provider reported success.",
                    required: true,
                },
                FieldSchema {
                    name: "data",
                    ty: TypeSchema::Json,
                    comment: "Raw provider response.",
                    required: true,
                },
                FieldSchema {
                    name: "error",
                    ty: TypeSchema::Option(Box::new(TypeSchema::String)),
                    comment: "Provider error message if `successful` is false.",
                    required: false,
                },
            ],
        },
        "tools_seltz_search" => ControllerSchema {
            namespace: "tools",
            function: "seltz_search",
            description: "Web search via the Seltz API. Returns structured results with \
                          URLs, content, and optional published dates. Supports domain \
                          filtering, date ranges, and news scope.",
            inputs: vec![
                FieldSchema {
                    name: "query",
                    ty: TypeSchema::String,
                    comment: "Search query string.",
                    required: true,
                },
                FieldSchema {
                    name: "max_results",
                    ty: TypeSchema::Option(Box::new(TypeSchema::U64)),
                    comment: "Max results (1-20, default 10).",
                    required: false,
                },
                FieldSchema {
                    name: "include_domains",
                    ty: TypeSchema::Option(Box::new(TypeSchema::Array(Box::new(
                        TypeSchema::String,
                    )))),
                    comment: "Restrict results to these domains.",
                    required: false,
                },
                FieldSchema {
                    name: "exclude_domains",
                    ty: TypeSchema::Option(Box::new(TypeSchema::Array(Box::new(
                        TypeSchema::String,
                    )))),
                    comment: "Exclude results from these domains.",
                    required: false,
                },
                FieldSchema {
                    name: "from_date",
                    ty: TypeSchema::Option(Box::new(TypeSchema::String)),
                    comment: "Only results published on or after (YYYY-MM-DD).",
                    required: false,
                },
                FieldSchema {
                    name: "to_date",
                    ty: TypeSchema::Option(Box::new(TypeSchema::String)),
                    comment: "Only results published on or before (YYYY-MM-DD).",
                    required: false,
                },
                FieldSchema {
                    name: "scope",
                    ty: TypeSchema::Option(Box::new(TypeSchema::String)),
                    comment: "Restrict to a scope, e.g. \"news\".",
                    required: false,
                },
            ],
            outputs: vec![FieldSchema {
                name: "documents",
                ty: TypeSchema::Array(Box::new(TypeSchema::Json)),
                comment: "Each item: {url, content, title?, published_date?}.",
                required: true,
            }],
        },
        _ => ControllerSchema {
            namespace: "tools",
            function: "unknown",
            description: "Unknown tools controller.",
            inputs: vec![],
            outputs: vec![FieldSchema {
                name: "error",
                ty: TypeSchema::String,
                comment: "Lookup error details.",
                required: true,
            }],
        },
    }
}

fn handle_composio_execute(params: Map<String, Value>) -> ControllerFuture {
    Box::pin(async move {
        let action = params
            .get("action")
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| "missing required `action`".to_string())?;
        let action_args = params.get("params").cloned();

        let config = config_rpc::load_config_with_timeout().await?;
        use crate::openhuman::composio::client::{
            create_composio_client, direct_execute, ComposioClientKind,
        };
        let ComposioClientKind::Direct(direct) =
            create_composio_client(&config).map_err(|e| format!("tools.composio_execute: {e}"))?;
        tracing::debug!(
            action = %action,
            mode = %config.composio.mode,
            "[tools][composio_execute] executing action"
        );
        let resp = direct_execute(&direct, &action, action_args, &config.composio.entity_id)
            .await
            .map_err(|e| format!("composio execute_tool failed: {e:#}"))?;
        tracing::debug!(
            action = %action,
            successful = resp.successful,
            "[tools][composio_execute] complete"
        );

        let payload = json!({
            "successful": resp.successful,
            "data": resp.data,
            "error": resp.error,
            "cost_usd": resp.cost_usd,
            "markdown_formatted": resp.markdown_formatted,
        });
        let log = vec![format!(
            "tools.composio_execute: action={action} successful={}",
            resp.successful
        )];
        RpcOutcome::new(payload, log).into_cli_compatible_json()
    })
}

fn handle_seltz_search(params: Map<String, Value>) -> ControllerFuture {
    Box::pin(async move {
        let query = params
            .get("query")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .ok_or_else(|| "missing or empty `query`".to_string())?;
        let max_results = params
            .get("max_results")
            .and_then(Value::as_u64)
            .map(|n| n.clamp(1, 20) as usize)
            .unwrap_or(10);

        let config = config_rpc::load_config_with_timeout().await?;

        if !config.seltz.enabled {
            tracing::debug!("[rpc][tools.seltz_search] seltz disabled — rejecting");
            return Err("Seltz search is not enabled. Set SELTZ_API_KEY to enable.".to_string());
        }

        let has_include_domains = params.get("include_domains").is_some();
        let has_exclude_domains = params.get("exclude_domains").is_some();
        let has_scope = params.get("scope").is_some();

        tracing::debug!(
            query_len = query.chars().count(),
            max_results,
            has_include_domains,
            has_exclude_domains,
            has_scope,
            "[rpc][tools.seltz_search] start"
        );

        let tool = crate::openhuman::integrations::SeltzSearchTool::new(
            config.seltz.api_key.clone(),
            config.seltz.api_url.clone(),
            max_results,
            config.seltz.timeout_secs,
        );

        // Build args JSON with all optional fields.
        let mut args = json!({ "query": query, "max_results": max_results });
        let args_map = args.as_object_mut().unwrap();
        if let Some(v) = params.get("include_domains") {
            args_map.insert("include_domains".to_string(), v.clone());
        }
        if let Some(v) = params.get("exclude_domains") {
            args_map.insert("exclude_domains".to_string(), v.clone());
        }
        if let Some(v) = params.get("from_date") {
            args_map.insert("from_date".to_string(), v.clone());
        }
        if let Some(v) = params.get("to_date") {
            args_map.insert("to_date".to_string(), v.clone());
        }
        if let Some(v) = params.get("scope") {
            args_map.insert("scope".to_string(), v.clone());
        }

        let result = tool
            .execute(args)
            .await
            .map_err(|e| format!("seltz search failed: {e:#}"))?;

        let payload = json!({ "documents": result.output() });
        let log = vec![format!(
            "[rpc][tools.seltz_search] success query_len={} max_results={}",
            query.chars().count(),
            max_results
        )];
        RpcOutcome::new(payload, log).into_cli_compatible_json()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_schemas_returns_two() {
        assert_eq!(all_controller_schemas().len(), 2);
    }

    #[test]
    fn all_controllers_returns_two() {
        assert_eq!(all_registered_controllers().len(), 2);
    }

    #[test]
    fn composio_execute_schema_shape() {
        let s = tools_schemas("tools_composio_execute");
        assert_eq!(s.namespace, "tools");
        assert_eq!(s.function, "composio_execute");
        assert!(s.inputs.iter().any(|f| f.name == "action" && f.required));
    }

    #[test]
    fn seltz_search_schema_shape() {
        let s = tools_schemas("tools_seltz_search");
        assert_eq!(s.namespace, "tools");
        assert_eq!(s.function, "seltz_search");
        assert!(s.inputs.iter().any(|f| f.name == "query" && f.required));
        assert!(s.inputs.iter().any(|f| f.name == "include_domains"));
        assert!(s.inputs.iter().any(|f| f.name == "scope"));
    }

    #[test]
    fn unknown_function_returns_unknown() {
        let s = tools_schemas("nonexistent");
        assert_eq!(s.function, "unknown");
    }
}
