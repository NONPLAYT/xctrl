# CLAUDE.md

`xctrl` — Xray control plane for a NixOS fleet. One binary, three roles: `agent` (per node, drives xray over gRPC), `serve` (one host: quota enforcement + subscriptions), and management subcommands. Public repo, MIT.

## The one rule that shapes everything

**Nix owns the config; this program owns almost no state.** Users, groups, nodes, inbounds and quotas arrive read-only in `/run/secrets/xctrl.json`, rendered by a sops template in the operator's flake. Never add a code path that writes config, mutates the roster, or asks the user to run a setup wizard — the answer to "where does X get configured" is always "in Nix".

The only things on disk (`/var/lib/xctrl`) are what Nix cannot know: bytes spent, who is cut off, when the period rolled over. Adding a file there needs a real justification.

## Hard rules

- No database. If something feels like it wants SQLite, it belongs in Nix or it is derived state.
- `cargo test` must pass and `cargo clippy` must be clean before you call anything done.
- Format with `cargo fmt`.
- `protoc` is needed to build: `nix develop`, or set `PROTOC`.
- Never commit anything resembling a real uuid, token, fqdn or reality key — not in tests, not in docs. Tests use obvious fakes.

## Mechanics

- `config.rs` is the contract with the Nix side. Changing a field there is a breaking change for the operator's flake: say so explicitly, and keep `#[serde(default)]` on anything optional so old configs keep parsing.
- Byte sizes deserialize from `"200G"` or a raw number via `Size`; quotas resolve user limit → group limit → unlimited (`Config::limit_of`). A user's `limit` is a `Limit`, not a `Size`, so `"unlimited"` can override a group default.
- `User.expires` is a date, evaluated at the end of that day in the quota offset (`Quota::expiry`). The controller keeps expired users in `Ledger.expired`, which — unlike `limited` — is not cleared when the period rolls over.
- Quota boundaries are computed in a fixed UTC offset (`Quota::reset_utc_offset_hours`), never host local time. Day 31 clamps to the month's length. This is tested — do not "simplify" it into `chrono::Local`.
- UA detection is an ordered table, first match wins; more specific patterns go first (clash-meta before clash). Unknown clients get base64.
- Traffic is read from xray with `reset: true`, so the running total only survives in the state file. Any code path that reads stats and drops the result loses bytes permanently.
- Link building is protocol-agnostic: `Link { scheme, port, params }` from config. A new protocol should be a config change, not a match arm.
