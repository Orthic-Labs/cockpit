# Cockpit

Native Mac Swift notch & native Windows Rust notch; shared Rust core, on-demand worker/CLI & storage dashboard. Source plan: docs/implementation-plan.md.

Use primary checkout. Preserve concurrent changes. Primary agent alone owns Git index, commits, upstream import & pushes.
Public Orthic-Labs repo: local-static-only. Compile, tests, packaging & signing use generated RightKit GitHub workflows; never run local Cargo/Swift builds. No private data, captured machine paths, credentials or usage snapshots in commits.
Do not install apps, change shortcuts, disable existing utilities or enable cleanup while feasibility gates are unresolved. Donor extraction follows pinned inventory & licence review. No fake telemetry or reclamation claims. Unknown liveness/metadata is explicit & never eligible for cleanup.
