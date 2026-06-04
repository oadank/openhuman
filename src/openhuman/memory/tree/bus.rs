//! Event-bus subscriber that ingests channel messages into the memory tree.
//!
//! Bridges ChannelMessageReceived events to the memory_tree ingest pipeline.
//! Messages are buffered per-conversation and flushed periodically to enable
//! proper scoring, deduplication, and chunking.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use parking_lot::RwLock;

use crate::core::event_bus::{DomainEvent, EventHandler, SubscriptionHandle};
use crate::openhuman::config::Config;
use crate::openhuman::memory::tree::canonicalize::chat::{ChatBatch, ChatMessage};
use crate::openhuman::memory::tree::ingest::ingest_chat;

static MEMORY_TREE_INGEST_HANDLE: OnceLock<SubscriptionHandle> = OnceLock::new();

const LOG_PREFIX: &str = "[memory_tree::bus]";

/// Minimum messages to accumulate before flushing.
const MIN_FLUSH_COUNT: usize = 3;

/// Maximum time to wait before flushing a buffer (seconds).
const MAX_FLUSH_INTERVAL_SECS: u64 = 60;

/// Maximum messages per buffer before forced flush.
const MAX_BUFFER_SIZE: usize = 50;

/// Supported channels for memory tree ingestion.
const SUPPORTED_CHANNELS: &[&str] = &["lark", "telegram"];

/// Conversation buffer key: (channel, sender, reply_target)
type BufferKey = (String, String, String);

/// Buffered message with timestamp.
#[derive(Clone)]
struct BufferedMessage {
    author: String,
    timestamp: DateTime<Utc>,
    text: String,
    source_ref: Option<String>,
    role: String,
}

/// Per-conversation message buffer.
#[derive(Clone)]
struct ConversationBuffer {
    messages: Vec<BufferedMessage>,
    last_flush: Instant,
    channel: String,
    channel_label: String,
}

/// Global state for the ingest subscriber.
struct IngestState {
    config: Config,
    buffers: HashMap<BufferKey, ConversationBuffer>,
}

static INGEST_STATE: OnceLock<Arc<RwLock<IngestState>>> = OnceLock::new();

/// Register the memory tree channel ingest subscriber.
///
/// This bridges channel events to the memory_tree ingest pipeline so
/// conversations from lark (Feishu) and Telegram become searchable memories.
pub fn register_memory_tree_ingest_subscriber(config: Config) {
    if MEMORY_TREE_INGEST_HANDLE.get().is_some() {
        return;
    }

    // Initialize global state
    let state = Arc::new(RwLock::new(IngestState {
        config,
        buffers: HashMap::new(),
    }));
    let _ = INGEST_STATE.set(state.clone());

    match crate::core::event_bus::subscribe_global(Arc::new(MemoryTreeIngestSubscriber::new(state))) {
        Some(handle) => {
            let _ = MEMORY_TREE_INGEST_HANDLE.set(handle);
            log::info!("{LOG_PREFIX} subscriber registered");
        }
        None => {
            log::warn!(
                "{LOG_PREFIX} failed to register subscriber — bus not initialized"
            );
        }
    }
}

pub struct MemoryTreeIngestSubscriber {
    state: Arc<RwLock<IngestState>>,
}

impl MemoryTreeIngestSubscriber {
    pub fn new(state: Arc<RwLock<IngestState>>) -> Self {
        Self { state }
    }
}

#[async_trait]
impl EventHandler for MemoryTreeIngestSubscriber {
    fn name(&self) -> &str {
        "memory_tree::channel_ingest"
    }

    fn domains(&self) -> Option<&[&str]> {
        Some(&["channel"])
    }

    async fn handle(&self, event: &DomainEvent) {
        match event {
            DomainEvent::ChannelMessageReceived {
                channel,
                message_id,
                sender,
                reply_target,
                content,
                thread_ts,
            } => {
                // Only process supported channels
                if !SUPPORTED_CHANNELS.contains(&channel.as_str()) {
                    return;
                }

                self.buffer_message(
                    channel,
                    sender,
                    reply_target,
                    thread_ts.as_deref(),
                    content,
                    sender, // author = sender for user messages
                    "user",
                    Some(format!("{}://{}", channel, message_id)),
                );
            }
            DomainEvent::ChannelMessageProcessed {
                channel,
                message_id,
                sender,
                reply_target,
                thread_ts,
                response,
                ..
            } => {
                // Only process supported channels
                if !SUPPORTED_CHANNELS.contains(&channel.as_str()) {
                    return;
                }

                self.buffer_message(
                    channel,
                    "assistant", // assistant is the author for responses
                    reply_target,
                    thread_ts.as_deref(),
                    response,
                    "openhuman", // author name
                    "assistant",
                    Some(format!("{}://{}", channel, message_id)),
                );
            }
            _ => {}
        }
    }
}

