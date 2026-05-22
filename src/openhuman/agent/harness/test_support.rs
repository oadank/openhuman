//! Smart-mock test support for the agent harness.
//!
//! This module provides a reusable "fake LLM" that drives the real
//! [`run_tool_call_loop`] without needing any network access.
//!
//! [`KeywordScriptedProvider`] is a [`Provider`] implementation that
//!    inspects the latest `user` message of the conversation and emits
//!    canned tool calls (or a final reply) when a configured keyword
//!    matches. The first turn that has *no* matching rule returns a
//!    plain "done" reply, which terminates the loop deterministically.
//!
//! Compared to the hand-rolled `ScriptedProvider` in
//! [`super::tool_loop_tests`], this provider reacts to rolling
//! conversation state, supports both native and prompt-guided tool-call
//! formats, and records turns for post-hoc assertions.

use std::collections::VecDeque;
use std::sync::Arc;

use async_trait::async_trait;
use parking_lot::Mutex;
use serde_json::json;

use crate::openhuman::inference::provider::traits::ProviderCapabilities;
use crate::openhuman::inference::provider::{
    ChatMessage, ChatRequest, ChatResponse, Provider, ToolCall,
};

/// One scripted reaction the [`KeywordScriptedProvider`] can emit when
/// it sees its keyword in the latest user/tool turn.
#[derive(Debug, Clone)]
pub struct KeywordRule {
    /// Substring matched (case-insensitive) against the latest user
    /// or tool message in the conversation.
    pub keyword: String,
    /// Tool calls to emit. Empty ⇒ no tool calls, only `final_text`.
    pub tool_calls: Vec<ScriptedToolCall>,
    /// Optional plain-text body to include alongside any tool calls.
    /// When `tool_calls` is empty, this becomes the loop-terminating
    /// final response.
    pub final_text: Option<String>,
    /// How many times this rule may fire. `None` ⇒ unlimited.
    pub max_fires: Option<usize>,
}

#[derive(Debug, Clone)]
pub struct ScriptedToolCall {
    pub name: String,
    pub arguments: serde_json::Value,
}

impl ScriptedToolCall {
    pub fn new(name: impl Into<String>, arguments: serde_json::Value) -> Self {
        Self {
            name: name.into(),
            arguments,
        }
    }
}

impl KeywordRule {
    pub fn final_reply(keyword: impl Into<String>, text: impl Into<String>) -> Self {
        Self {
            keyword: keyword.into(),
            tool_calls: Vec::new(),
            final_text: Some(text.into()),
            max_fires: None,
        }
    }

    pub fn tool_call(keyword: impl Into<String>, call: ScriptedToolCall) -> Self {
        Self {
            keyword: keyword.into(),
            tool_calls: vec![call],
            final_text: None,
            max_fires: Some(1),
        }
    }

    pub fn with_text(mut self, text: impl Into<String>) -> Self {
        self.final_text = Some(text.into());
        self
    }

    pub fn unlimited(mut self) -> Self {
        self.max_fires = None;
        self
    }
}

/// Snapshot of one turn the provider served — handy for tests that
/// want to assert what the LLM "saw" without coupling to the harness
/// internals.
#[derive(Debug, Clone)]
pub struct ProviderTurn {
    pub messages: Vec<ChatMessage>,
    pub rule_keyword: Option<String>,
    pub emitted_tool_calls: Vec<ToolCall>,
    pub emitted_text: Option<String>,
}

struct ProviderState {
    rules: Vec<KeywordRule>,
    fired: Vec<usize>,
    turns: Vec<ProviderTurn>,
    fallback_text: String,
    next_call_id: usize,
    /// Optional queue of scripted responses to consume *before* the
    /// keyword rules run — useful when a test wants the first turn to
    /// behave deterministically regardless of the user message.
    forced: VecDeque<ChatResponse>,
}

/// Smart provider that reacts to conversation state via keyword rules.
pub struct KeywordScriptedProvider {
    state: Arc<Mutex<ProviderState>>,
    native_tools: bool,
    vision: bool,
}

impl KeywordScriptedProvider {
    pub fn new(rules: Vec<KeywordRule>) -> Self {
        Self {
            state: Arc::new(Mutex::new(ProviderState {
                rules,
                fired: Vec::new(),
                turns: Vec::new(),
                fallback_text: "done".to_string(),
                next_call_id: 0,
                forced: VecDeque::new(),
            })),
            native_tools: false,
            vision: false,
        }
    }

