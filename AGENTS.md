# Cockpit

Mac notch (Codenotch fork, Swift), Tauri hub, shared Rust core & CLI; Windows later. Source plan: docs/plan.md. No Dock or menu-bar item; notch right-click shows only Quit.

Use primary checkout. Preserve concurrent changes. Primary agent alone owns Git index, commits, upstream import & pushes.
Public Orthic-Labs repo: local-static-only. Compile, tests, packaging & signing use generated RightKit GitHub workflows; never run local Cargo/Swift builds. No private data, captured machine paths, credentials or usage snapshots in commits.
Do not install apps, change shortcuts, disable existing utilities or enable cleanup while feasibility gates are unresolved. Donor extraction follows pinned inventory & licence review. No fake telemetry or reclamation claims. Unknown liveness/metadata is explicit & never eligible for cleanup.

Add only substantial end-to-end user journeys; do not add new small tests or pad test counts. Verify installed native interactions, rendered outcomes, failure recovery & retained state. Report total test inventory at delivery, distinguishing existing component tests from end-to-end journeys. Follow docs/testing.md. Treat green helper tests as component evidence only.

Prefer quality over quantity: add no unit, component or helper tests; extend complete E2E journeys instead. Retain existing tests until replacement journeys prove their behavior.

Every test migration must inventory exact deleted test names/files, replacement E2E journeys, retained tests & before/after totals. Map removed behavior to observed replacement coverage; never claim replacement from a renamed unit test or an unexecuted journey.