impl MemoryTreeIngestSubscriber {
    /// Add a message to the conversation buffer and check flush conditions.
    fn buffer_message(
        &self,
        channel: &str,
        sender: &str,
        reply_target: &str,
        thread_ts: Option<&str>,
        content: &str,
        author: &str,
        role: &str,
        source_ref: Option<String>,
    ) {
        // Skip empty content
        let content = content.trim();
        if content.is_empty() {
            return;
        }

        // Build buffer key (telegram uses reply_target only, not thread_ts)
        let key = if channel == "telegram" {
            (channel.to_string(), sender.to_string(), reply_target.to_string())
        } else {
            let thread_key = thread_ts.map(|t| format!("thread:{}", t)).unwrap_or_default();
            (channel.to_string(), sender.to_string(), format!("{}:{}", reply_target, thread_key))
        };

        // Build channel label for display
        let channel_label = build_channel_label(channel, reply_target, thread_ts);

        let now = Utc::now();
        let instant_now = Instant::now();

        let mut state = self.state.write();

        // Clone config early to avoid borrow conflict
        let config = state.config.clone();

        // Get or create buffer
        let buffer = state.buffers.entry(key.clone()).or_insert(ConversationBuffer {
            messages: Vec::new(),
            last_flush: instant_now,
            channel: channel.to_string(),
            channel_label: channel_label.clone(),
        });

        // Add message
        buffer.messages.push(BufferedMessage {
            author: author.to_string(),
            timestamp: now,
            text: content.to_string(),
            source_ref,
            role: role.to_string(),
        });

        let msg_len = buffer.messages.len();

        log::debug!(
            "{LOG_PREFIX} buffered message channel={} sender={} author={} role={} len={}",
            channel,
            sender,
            author,
            role,
            msg_len
        );

        // Check flush conditions
        let should_flush = msg_len >= MAX_BUFFER_SIZE
            || msg_len >= MIN_FLUSH_COUNT
                && instant_now.duration_since(buffer.last_flush).as_secs() >= MAX_FLUSH_INTERVAL_SECS;

        if should_flush {
            let messages_to_flush = buffer.messages.clone();
            let channel_name = buffer.channel.clone();
            let channel_label_flush = buffer.channel_label.clone();
            let owner = sender.to_string();

            // Clear buffer and update flush time
            buffer.messages.clear();
            buffer.last_flush = instant_now;

            // Drop lock before async ingest
            drop(state);

            // Spawn async ingest
            self.spawn_ingest(config, key, channel_name, channel_label_flush, owner, messages_to_flush);
        }
    }

    /// Spawn async ingest task.
    fn spawn_ingest(
        &self,
        config: Config,
        key: BufferKey,
        channel: String,
        channel_label: String,
        owner: String,
        messages: Vec<BufferedMessage>,
    ) {
        // Convert to ChatBatch format
        let chat_messages: Vec<ChatMessage> = messages
            .into_iter()
            .map(|m| ChatMessage {
                author: m.author,
                timestamp: m.timestamp,
                text: m.text,
                source_ref: m.source_ref,
            })
            .collect();

        if chat_messages.is_empty() {
            return;
        }

        let batch = ChatBatch {
            platform: channel,
            channel_label,
            messages: chat_messages,
        };

        // Build source_id from buffer key
        let source_id = format!("{}:{}:{}", key.0, key.1, key.2);

        // Spawn ingest in background
        tokio::spawn(async move {
            let result = ingest_chat(&config, &source_id, &owner, vec![], batch).await;

            match result {
                Ok(ingest_result) => {
                    if ingest_result.already_ingested {
                        log::debug!(
                            "{LOG_PREFIX} already ingested source_id={}",
                            source_id
                        );
                    } else {
                        log::info!(
                            "{LOG_PREFIX} ingest success source_id={} chunks={} dropped={}",
                            source_id,
                            ingest_result.chunks_written,
                            ingest_result.chunks_dropped
                        );
                    }
                }
                Err(e) => {
                    log::warn!(
                        "{LOG_PREFIX} ingest failed source_id={} error={}",
                        source_id,
                        e
                    );
                }
            }
        });
    }
}

/// Build human-readable channel label.
fn build_channel_label(channel: &str, reply_target: &str, thread_ts: Option<&str>) -> String {
    match thread_ts.and_then(non_empty_trimmed) {
        Some(thread_ts) if channel != "telegram" => {
            format!("{channel} · {reply_target} · thread {thread_ts}")
        }
        _ => format!("{channel} · {reply_target}"),
    }
}

fn non_empty_trimmed(value: &str) -> Option<&str> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn mock_config() -> Config {
        Config {
            workspace_dir: TempDir::new().expect("tempdir").path().to_path_buf(),
            ..Default::default()
        }
    }

    #[test]
    fn buffer_key_telegram_ignores_thread_ts() {
        let key = super::buffer_message_key("telegram", "alice", "chat-1", Some("100"));
        assert_eq!(key, ("telegram", "alice", "chat-1"));
    }

    #[test]
    fn buffer_key_lark_includes_thread_ts() {
        let key = super::buffer_message_key("lark", "alice", "general", Some("thread-1"));
        assert_eq!(key, ("lark", "alice", "general:thread:thread-1"));
    }

    fn buffer_message_key(channel: &str, sender: &str, reply_target: &str, thread_ts: Option<&str>) -> BufferKey {
        if channel == "telegram" {
            (channel.to_string(), sender.to_string(), reply_target.to_string())
        } else {
            let thread_key = thread_ts.map(|t| format!("thread:{}", t)).unwrap_or_default();
            (channel.to_string(), sender.to_string(), format!("{}:{}", reply_target, thread_key))
        }
    }
}