    pub fn with_native_tools(mut self, enabled: bool) -> Self {
        self.native_tools = enabled;
        self
    }

    pub fn with_vision(mut self, enabled: bool) -> Self {
        self.vision = enabled;
        self
    }

    pub fn with_fallback(self, text: impl Into<String>) -> Self {
        {
            let mut guard = self.state.lock();
            guard.fallback_text = text.into();
        }
        self
    }

    pub fn push_forced_response(&self, resp: ChatResponse) {
        self.state.lock().forced.push_back(resp);
    }

    pub fn turns(&self) -> Vec<ProviderTurn> {
        self.state.lock().turns.clone()
    }

    pub fn turn_count(&self) -> usize {
        self.state.lock().turns.len()
    }
}

fn latest_user_or_tool_msg(messages: &[ChatMessage]) -> Option<&ChatMessage> {
    messages
        .iter()
        .rev()
        .find(|m| m.role == "user" || m.role == "tool")
}

#[async_trait]
impl Provider for KeywordScriptedProvider {
    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            native_tool_calling: self.native_tools,
            vision: self.vision,
        }
    }

    async fn chat_with_system(
        &self,
        _system_prompt: Option<&str>,
        _message: &str,
        _model: &str,
        _temperature: f64,
    ) -> anyhow::Result<String> {
        Ok("fallback".into())
    }

    async fn chat(
        &self,
        request: ChatRequest<'_>,
        _model: &str,
        _temperature: f64,
    ) -> anyhow::Result<ChatResponse> {
        let messages = request.messages.to_vec();
        let mut state = self.state.lock();

        // Forced queue wins, regardless of keyword matching.
        if let Some(resp) = state.forced.pop_front() {
            state.turns.push(ProviderTurn {
                messages,
                rule_keyword: None,
                emitted_tool_calls: resp.tool_calls.clone(),
                emitted_text: resp.text.clone(),
            });
            return Ok(resp);
        }

        let probe = latest_user_or_tool_msg(&messages)
            .map(|m| m.content.to_lowercase())
            .unwrap_or_default();

        let mut chosen: Option<usize> = None;
        for (idx, rule) in state.rules.iter().enumerate() {
            let fired = *state.fired.get(idx).unwrap_or(&0);
            if let Some(cap) = rule.max_fires {
                if fired >= cap {
                    continue;
                }
            }
            if probe.contains(&rule.keyword.to_lowercase()) {
                chosen = Some(idx);
                break;
            }
        }

        let (rule_keyword, tool_calls, text) = if let Some(idx) = chosen {
            while state.fired.len() <= idx {
                state.fired.push(0);
            }
            state.fired[idx] += 1;
            let rule = state.rules[idx].clone();
            let tool_calls: Vec<ToolCall> = if self.native_tools {
                rule.tool_calls
                    .iter()
                    .map(|c| {
                        let id = state.next_call_id;
                        state.next_call_id += 1;
                        ToolCall {
                            id: format!("call_{id}"),
                            name: c.name.clone(),
                            arguments: c.arguments.to_string(),
                        }
                    })
                    .collect()
            } else {
                Vec::new()
            };

            let text = if self.native_tools {
                rule.final_text.clone()
            } else if !rule.tool_calls.is_empty() {
                // Prompt-guided: emit XML-wrapped tool calls in text.
                let mut body = String::new();
                if let Some(prefix) = &rule.final_text {
                    body.push_str(prefix);
                    if !prefix.ends_with('\n') {
                        body.push('\n');
                    }
                }
                for c in &rule.tool_calls {
                    body.push_str("<tool_call>");
                    body.push_str(&json!({"name": c.name, "arguments": c.arguments}).to_string());
                    body.push_str("</tool_call>\n");
                }
                Some(body)
            } else {
                rule.final_text.clone()
            };

            (Some(rule.keyword.clone()), tool_calls, text)
        } else {
            // No rule matched — emit the fallback as the final reply
            // so the loop terminates rather than hanging.
            (None, Vec::new(), Some(state.fallback_text.clone()))
        };

        let resp = ChatResponse {
            text: text.clone(),
            tool_calls: tool_calls.clone(),
            usage: None,
        };

        state.turns.push(ProviderTurn {
            messages,
            rule_keyword,
            emitted_tool_calls: tool_calls,
            emitted_text: text,
        });

        Ok(resp)
    }
}
