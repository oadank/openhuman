# Native Android Integration Plan

## Status

Planning document. No Android product target exists in this repository today.

OpenHuman currently ships as a desktop product. The existing Tauri host is
desktop-only and intentionally rejects non-desktop targets. Android integration
should therefore be treated as a new native shell around the reusable Rust core,
not as a direct extension of `app/src-tauri`.

## Goals

- Ship a native Android application for the core OpenHuman user workflows.
- Reuse the Rust core as the source of business logic wherever it is portable.
- Keep Android UI and lifecycle behavior native to Android.
- Preserve the existing controller/RPC contract so desktop and Android share
  feature semantics.
- Avoid moving desktop-only Tauri, CEF, accessibility, scanner, or windowing
  concerns into the mobile surface.

## Non-Goals

- Do not compile the current `app/src-tauri` crate for Android.
- Do not ship a WebView wrapper as the native Android app.
- Do not port desktop-only features such as tray, global hotkeys, CEF webview
  accounts, iMessage scanning, desktop screen capture, or desktop service
  management in the first milestone.
- Do not duplicate core business rules in Kotlin.

## Current Architecture Constraints

- `app/src-tauri/src/lib.rs` has a compile-time desktop guard:
  `windows`, `macos`, and `linux` are the only supported shell targets.
- The Tauri shell depends on CEF and desktop plugins/modules that do not map
  cleanly to Android.
- The React frontend already talks to the core through JSON-RPC over HTTP and
  Socket.IO, which gives Android a reusable contract surface.
- The root Rust crate exposes domain functionality through controller registries
  in `src/core/all.rs` and HTTP routing in `src/core/jsonrpc.rs`.
- The desktop shell already embeds the core as an in-process Tokio task, which
  is the lifecycle model Android should adapt in a smaller mobile host.

## Recommended Architecture

Create a separate Android app and a thin Rust mobile facade:

```text
android/                 Kotlin + Jetpack Compose native app
crates/mobile_core/      Android-safe Rust facade and bindings
src/core/                Existing controller, dispatch, JSON-RPC contracts
src/openhuman/           Existing domain logic, gated for mobile portability
app/src/                 Desktop React UI remains desktop/web-oriented
app/src-tauri/           Desktop-only Tauri host remains unchanged
```

The Android app should call the mobile facade through generated Kotlin bindings
or JNI. The facade should expose a small lifecycle and RPC API:

- `start_core(options) -> CoreHandle`
- `stop_core(handle)`
- `core_rpc_url(handle) -> String`
- `core_rpc_token(handle) -> String`
- `call_core_rpc(method, params_json) -> result_json`
- optional event stream bridge for notifications and long-running work

The initial implementation can continue to use the core HTTP JSON-RPC server
bound to loopback. A later milestone can replace loopback calls with direct
in-process controller dispatch if Android performance or lifecycle constraints
require it.

## Platform Decisions

### UI

Use Kotlin + Jetpack Compose for the Android UI. This keeps navigation,
permissions, background behavior, notifications, and device-specific UX aligned
with Android conventions.

### Rust Bridge

Prefer UniFFI for generated Kotlin bindings if the facade can be expressed with
stable owned types and JSON payloads. Use hand-written JNI only for cases where
UniFFI blocks lifecycle or async requirements.

### Background Work

Use Android-native background primitives:

- Foreground service only for user-visible long-running work.
- WorkManager for deferrable sync and retryable background tasks.
- Push notifications for server-originated events where possible.
- Android notification channels for user-visible alerts.

Avoid assuming desktop-style always-on background execution.

## Phase 0: Feasibility Spike

Deliverable: a minimal Android-hostable Rust facade that compiles for Android
and can answer `core.ping`.

Tasks:

- Add an Android-safe Rust facade crate or module.
- Build for Android targets, starting with `aarch64-linux-android`.
- Start the existing core JSON-RPC server in-process with a cancellation token.
- Generate or expose a per-process bearer token to Kotlin.
- Add a smoke test or local script that verifies `core.ping` through the mobile
  bridge.

Exit criteria:

- Rust mobile facade compiles for Android.
- Android/Kotlin can start the core, call `core.ping`, and stop the core.
- Desktop builds remain unchanged.

## Phase 1: Dependency and Feature Gating

Deliverable: a clear mobile feature profile for the Rust core.

Tasks:

