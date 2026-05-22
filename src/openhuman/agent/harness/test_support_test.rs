//! Behavioural tests of the agent harness driven by the smart mock
//! provider in [`super::test_support`]. These exercise the real
//! [`run_tool_call_loop`] path end-to-end — no provider stubbing inside
//! the test bodies — and surface regressions in tool dispatch, parsing,
//! and history threading.

use super::test_support::{KeywordRule, KeywordScriptedProvider, ScriptedToolCall};
use super::tool_loop::run_tool_call_loop;
use crate::openhuman::inference::provider::{ChatMessage, ChatRequest, ChatResponse, Provider};
use crate::openhuman::tools::traits::{PermissionLevel, Tool, ToolCategory, ToolResult, ToolScope};
use async_trait::async_trait;
use serde_json::json;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

fn mm() -> crate::openhuman::config::MultimodalConfig {
    crate::openhuman::config::MultimodalConfig::default()
}

#[tokio::test]
async fn keyword_provider_records_forced_then_fallback_turns() {
    let provider =
        KeywordScriptedProvider::new(vec![KeywordRule::final_reply("matched", "final answer")])
            .with_native_tools(true)
            .with_vision(true)
            .with_fallback("fallback reply");

    let caps = provider.capabilities();
    assert!(caps.native_tool_calling);
    assert!(caps.vision);

    provider.push_forced_response(ChatResponse {
        text: Some("forced reply".into()),
        tool_calls: vec![],
        usage: None,
    });

    let messages = vec![ChatMessage::user("nothing should match here")];
    let forced = provider
        .chat(
            ChatRequest {
                messages: &messages,
                tools: None,
                stream: None,
            },
            "test-model",
            0.0,
        )
        .await
        .expect("forced response");
    assert_eq!(forced.text.as_deref(), Some("forced reply"));

    let fallback = provider
        .chat(
            ChatRequest {
                messages: &messages,
                tools: None,
                stream: None,
            },
            "test-model",
            0.0,
        )
        .await
        .expect("fallback response");
    assert_eq!(fallback.text.as_deref(), Some("fallback reply"));

    let turns = provider.turns();
    assert_eq!(turns.len(), 2);
    assert_eq!(turns[0].rule_keyword, None);
    assert_eq!(turns[0].emitted_text.as_deref(), Some("forced reply"));
    assert_eq!(turns[1].rule_keyword, None);
    assert_eq!(turns[1].emitted_text.as_deref(), Some("fallback reply"));
}

#[tokio::test]
async fn keyword_provider_prompt_guided_text_wraps_tool_calls_and_honors_fire_limit() {
    let provider = KeywordScriptedProvider::new(vec![KeywordRule::tool_call(
        "search please",
        ScriptedToolCall::new("search_tool", json!({"q": "rust"})),
    )
    .with_text("Looking it up.")]);

    let messages = vec![
        ChatMessage::assistant("earlier assistant turn"),
        ChatMessage::tool("search please from a tool result"),
    ];

    let first = provider
        .chat(
            ChatRequest {
                messages: &messages,
                tools: None,
                stream: None,
            },
            "test-model",
            0.0,
        )
        .await
        .expect("prompt-guided response");

    let text = first.text.expect("prompt-guided text body");
    assert!(first.tool_calls.is_empty());
    assert!(text.starts_with("Looking it up.\n"));
    assert!(text.contains("<tool_call>"));
    assert!(text.contains("\"name\":\"search_tool\""));
    assert!(text.contains("\"q\":\"rust\""));

    let second = provider
        .chat(
            ChatRequest {
                messages: &messages,
                tools: None,
                stream: None,
            },
            "test-model",
            0.0,
        )
        .await
        .expect("fallback after max_fires");
    assert_eq!(second.text.as_deref(), Some("done"));
    assert_eq!(provider.turn_count(), 2);
}

/// Generic test tool: records the args it was called with and returns
/// whatever was wired at construction.
struct RecordingTool {
    name_str: String,
    description_str: String,
    result: ToolResult,
    calls: Arc<parking_lot::Mutex<Vec<serde_json::Value>>>,
    permission: PermissionLevel,
    scope_v: ToolScope,
    category_v: ToolCategory,
}

