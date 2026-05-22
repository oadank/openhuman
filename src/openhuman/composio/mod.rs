//! Composio domain module — direct access to the user's Composio v3 tenant.
//!
//! The hosted OpenHuman backend proxy has been removed from this fork.
//! Composio calls use the user's own API key, native trigger delivery
//! runs through the embedded webhook receiver, and action execution is
//! routed through the direct Composio client helpers in this module.
//!
//! ## Surface
//!
//! - **RPC controllers** (`schemas.rs` / `ops.rs`) — `openhuman.composio_*`
//!   methods for listing toolkits, managing connections, listing tools,
//!   and executing actions. These are registered in
//!   [`crate::core::all`] alongside other domains.
//!
//! - **Agent tools** (`tools.rs`) — model-facing `composio_*` tools the
//!   autonomous agent loop can call. Registered from
//!   [`crate::openhuman::tools::ops::all_tools_with_runtime`].
//!
//! - **Event bus** (`bus.rs`) — `ComposioTriggerSubscriber` listens for
//!   [`DomainEvent::ComposioTriggerReceived`] events published by the
//!   local webhook receiver.
//!
//! [`DomainEvent::ComposioTriggerReceived`]:
//! crate::core::event_bus::DomainEvent::ComposioTriggerReceived

pub mod action_tool;
pub mod bus;
pub mod client;
pub mod error_mapping;
pub mod execute_dispatch;
pub mod execute_prepare;
pub mod googlecalendar_args;
pub mod ops;
pub mod periodic;
pub mod providers;
pub mod schemas;
pub mod tools;
pub mod trigger_history;
pub mod types;
pub mod webhook_receiver;

pub use action_tool::ComposioActionTool;
pub use bus::{
    register_composio_trigger_subscriber, ComposioConfigChangedSubscriber,
    ComposioTriggerSubscriber,
};
pub use ops::{
    cached_active_integrations, connected_set_hash, fetch_connected_integrations,
    fetch_connected_integrations_status, fetch_direct_toolkit_actions,
    invalidate_connected_integrations_cache, FetchConnectedIntegrationsStatus,
};
pub use periodic::{record_sync_success, start_periodic_sync};
pub use providers::{
    all_providers as all_composio_providers, get_provider as get_composio_provider,
    init_default_providers as init_default_composio_providers, ComposioProvider, ProviderContext,
    ProviderUserProfile, SyncOutcome, SyncReason,
};
pub use schemas::{
    all_controller_schemas as all_composio_controller_schemas,
    all_registered_controllers as all_composio_registered_controllers,
};
pub use tools::all_composio_agent_tools;
pub use trigger_history::{
    global as global_composio_trigger_history, init_global as init_composio_trigger_history,
};
pub use types::{
    ComposioAuthorizeResponse, ComposioCapabilitiesResponse, ComposioCapability,
    ComposioConnection, ComposioConnectionsResponse, ComposioDeleteResponse,
    ComposioExecuteResponse, ComposioToolFunction, ComposioToolSchema, ComposioToolkitsResponse,
    ComposioToolsResponse, ComposioTriggerEvent, ComposioTriggerHistoryEntry,
    ComposioTriggerHistoryResult, ComposioTriggerMetadata,
};