- Audit root `Cargo.toml` dependencies for Android portability.
- Add a `mobile` or `android` feature profile where necessary.
- Gate or stub non-portable modules:
  - desktop accessibility and text input automation
  - screen intelligence and screen capture
  - local AI installers that assume desktop paths or binaries
  - voice subsystems that depend on desktop audio assumptions
  - service management for launchd, systemd, and Windows services
  - browser/CDP/webview account bridges
  - iMessage, Google Messages, WhatsApp Web, Slack scanner shell modules
- Keep unsupported controller responses explicit, for example
  `platform_supported: false`, rather than letting calls fail with missing
  symbols or runtime panics.

Exit criteria:

- Android core build has an explicit list of included and excluded domains.
- Unsupported features fail gracefully through existing status/controller
  patterns.

## Phase 2: Native Android Shell

Deliverable: a basic installable Android app with session and core lifecycle.

Tasks:

- Scaffold `android/` as a native Android project.
- Add Compose navigation for welcome/login, home, chat, notifications, and
  settings skeletons.
- Wire secure token/session storage using Android Keystore-backed storage.
- Start and stop the Rust core with Activity/Application lifecycle awareness.
- Add a Kotlin RPC client that mirrors `app/src/services/coreRpcClient.ts`
  semantics:
  - bearer token injection
  - timeout handling
  - stable error classification
  - auth-expired event handling
- Add logging and crash-reporting boundaries that do not leak secrets.

Exit criteria:

- Android app boots, starts the core, resolves auth state, and renders the
  signed-in or signed-out route.

## Phase 3: MVP Product Surface

Deliverable: useful Android workflows backed by shared core logic.

Candidate MVP scope:

- Authentication and onboarding state.
- Chat/thread list and basic agent chat.
- Notifications list and triage actions.
- Channels overview for supported channel types.
- Skills catalog metadata browsing, without assuming local skill execution.
- Basic settings for account, analytics consent, backend/core connection, and
  diagnostics.

Out of MVP:

- Desktop webview-account automation.
- Desktop screen intelligence.
- Desktop autocomplete and global hotkey flows.
- Local model installation unless Android-specific runtime support is proven.

Exit criteria:

- A user can sign in, complete mobile-safe onboarding, chat with the agent,
  view notifications, and manage basic settings.

## Phase 4: Background and Realtime Behavior

Deliverable: Android-safe background behavior.

Tasks:

- Decide which realtime socket responsibilities run in Rust core versus Kotlin.
- Map cron and scheduler behavior to Android constraints.
- Add WorkManager jobs for sync that can tolerate process death.
- Add foreground-service UX only for explicit user-visible work.
- Integrate push notifications for remote events where the backend supports it.
- Define battery/data saver behavior and user controls.

Exit criteria:

- The app behaves predictably through backgrounding, process death, network
  changes, and device reboot.

## Phase 5: CI, Testing, and Release

Deliverable: repeatable Android build and validation.

Tasks:

- Add Android Rust compile check for supported ABIs.
- Add Android Gradle build in CI.
- Add emulator smoke coverage for boot, login mock, core ping, and one chat RPC.
- Add unit tests for Kotlin RPC error handling.
- Add docs for local Android setup.
- Add signing and Android App Bundle release path.

Exit criteria:

- CI proves desktop remains green and Android builds are reproducible.
- Release artifacts can be produced without manual local-only steps.

## Main Risks

- Core dependency portability: some root dependencies may not build or run on
  Android without feature gating.
- Long-lived background assumptions: desktop service and scheduler behavior must
  be redesigned for Android lifecycle rules.
- Local AI and audio: mobile hardware, permissions, binary distribution, and
  battery constraints make this a separate product decision.
- Webview account automation: current CEF/CDP architecture is desktop-specific.
- Storage paths and encryption: workspace and credential storage need
  Android-specific policy.
- RPC over loopback: simple for the spike, but may need direct in-process
  dispatch later for lifecycle and performance.

## Open Decisions

- Should the Android app run a local embedded core, talk to a remote/cloud core,
  or support both modes?
- Which domains are required for the first Android release?
- Should the mobile bridge expose JSON-RPC only, typed Kotlin methods, or both?
- How much offline behavior is required?
- Should Android notifications be driven by backend push, local Rust socket
  state, or a hybrid?
- Which local AI features, if any, are product requirements on mobile?

## First Implementation PR

The first PR should be intentionally small:

- Add this plan to the repository.
- Add an Android compile spike for a minimal Rust facade if the build
  environment supports the Android toolchain.
- Do not modify the existing desktop Tauri shell beyond documentation links or
  feature-gating needed to keep desktop builds unchanged.