impl RecordingTool {
    fn echo(name: &str) -> (Self, Arc<parking_lot::Mutex<Vec<serde_json::Value>>>) {
        let calls = Arc::new(parking_lot::Mutex::new(Vec::new()));
        let tool = Self {
            name_str: name.to_string(),
            description_str: format!("recording tool {name}"),
            result: ToolResult::success(format!("{name}-ok")),
            calls: calls.clone(),
            permission: PermissionLevel::ReadOnly,
            scope_v: ToolScope::All,
            category_v: ToolCategory::System,
        };
        (tool, calls)
    }
}

struct SequencedTool {
    name_str: String,
    result: ToolResult,
    calls: Arc<parking_lot::Mutex<Vec<serde_json::Value>>>,
    sequence: Arc<parking_lot::Mutex<Vec<String>>>,
}

impl SequencedTool {
    fn new(
        name: &str,
        result: ToolResult,
        sequence: Arc<parking_lot::Mutex<Vec<String>>>,
    ) -> (Self, Arc<parking_lot::Mutex<Vec<serde_json::Value>>>) {
        let calls = Arc::new(parking_lot::Mutex::new(Vec::new()));
        (
            Self {
                name_str: name.to_string(),
                result,
                calls: calls.clone(),
                sequence,
            },
            calls,
        )
    }
}

#[async_trait]
impl Tool for SequencedTool {
    fn name(&self) -> &str {
        &self.name_str
    }
    fn description(&self) -> &str {
        &self.name_str
    }
    fn parameters_schema(&self) -> serde_json::Value {
        json!({"type": "object", "additionalProperties": true})
    }
    fn permission_level(&self) -> PermissionLevel {
        PermissionLevel::ReadOnly
    }
    fn scope(&self) -> ToolScope {
        ToolScope::All
    }
    fn category(&self) -> ToolCategory {
        ToolCategory::System
    }
    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        self.sequence.lock().push(self.name_str.clone());
        self.calls.lock().push(args);
        Ok(self.result.clone())
    }
}

#[async_trait]
impl Tool for RecordingTool {
    fn name(&self) -> &str {
        &self.name_str
    }
    fn description(&self) -> &str {
        &self.description_str
    }
    fn parameters_schema(&self) -> serde_json::Value {
        json!({"type": "object", "additionalProperties": true})
    }
    fn permission_level(&self) -> PermissionLevel {
        self.permission
    }
    fn scope(&self) -> ToolScope {
        self.scope_v
    }
    fn category(&self) -> ToolCategory {
        self.category_v
    }
    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        self.calls.lock().push(args);
        Ok(self.result.clone())
    }
}

// ── 1. Keyword-driven loop: prompt-guided XML path ────────────────

#[tokio::test]
async fn keyword_provider_drives_prompt_guided_tool_loop_to_completion() {
    let provider = KeywordScriptedProvider::new(vec![
        KeywordRule::tool_call(
            "search",
            ScriptedToolCall::new("search_tool", json!({"q": "rust"})),
        )
        .with_text("Looking it up."),
        KeywordRule::final_reply("search_tool-ok", "Here is the answer."),
    ]);

    let (search_tool, search_calls) = RecordingTool::echo("search_tool");
    let tools: Vec<Box<dyn Tool>> = vec![Box::new(search_tool)];

    let mut history = vec![ChatMessage::user("please search the web for rust news")];

    let result = run_tool_call_loop(
        &provider,
        &mut history,
        &tools,
        "mock",
        "test-model",
        0.0,
        true,
        None,
        "channel",
        &mm(),
        5,
        None,
        None,
        &[],
        None,
        None,
    )
    .await
    .expect("loop should complete");

    assert_eq!(result, "Here is the answer.");
    assert_eq!(
        search_calls.lock().len(),
        1,
        "tool should fire exactly once"
    );
    assert_eq!(search_calls.lock()[0]["q"], "rust");
    assert!(provider.turn_count() >= 2);
}

// ── 2. Keyword-driven loop: native tool_calls path ────────────────

