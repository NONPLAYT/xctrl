# xctrl

Xray control plane for a NixOS fleet. One Rust binary that acts as a **node agent**, a **quota controller** and a **subscription server** — no web panel, no database.

The distinguishing idea: **the NixOS flake is the source of truth.** Users, groups, nodes, inbounds and quotas are declared in Nix and rendered into a single JSON config through a sops template. `xctrl` only ever reads that file. The only state it owns is what Nix cannot know — bytes actually spent, who is currently cut off, and when the billing period last rolled over.

That makes this deliberately unportable: it is built for NixOS and assumes sops-nix. If you want a general-purpose panel, look elsewhere.

## Roles

| command | runs where | does |
|---|---|---|
| `xctrl agent` | every node | projects the user list onto the local xray over gRPC, collects traffic |
| `xctrl serve` | one host | polls agents, sums traffic fleet-wide, enforces quotas, serves subscriptions |
| `xctrl ls\|status\|sync\|block\|unblock\|export` | anywhere with the token | management over the agents' HTTP API |

## Groups

A group is a tenant: a set of users served by a set of node inbounds. Two groups on one node means two independent VPNs sharing a machine — each with its own xray inbound, its own reality keys and its own traffic accounting. Users never see nodes outside their group.

## Quotas

Per-user byte limits, written the way humans write them (`"200G"`), or `"unlimited"` to opt out of the group default. Exhausting the limit removes the user from every inbound until the period rolls over. A user may also carry an `expires` date, after which they are cut off for good — a new period resets traffic, not validity.

The reset boundary is a day of the month (1–31, clamped to the last day of shorter months) at a fixed hour, pinned to a **fixed UTC offset** from the config rather than host local time — nodes in different timezones must agree on the boundary, and a DST shift must not move it.

## Subscriptions

The client is detected by User-Agent and gets the format it understands: Clash / Clash.Meta, sing-box, Xray, Happ, plain links or base64. Browsers get an HTML dashboard with a QR code instead.

Routing policy travels with the subscription: the Clash formats carry a full profile — DNS, sniffer, rule providers, rules — and Happ gets a `happ://routing/onadd/…` line it imports on add. Both come from the operator's flake, not from this repo.

## Install

```bash
nix run github:NONPLAYT/xctrl -- --help
nix build github:NONPLAYT/xctrl        # ./result/bin/xctrl
```

## Development

```bash
nix develop        # cargo, clippy, rust-analyzer, protobuf
cargo test
```
