# Test Coverage Matrix

This matrix was reset for the closedhuman fork after removal of the hosted
OpenHuman backend, backend login, billing, team, rewards, and backend-proxied
integration flows.

Current merge coverage should be derived from:

| Area | Primary coverage |
| --- | --- |
| Core JSON-RPC and local app state | `tests/json_rpc_e2e.rs`, focused Rust unit tests |
| Native OAuth and direct providers | `src/openhuman/oauth/**`, `src/openhuman/providers_native/**` tests |
| Direct Composio mode and local webhooks | `src/openhuman/composio/**` tests, `app/test/e2e/specs/composio-triggers-flow.spec.ts` |
| Local channel runtime | `src/openhuman/channels/tests/**`, `app/test/e2e/specs/channels-smoke.spec.ts` |
| Frontend shell and settings | co-located Vitest under `app/src/**`, current WDIO specs under `app/test/e2e/specs/` |
| Local AI, voice, memory, tools | focused Rust/Vitest tests plus the surviving WDIO specs |

When adding or changing a user-visible feature, update the nearest focused test
or add a new one next to the changed code. Backend-auth, billing, team, referral,
and rewards rows from the upstream matrix intentionally no longer apply.