#[tokio::test]
async fn keyword_provider_drives_native_tool_calls_path() {
    let provider = KeywordScriptedProvider::new(vec![
        KeywordRule::tool_call(
            "weather",
            ScriptedToolCall::new("weather_tool", json!({"city": "Berlin"})),
        ),
        KeywordRule::final_reply("weather_tool-ok", "It's sunny."),
    ])
    .with_native_tools(true);

    let (weather_tool, weather_calls) = RecordingTool::echo("weather_tool");
    let tools: Vec<Box<dyn Tool>> = vec![Box::new(weather_tool)];

    let mut history = vec![ChatMessage::user("what's the weather in Berlin?")];

    let out = run_tool_call_loop(
        &provider,
        &mut history,
        &tools,
        "mock-native",
        "test-model",
        0.0,
        true,
        None,
        "channel",
        &mm(),
        5,
        None,
        None,
        &[],
        None,
        None,
    )
    .await
    .expect("loop should complete");

    assert_eq!(out, "It's sunny.");
    assert_eq!(weather_calls.lock().len(), 1);
    // History should contain a tool role message (native path) referencing the call id.
    assert!(history.iter().any(|m| m.role == "tool"));
    let tool_msg = history.iter().find(|m| m.role == "tool").unwrap();
    assert!(
        tool_msg.content.contains("weather_tool-ok"),
        "tool result should be threaded through history: {}",
        tool_msg.content
    );
}

// ── 3. Multi-tool chain via successive keyword matches ────────────

#[tokio::test]
async fn keyword_provider_chains_multiple_tools_across_iterations() {
    let provider = KeywordScriptedProvider::new(vec![
        KeywordRule::tool_call(
            "draft an email",
            ScriptedToolCall::new("draft_tool", json!({"to": "alice@example.com"})),
        ),
        KeywordRule::tool_call(
            "draft_tool-ok",
            ScriptedToolCall::new("send_tool", json!({"draft_id": "d-1"})),
        ),
        KeywordRule::final_reply("send_tool-ok", "Email sent to alice."),
    ]);

    let (draft_tool, draft_calls) = RecordingTool::echo("draft_tool");
    let (send_tool, send_calls) = RecordingTool::echo("send_tool");
    let tools: Vec<Box<dyn Tool>> = vec![Box::new(draft_tool), Box::new(send_tool)];

    let mut history = vec![ChatMessage::user("draft an email to alice")];

    let out = run_tool_call_loop(
        &provider,
        &mut history,
        &tools,
        "mock",
        "test-model",
        0.0,
        true,
        None,
        "channel",
        &mm(),
        10,
        None,
        None,
        &[],
        None,
        None,
    )
    .await
    .unwrap();

    assert_eq!(out, "Email sent to alice.");
    assert_eq!(draft_calls.lock().len(), 1);
    assert_eq!(send_calls.lock().len(), 1);
}

// ── 4. Crypto wallet flow: inspect → quote → confirm → execute ───

