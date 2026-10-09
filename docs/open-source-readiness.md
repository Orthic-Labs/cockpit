# Release readiness: Pulse

Pulse is proprietary. See [LICENSE](../LICENSE) (Copyright (c) 2026 Damned Ventures LLC, d/b/a Orthic Labs, all rights reserved). Third-party components and their licence texts are listed in [NOTICE](../NOTICE). Vendored or third-party directories keep their own licences where they declare one.

Pulse is not an open-source project and this file is not a licence plan. The checklist below is what to keep true before any push or publication:

- No secrets, keys, certificates, provisioning profiles or `.env` files in commits.
- No private data, captured machine paths, usernames or usage snapshots in commits or docs.
- Every bundled third-party component is listed in NOTICE with its licence text where that licence requires it.
- Build output (`mac/.build/`) stays out of Git (`.gitignore` anchors `/.build/` only at the root).
- Signing, notarization and publication run through the RightKit release workflows, not on developer machines.