#[tokio::test]
async fn crypto_wallet_send_flow_sequences_wallet_tools_and_confirmation_gate() {
    let provider = KeywordScriptedProvider::new(vec![
        KeywordRule::tool_call(
            "send john $5",
            ScriptedToolCall::new("wallet_status", json!({})),
        ),
        KeywordRule::tool_call(
            "wallet_status-ok",
            ScriptedToolCall::new("wallet_balances", json!({})),
        ),
        KeywordRule::tool_call(
            "wallet_balances-ok",
            ScriptedToolCall::new("wallet_chain_status", json!({})),
        ),
        KeywordRule::tool_call(
            "wallet_chain_status-ok",
            ScriptedToolCall::new(
                "wallet_prepare_transfer",
                json!({
                    "chain": "evm",
                    "to_address": "0x00000000000000000000000000000000000000aa",
                    "amount_raw": "5000000",
                }),
            ),
        ),
        KeywordRule::tool_call(
            "wallet_prepare_transfer-ok",
            ScriptedToolCall::new(
                "ask_user_clarification",
                json!({"question": "Send $5 to John on EVM?"}),
            ),
        ),
        KeywordRule::tool_call(
            "ask_user_clarification-ok",
            ScriptedToolCall::new(
                "wallet_execute_prepared",
                json!({"quoteId": "q_test_send_john", "confirmed": true}),
            ),
        ),
        KeywordRule::final_reply(
            "wallet_execute_prepared-ok",
            "Prepared quote confirmed and handed to the wallet signer.",
        ),
    ])
    .with_native_tools(true);

    let sequence = Arc::new(parking_lot::Mutex::new(Vec::new()));
    let (wallet_status, wallet_status_calls) = SequencedTool::new(
        "wallet_status",
        ToolResult::success("wallet_status-ok"),
        sequence.clone(),
    );
    let (wallet_balances, wallet_balances_calls) = SequencedTool::new(
        "wallet_balances",
        ToolResult::success("wallet_balances-ok"),
        sequence.clone(),
    );
    let (wallet_chain_status, wallet_chain_status_calls) = SequencedTool::new(
        "wallet_chain_status",
        ToolResult::success("wallet_chain_status-ok"),
        sequence.clone(),
    );
    let (wallet_prepare_transfer, wallet_prepare_transfer_calls) = SequencedTool::new(
        "wallet_prepare_transfer",
        ToolResult::success("wallet_prepare_transfer-ok"),
        sequence.clone(),
    );
    let (ask_user_clarification, ask_user_clarification_calls) = SequencedTool::new(
        "ask_user_clarification",
        ToolResult::success("ask_user_clarification-ok"),
        sequence.clone(),
    );
    let (wallet_execute_prepared, wallet_execute_prepared_calls) = SequencedTool::new(
        "wallet_execute_prepared",
        ToolResult::success("wallet_execute_prepared-ok"),
        sequence.clone(),
    );

    let tools: Vec<Box<dyn Tool>> = vec![
        Box::new(wallet_status),
        Box::new(wallet_balances),
        Box::new(wallet_chain_status),
        Box::new(wallet_prepare_transfer),
        Box::new(ask_user_clarification),
        Box::new(wallet_execute_prepared),
    ];
    let mut history = vec![ChatMessage::user("Please send John $5 on EVM.")];

    let out = run_tool_call_loop(
        &provider,
        &mut history,
        &tools,
        "mock-native",
        "test-model",
        0.0,
        true,
        None,
        "web",
        &mm(),
        10,
        None,
        None,
        &[],
        None,
        None,
    )
    .await
    .expect("crypto wallet flow should complete");

    assert_eq!(
        out,
        "Prepared quote confirmed and handed to the wallet signer."
    );
    assert_eq!(
        sequence.lock().clone(),
        vec![
            "wallet_status",
            "wallet_balances",
            "wallet_chain_status",
            "wallet_prepare_transfer",
            "ask_user_clarification",
            "wallet_execute_prepared",
        ]
    );
    assert_eq!(wallet_status_calls.lock().len(), 1);
    assert_eq!(wallet_balances_calls.lock().len(), 1);
    assert_eq!(wallet_chain_status_calls.lock().len(), 1);
    assert_eq!(
        wallet_prepare_transfer_calls.lock()[0],
        json!({
            "chain": "evm",
            "to_address": "0x00000000000000000000000000000000000000aa",
            "amount_raw": "5000000",
        })
    );
    assert_eq!(
        ask_user_clarification_calls.lock()[0]["question"],
        "Send $5 to John on EVM?"
    );
    assert_eq!(
        wallet_execute_prepared_calls.lock()[0],
        json!({"quoteId": "q_test_send_john", "confirmed": true})
    );
}

#[tokio::test]
async fn crypto_wallet_send_flow_does_not_execute_when_confirmation_is_not_granted() {
    let provider = KeywordScriptedProvider::new(vec![
        KeywordRule::tool_call(
            "send john $5",
            ScriptedToolCall::new("wallet_status", json!({})),
        ),
        KeywordRule::tool_call(
            "wallet_status-ok",
            ScriptedToolCall::new("wallet_prepare_transfer", json!({"chain": "evm"})),
        ),
        KeywordRule::tool_call(
            "wallet_prepare_transfer-ok",
            ScriptedToolCall::new(
                "ask_user_clarification",
                json!({"question": "Confirm the transfer?"}),
            ),
        ),
        KeywordRule::final_reply(
            "user declined the transfer",
            "Cancelled before execution because the user did not confirm.",
        ),
    ])
    .with_native_tools(true);

    let sequence = Arc::new(parking_lot::Mutex::new(Vec::new()));
    let (wallet_status, _) = SequencedTool::new(
        "wallet_status",
        ToolResult::success("wallet_status-ok"),
        sequence.clone(),
    );
    let (wallet_prepare_transfer, _) = SequencedTool::new(
        "wallet_prepare_transfer",
        ToolResult::success("wallet_prepare_transfer-ok"),
        sequence.clone(),
    );
    let (ask_user_clarification, _) = SequencedTool::new(
        "ask_user_clarification",
        ToolResult::success("user declined the transfer"),
        sequence.clone(),
    );
    let (wallet_execute_prepared, wallet_execute_prepared_calls) = SequencedTool::new(
        "wallet_execute_prepared",
        ToolResult::success("wallet_execute_prepared-ok"),
        sequence.clone(),
    );

    let tools: Vec<Box<dyn Tool>> = vec![
        Box::new(wallet_status),
        Box::new(wallet_prepare_transfer),
        Box::new(ask_user_clarification),
        Box::new(wallet_execute_prepared),
    ];
    let mut history = vec![ChatMessage::user("Please send John $5 on EVM.")];

    let out = run_tool_call_loop(
        &provider,
        &mut history,
        &tools,
        "mock-native",
        "test-model",
        0.0,
        true,
        None,
        "telegram",
        &mm(),
        8,
        None,
        None,
        &[],
        None,
        None,
    )
    .await
    .expect("declined flow should still complete");

    assert_eq!(
        out,
        "Cancelled before execution because the user did not confirm."
    );
    assert_eq!(
        sequence.lock().clone(),
        vec![
            "wallet_status",
            "wallet_prepare_transfer",
            "ask_user_clarification",
        ]
    );
    assert!(
        wallet_execute_prepared_calls.lock().is_empty(),
        "execute tool must not run after a declined confirmation"
    );
}

#[tokio::test]
async fn keyword_provider_uses_latest_tool_result_to_drive_the_next_tool_call() {
    let provider = KeywordScriptedProvider::new(vec![
        KeywordRule::tool_call(
            "start lookup",
            ScriptedToolCall::new("lookup_tool", json!({"symbol": "BTC"})),
        ),
        KeywordRule::tool_call(
            "lookup_tool-ok",
            ScriptedToolCall::new("enrich_tool", json!({"source": "lookup"})),
        ),
        KeywordRule::final_reply("enrich_tool-ok", "Finished after the second tool."),
    ])
    .with_native_tools(true);

    let (lookup_tool, lookup_calls) = RecordingTool::echo("lookup_tool");
    let (enrich_tool, enrich_calls) = RecordingTool::echo("enrich_tool");
    let tools: Vec<Box<dyn Tool>> = vec![Box::new(lookup_tool), Box::new(enrich_tool)];

    let mut history = vec![ChatMessage::user("please start lookup for BTC")];

    let out = run_tool_call_loop(
        &provider,
        &mut history,
        &tools,
        "mock-native",
        "test-model",
        0.0,
        true,
        None,
        "channel",
        &mm(),
        10,
        None,
        None,
        &[],
        None,
        None,
    )
    .await
    .expect("loop should complete");

    assert_eq!(out, "Finished after the second tool.");
    assert_eq!(lookup_calls.lock().as_slice(), &[json!({"symbol": "BTC"})]);
    assert_eq!(
        enrich_calls.lock().as_slice(),
        &[json!({"source": "lookup"})]
    );

    let turns = provider.turns();
    assert_eq!(
        turns.len(),
        3,
        "expected two tool turns and one final reply"
    );
    assert_eq!(turns[0].rule_keyword.as_deref(), Some("start lookup"));
    assert_eq!(turns[1].rule_keyword.as_deref(), Some("lookup_tool-ok"));
    assert_eq!(turns[2].rule_keyword.as_deref(), Some("enrich_tool-ok"));

    let second_turn_probe = turns[1]
        .messages
        .iter()
        .rev()
        .find(|msg| msg.role == "tool")
        .map(|msg| msg.content.clone())
        .unwrap_or_default();
    assert!(
        second_turn_probe.contains("lookup_tool-ok"),
        "second turn should be driven by the first tool result, got: {second_turn_probe}"
    );
}

#[tokio::test]
async fn keyword_provider_executes_multiple_native_tool_calls_from_one_turn() {
    let provider = KeywordScriptedProvider::new(vec![
        KeywordRule {
            keyword: "do both".to_string(),
            tool_calls: vec![
                ScriptedToolCall::new("lookup_tool", json!({"symbol": "BTC"})),
                ScriptedToolCall::new("enrich_tool", json!({"source": "coinbase"})),
            ],
            final_text: Some("Running both tools.".to_string()),
            max_fires: Some(1),
        },
        KeywordRule::final_reply("enrich_tool-ok", "Both tools completed."),
    ])
    .with_native_tools(true);

    let (lookup_tool, lookup_calls) = RecordingTool::echo("lookup_tool");
    let (enrich_tool, enrich_calls) = RecordingTool::echo("enrich_tool");
    let tools: Vec<Box<dyn Tool>> = vec![Box::new(lookup_tool), Box::new(enrich_tool)];

    let mut history = vec![ChatMessage::user("please do both actions now")];

    let out = run_tool_call_loop(
        &provider,
        &mut history,
        &tools,
        "mock-native",
        "test-model",
        0.0,
        true,
        None,
        "channel",
        &mm(),
        10,
        None,
        None,
        &[],
        None,
        None,
    )
    .await
    .expect("loop should complete");

    assert_eq!(out, "Both tools completed.");
    assert_eq!(lookup_calls.lock().as_slice(), &[json!({"symbol": "BTC"})]);
    assert_eq!(
        enrich_calls.lock().as_slice(),
        &[json!({"source": "coinbase"})]
    );

    let turns = provider.turns();
    assert_eq!(turns[0].emitted_tool_calls.len(), 2);
    assert_eq!(turns[0].emitted_tool_calls[0].name, "lookup_tool");
    assert_eq!(turns[0].emitted_tool_calls[1].name, "enrich_tool");
}

// ── 6. Unknown tool name handled gracefully ───────────────────────

#[tokio::test]
async fn keyword_provider_unknown_tool_surfaces_error_and_loop_continues() {
    let provider = KeywordScriptedProvider::new(vec![
        KeywordRule::tool_call("go", ScriptedToolCall::new("nonexistent_tool", json!({}))),
        // After we see "Unknown tool" in the role=tool injection, give up.
        KeywordRule::final_reply("unknown tool", "Sorry, I can't do that."),
    ]);

    let tools: Vec<Box<dyn Tool>> = vec![];

    let mut history = vec![ChatMessage::user("go go go")];

    let out = run_tool_call_loop(
        &provider,
        &mut history,
        &tools,
        "mock",
        "test-model",
        0.0,
        true,
        None,
        "channel",
        &mm(),
        5,
        None,
        None,
        &[],
        None,
        None,
    )
    .await
    .unwrap();

    assert_eq!(out, "Sorry, I can't do that.");
    // The Tool Results message should record the "Unknown tool: nonexistent_tool".
    assert!(history.iter().any(|m| m.content.contains("Unknown tool")));
}

// ── 5. Max iterations guard ───────────────────────────────────────

#[tokio::test]
async fn run_tool_call_loop_returns_max_iterations_error() {
    // Configure a rule that keeps firing forever — but cap iterations.
    let provider = KeywordScriptedProvider::new(vec![KeywordRule {
        keyword: "echo-ok".to_string(), // matches tool result, so it loops
        tool_calls: vec![ScriptedToolCall::new("echo", json!({}))],
        final_text: None,
        max_fires: None,
    }])
    // First turn: kick it off
    .with_fallback("end");
    provider.push_forced_response(ChatResponse {
        text: Some("<tool_call>{\"name\":\"echo\",\"arguments\":{}}</tool_call>".into()),
        tool_calls: vec![],
        usage: None,
    });

    let (echo_tool, _) = RecordingTool::echo("echo");
    let tools: Vec<Box<dyn Tool>> = vec![Box::new(echo_tool)];
    let mut history = vec![ChatMessage::user("loop forever")];

    let err = run_tool_call_loop(
        &provider,
        &mut history,
        &tools,
        "mock",
        "test-model",
        0.0,
        true,
        None,
        "channel",
        &mm(),
        3,
        None,
        None,
        &[],
        None,
        None,
    )
    .await
    .expect_err("should hit max iterations");

    let s = err.to_string();
    assert!(
        s.contains("3") && s.to_lowercase().contains("iteration"),
        "expected MaxIterationsExceeded with 3, got: {s}"
    );
}

// ── 6. CliRpcOnly tools are blocked in the agent loop ─────────────

struct CliOnlyTool {
    calls: Arc<AtomicUsize>,
}

#[async_trait]
impl Tool for CliOnlyTool {
    fn name(&self) -> &str {
        "cli_only_tool"
    }
    fn description(&self) -> &str {
        "cli-only"
    }
    fn parameters_schema(&self) -> serde_json::Value {
        json!({"type": "object"})
    }
    fn scope(&self) -> ToolScope {
        ToolScope::CliRpcOnly
    }
    async fn execute(&self, _args: serde_json::Value) -> anyhow::Result<ToolResult> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(ToolResult::success("ran"))
    }
}

#[tokio::test]
async fn agent_loop_refuses_clirpconly_tools() {
    let calls = Arc::new(AtomicUsize::new(0));
    let tool = CliOnlyTool {
        calls: calls.clone(),
    };

    let provider = KeywordScriptedProvider::new(vec![
        KeywordRule::tool_call("use", ScriptedToolCall::new("cli_only_tool", json!({}))),
        KeywordRule::final_reply("only available via", "Denied as expected."),
    ]);

    let tools: Vec<Box<dyn Tool>> = vec![Box::new(tool)];
    let mut history = vec![ChatMessage::user("use the tool")];

    let out = run_tool_call_loop(
        &provider,
        &mut history,
        &tools,
        "mock",
        "test-model",
        0.0,
        true,
        None,
        "channel",
        &mm(),
        5,
        None,
        None,
        &[],
        None,
        None,
    )
    .await
    .unwrap();

    assert_eq!(out, "Denied as expected.");
    assert_eq!(
        calls.load(Ordering::SeqCst),
        0,
        "CliRpcOnly tool must never execute in the agent loop"
    );
}

// ── 7. Tool error result is threaded back as `Error: …` ───────────

struct FailingTool;

#[async_trait]
impl Tool for FailingTool {
    fn name(&self) -> &str {
        "fail_tool"
    }
    fn description(&self) -> &str {
        "always fails"
    }
    fn parameters_schema(&self) -> serde_json::Value {
        json!({"type": "object"})
    }
    async fn execute(&self, _args: serde_json::Value) -> anyhow::Result<ToolResult> {
        Ok(ToolResult::error("boom"))
    }
}

#[tokio::test]
async fn tool_error_result_is_surfaced_to_next_iteration() {
    let provider = KeywordScriptedProvider::new(vec![
        KeywordRule::tool_call("try", ScriptedToolCall::new("fail_tool", json!({}))),
        KeywordRule::final_reply("boom", "got the error"),
    ]);

    let tools: Vec<Box<dyn Tool>> = vec![Box::new(FailingTool)];
    let mut history = vec![ChatMessage::user("try the broken tool")];

    let out = run_tool_call_loop(
        &provider,
        &mut history,
        &tools,
        "mock",
        "test-model",
        0.0,
        true,
        None,
        "channel",
        &mm(),
        5,
        None,
        None,
        &[],
        None,
        None,
    )
    .await
    .unwrap();

    assert_eq!(out, "got the error");
    assert!(history.iter().any(|m| m.content.contains("Error: boom")));
}

// ── 8. Tool that bails with anyhow::Error ─────────────────────────

struct PanickyTool;

#[async_trait]
impl Tool for PanickyTool {
    fn name(&self) -> &str {
        "panicky"
    }
    fn description(&self) -> &str {
        "raises anyhow"
    }
    fn parameters_schema(&self) -> serde_json::Value {
        json!({"type": "object"})
    }
    async fn execute(&self, _args: serde_json::Value) -> anyhow::Result<ToolResult> {
        anyhow::bail!("kaboom")
    }
}

#[tokio::test]
async fn tool_anyhow_error_surfaces_in_history() {
    let provider = KeywordScriptedProvider::new(vec![
        KeywordRule::tool_call("run", ScriptedToolCall::new("panicky", json!({}))),
        KeywordRule::final_reply("kaboom", "tool blew up"),
    ]);

    let tools: Vec<Box<dyn Tool>> = vec![Box::new(PanickyTool)];
    let mut history = vec![ChatMessage::user("run it")];

    let out = run_tool_call_loop(
        &provider,
        &mut history,
        &tools,
        "mock",
        "test-model",
        0.0,
        true,
        None,
        "channel",
        &mm(),
        5,
        None,
        None,
        &[],
        None,
        None,
    )
    .await
    .unwrap();

    assert_eq!(out, "tool blew up");
    assert!(history.iter().any(|m| m.content.contains("kaboom")));
}

// ── 9. visible_tool_names whitelist hides tools from the model ────

#[tokio::test]
async fn visible_tool_names_whitelist_rejects_filtered_out_tools() {
    let provider = KeywordScriptedProvider::new(vec![
        // Model asks for a tool that *exists* but is filtered out.
        KeywordRule::tool_call("go", ScriptedToolCall::new("hidden", json!({}))),
        KeywordRule::final_reply("unknown tool", "Cannot reach hidden tool."),
    ]);

    let (visible_tool, visible_calls) = RecordingTool::echo("visible");
    let (hidden_tool, hidden_calls) = RecordingTool::echo("hidden");
    let tools: Vec<Box<dyn Tool>> = vec![Box::new(visible_tool), Box::new(hidden_tool)];

    let mut visible = std::collections::HashSet::new();
    visible.insert("visible".to_string());

    let mut history = vec![ChatMessage::user("go please")];

    let out = run_tool_call_loop(
        &provider,
        &mut history,
        &tools,
        "mock",
        "test-model",
        0.0,
        true,
        None,
        "channel",
        &mm(),
        5,
        None,
        Some(&visible),
        &[],
        None,
        None,
    )
    .await
    .unwrap();

    assert_eq!(out, "Cannot reach hidden tool.");
    assert_eq!(visible_calls.lock().len(), 0);
    assert_eq!(
        hidden_calls.lock().len(),
        0,
        "hidden tool must not execute when filtered out"
    );
}

// ── 10. extra_tools are reachable alongside the registry ──────────

#[tokio::test]
async fn extra_tools_are_invokable_alongside_registry() {
    let provider = KeywordScriptedProvider::new(vec![
        KeywordRule::tool_call("delegate", ScriptedToolCall::new("extra", json!({"x": 1}))),
        KeywordRule::final_reply("extra-ok", "delegated"),
    ]);

    let (extra_tool, extra_calls) = RecordingTool::echo("extra");
    let extras: Vec<Box<dyn Tool>> = vec![Box::new(extra_tool)];

    let tools: Vec<Box<dyn Tool>> = vec![];
    let mut history = vec![ChatMessage::user("delegate the work")];

    let out = run_tool_call_loop(
        &provider,
        &mut history,
        &tools,
        "mock",
        "test-model",
        0.0,
        true,
        None,
        "channel",
        &mm(),
        5,
        None,
        None,
        &extras,
        None,
        None,
    )
    .await
    .unwrap();

    assert_eq!(out, "delegated");
    assert_eq!(extra_calls.lock().len(), 1);
}
