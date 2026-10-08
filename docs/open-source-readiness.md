# Open-source readiness: Pulse

Read-only review of `/Volumes/D/claude/cockpit` (product Pulse, target GitHub `Orthic-Labs/pulse`). Nothing was edited, committed, or pushed except this file. Secret values are never printed here; hints show at most four characters.

Reviewed state: `main` at `9ae88471`, with four uncommitted working-tree changes (`hub/src/views/Settings.tsx`, `mac/Notch/Sources/Settings/Preferences.swift`, `mac/Notch/Sources/System/HubBridge.swift`, untracked `mac/.build/`). Those changes are not in history and were not reviewed.

## Summary

- **Secrets: none live found.** gitleaks 8.30.1 over all 255 commits (`rev-list --all`, merges included) reported 15 hits. All 15 are test fixtures, placeholders, SIMD code, or a public key. One item (a 64-hex device secret in a protocol doc, commit `f7dfd810`) is a documented example that the owner should confirm. A separate key-format sweep (AWS, GitHub, Anthropic, OpenAI, Google, Slack, Stripe, npm, Hugging Face, GitLab, JWT, PEM/PGP private keys, Apple AuthKey and issuer IDs, Bearer literals, credential URLs) found no real keys. No `.env`, `.p8`, `.p12`, `.pem`, `.key`, `.mobileprovision`, `.cer`, `.pfx`, `.keystore`, or database file was ever committed.
- **Personal data: none in Pulse-owned code or history.** No `/Users/adrdsouza`, `/Volumes/D`, or `adrdsouza` string exists anywhere in HEAD or in history. The only personal-looking data is third-party email addresses in donor test fixtures and READMEs (inside `vendor/`).
- **Private context: present, mostly in `docs/` and agent files.** The owner's proprietary "HeardRight" checkout path is named in `docs/donors.md`, `docs/composition.md`, `docs/feasibility.md`, and `docs/implementation-plan.md`. `AGENTS.md` and several `docs/` files are internal process documents.
- **Licences: the notch is GPL-3.0-or-later as a whole.** Two Vorssaint GPL-3.0-or-later files sit in the MIT Codenotch fork, so the distributed notch app is a GPL-3.0-or-later combined work. The repo has no root LICENSE, and the README says "Licence: not yet chosen."
- **Biggest blocker is not secrets: it is `vendor/`.** `vendor/` holds 2,632 donor files (about 169 MB), including full AGPL Tinycast and GPL Vorssaint trees, a 12 MB Codenotch `.dmg`, third-party media, donor agent files, and third-party emails. It is not needed to build Pulse, and the donor pins already live in `upstream.lock.json` and `docs/donors.md`.
- **History rewrite: not required for secrets or privacy.** Optional only, to drop donor copies and shrink `.git` (370 MB).

---

## 1. Secrets scan

### Method

1. `gitleaks detect --log-opts=--all` (already installed at `/Users/adrdsouza/.local/bin/gitleaks`, not installed for this review). Result: 236 commits scanned, 15 findings. The tool's count is 19 short of the 255 reachable commits; the output does not explain the gap, so step 2 (which includes merges) covers the full set.
2. Custom sweep of `git log --all -p -m` (merges included, 255 commits), additions only, with 18 key-format and assignment patterns.
3. Path sweep of every path ever added (8,446 unique paths) for `.env`, key, certificate, provisioning, database, and agent-file names.
4. HEAD sweep (`git grep`) for personal paths, emails, private names, and workflow secret names.

### gitleaks findings (all 15 triaged)

| # | Rule | File (as first seen) | Commit | Verdict | Hint |
|---|---|---|---|---|---|
| 1-3 | generic-api-key | `Scripts/raycast-runtime/fixtures.mjs` (WebSocket sample key; `gho_`-prefixed access and refresh token literals) | `a971b22142` | False positive / fixture. GitHub tokens are 40 characters; these literals are 13. | `gho_…` (13 chars), `dGh…` (RFC-style sample) |
| 4-5 | generic-api-key | `mac/Notch/Sources/Vendor/zstd/zstddeclib.c` | `59afd289b5` | False positive (AVX-512 intrinsic code) | n/a |
| 6-8 | generic-api-key | `Tests/GLMUsageTests.swift` (Anthropic-style key literal in parser tests) | `f7dfd81020` | Test fixture; 13-character placeholder | `sk-…` (13 chars) |
| 9 | generic-api-key | `Tests/LMStudioUsageTests.swift` (two short literals in a CLI-credential test) | `f7dfd81020` | Test fixture (low risk). Owner to confirm. | short literal, 8 and 20 chars |
| 10 | generic-api-key | `docs/phone-link-protocol.md` (in HEAD now at `vendor/codenotch/docs/phone-link-protocol.md`) | `f7dfd810` | **Needs owner confirmation.** A 64-hex `deviceSecret` in a documented example with `deviceId` `012345…` and name "Test Phone". Looks like a protocol test vector. Only one occurrence in HEAD and one commit in history. Not a live credential unless a real pairing used it. | `d56d1f…` (64 hex) |
| 11 | generic-api-key | `project.yml` (Codenotch upstream). Pulse still ships the key in `mac/Notch/Sources/Info.plist`. | `f7dfd810` | Public half of an EdDSA (Sparkle) key. Public by design, not a secret. But Sparkle was removed, so the key is dead config. | `rmUfT5…` |
| 12-13 | curl-auth-header | `Tests/MiniMaxCredentialsTests.swift` (placeholder `sk-cp-secret` literal) | `f7dfd810` | Placeholder, not a credential | `sk-cp-…` |
| 14-15 | generic-api-key | `Sources/Vendor/zstd/zstddeclib.c` (upstream copy) | `f7dfd810` | False positive | n/a |

Verdict: **no live credential was found.** Item 10 is the only one that needs owner confirmation. If it was ever a real device pairing secret, rotate it. Removing `vendor/` removes it from HEAD either way.

### Custom sweep results

- Key formats (AWS `AKIA`/`ASIA`, GitHub `gh[pousr]_` and `github_pat_`, Anthropic `sk-ant-`, OpenAI `sk-`/`sk-proj-`, Google `AIza`, Slack `xox`, Stripe live, npm, Hugging Face, GitLab `glpat-`, JWT, PEM/PGP private-key blocks, Apple AuthKey filenames, Apple issuer UUIDs, Bearer literals): **0 hits**.
- Credential URLs: 7 hits, all `user:pass@example.com` test fixtures or `${TAP_…}` environment variable references in a release workflow. Not secrets.
- Assignment-style literals (`…key/token/secret/password = "…"`): 18 distinct hits. All are identifier names such as `totalAvailableTokenEstimation`, `apiKeyEnvironment`, or `cacheReadInputTokens`. None are values.
- Sensitive filenames ever committed: **none** (`.env*`, `*.p8`, `*.p12`, `*.pem`, `*.key`, `*.keystore`, `*.mobileprovision`, `*.cer`, `*.pfx`, `*.netrc`, `*.kdb`, `*.sqlite`, `*.db`). The only binary of concern is `vendor/codenotch/site/Codenotch-1.21.0.dmg` (12.5 MB, added in `f7dfd810`).
- Ignored local directories `local/`, `dist/`, `.audit/`, `node_modules/` were never committed.

### Secret names in CI (names only, no values)

`APPLE_API_ISSUER`, `APPLE_API_KEY`, `APPLE_API_KEY_BASE64`, `APPLE_CERTIFICATE_BASE64`, `APPLE_CERTIFICATE_PASSWORD`, `APPLE_KEYCHAIN_PASSWORD`, `GH_TOKEN`. These are read from GitHub secrets in `release-candidate.yml`, not stored in the repo. The Apple Team ID `6KLGD3LLKF` appears in `docs/helper.md`, `docs/mac-app-delivery.md`, `mac/Notch/Shared/HelperProtocol.swift`, and `scripts/release/candidate.mjs`. It is an identifier embedded in signed binaries, not a secret. Keep it.

---

## 2. Personal and private context

### Personal data

| Item | Where | Finding | Recommendation |
|---|---|---|---|
| Personal paths (`/Users/adrdsouza`, `/Volumes/D`) | HEAD and all history | None | Keep (nothing to do) |
| Owner email | HEAD and history | None | Keep |
| Third-party personal emails in donor tests and READMEs (for example `iabueammar@gmail.com` in Tinycast `README.md` and `AboutView.swift`, `paulo@gmail.com` and `eureka@gmail.com` in Codenotch `ClaudeProfileTests.swift`) | `vendor/` only | Donor data; public upstream but not Pulse's to republish | **Remove** with `vendor/` |
| `someone@acme.co.uk` | `mac/Notch/Sources/Providers/ClaudeProfile.swift:148` (a doc comment) | Illustrative example | Keep |
| Machine-specific paths in Petal tests (`/System/Volumes/Data/Users/sam/…`) | `vendor/petal/src/…` (Petal's own test strings) | Synthetic | Remove with `vendor/`. Pulse's ported code in `core/` has no such strings. |
| Design screenshot `docs/design-refresh/current/hub-now.png` | HEAD | Checked: blank terminal frame | Keep. The other `docs/design-refresh/*.html` mockups and `current/monitor.png` were not viewed; check for real disk names or paths before publishing. |

### Private or owner references

| Item | Where | Finding | Recommendation |
|---|---|---|---|
| "HeardRight" (owner's proprietary app, its `tauri-app-next` checkout path, Rust source paths) | `docs/donors.md` (a whole paragraph), `docs/composition.md:130`, `docs/feasibility.md:12`, `docs/implementation-plan.md:75,108,111,115,116,119` | Names a private codebase and its file paths. Not a licence problem, but it leaks private structure. | **Trim**: delete the paragraph and reword as "a fullscreen-detection reference, not a donor" |
| "Adrian" (`docs/implementation-plan.md:229`), "owner's keyboards" / "owner's" (`:381`, `:398`) | `docs/implementation-plan.md` | Internal attribution | **Trim** |
| Internal workflow and agent process (for example "Primary agent alone owns Git index", test-count rules, "Public Orthic-Labs repo: local-static-only", RightKit notes) | `AGENTS.md` (only agent-instruction file in HEAD) | Agent instructions, not contributor docs | **Trim**: rewrite as `CONTRIBUTING.md` (build and test commands, donor-code rules) and drop agent-process lines. Or remove. |
| `docs/implementation-plan.md` (56 process-word hits), `docs/plan.md` (9), `docs/composition.md` (9), `docs/test-inventory.json` (10), `docs/test-migration.md` (8), `docs/mac-app-delivery.md` (7), `docs/runtime.md` (4), `docs/measurement.md`, `docs/testing.md` | `docs/` | Internal planning, RightKit release evidence, CI run IDs | **Trim** process evidence; keep architecture, licence and build content |
| `docs/design-refresh/BRIEF.md`, `REVIEW-BRIEF.md`, `proposal.md`, `proposal-v2.md`, `review.md`, `review-v2.md` | `docs/design-refresh/` | Internal design-review process, agent briefs | **Remove** the briefs and reviews. Keep final mockups if wanted. |
| `.rightgit.json`, `right-release.config.mjs`, `.github/workflows/*.yml` | root and `.github/` | Generated by "right-git"; reference a `rightkit:` script set and an `xcode-27` runner label | **Decide**: keep if RightKit's build path is meant to be public; otherwise remove. Workflows say "Managed by right-git: do not hand-edit". |
| `core/src/state_migration.rs`, `mac/Notch/Sources/App/ProductMigration.swift` (`dev.orthic.cockpit.hub` legacy paths) | code | Old product name, harmless | Keep |
| `mac/Notch/Sources/Info.plist` (`CFBundleName` and similar still say "Codenotch", plus `SUPublicEDKey`) | notch | Old name; Sparkle key left behind after Sparkle was removed | **Trim**: delete `SUPublicEDKey` and any `SUFeedURL`; rename display strings if the product name matters |
| `windows/` | HEAD | No glyph assets; only 11 files | Keep |
| `mac/.build/` | untracked working tree | Build output, not ignored by the root-anchored `/.build/` rule | **Add** `.build/` to `.gitignore` before publishing |

### Agent-instruction files

| File | Status | Recommendation |
|---|---|---|
| `AGENTS.md` (repo root, in HEAD) | Tracked | Trim or replace with `CONTRIBUTING.md` (see above) |
| `vendor/tinycast/AGENTS.md`, `vendor/tinycast/CLAUDE.md` | Donor files, tracked under `vendor/` | Remove with `vendor/` |
| root `CLAUDE.md` | Not in Pulse history. It exists only inside the `upstream/tinycast` subtree squash (`a971b22142`), a donor file. | Nothing to do for Pulse |
| `docs/agent-rules/*`, `CLAUDE.md` in the workspace root | Outside this repo (`/Volumes/D/claude/docs/...`) | Not in the repo, nothing to publish |
| `.claude/`, `.audit/` | Present locally, git-ignored, never committed | Keep ignored |

---

## 3. Licence obligations

### What the code actually is

| Component | Where | Licence (SPDX / text in repo) | Copyright holder (text) | Used in shipped Pulse? |
|---|---|---|---|---|
| Codenotch fork (the notch) | `mac/Notch/` (about 122 Swift files, `mac/Notch/LICENSE`) | MIT | Copyright (c) 2026 Vinz | Yes. The whole notch app is a fork. |
| Vorssaint (GPL-3.0-or-later) | `mac/Notch/Sources/Conveniences/DiskImageInstaller.swift` and `DiskImageInstallerSupport.swift` (SPDX headers present) | GPL-3.0-or-later | Copyright (C) 2026 Vorssaint | Yes, ported into the notch. The other Vorssaint features were reimplemented per `mac/Notch/FORK.md`. |
| Petal (MIT) | `core/src/platform/mac_bulk.rs` (header), `core/src/cleanup_scan.rs` (comment), `hub/src/chart.ts` (header) | MIT | Copyright (c) 2026 Henry Dennis | Yes, ported code and ideas |
| Uninstally (MIT) | `core/src/app_manager.rs` (header and port comments) | MIT | (c) 2026 Codenta | Yes, ported matching rules |
| Tinycast (AGPL-3.0-or-later) | `vendor/tinycast/` (full copy, 1,196 files, `LICENSE`). No code found in Pulse's product paths. | AGPL-3.0-or-later | Copyright (C) 2026 Abue Ammar | Not shipped as far as the repo shows (see below) |
| zstd (BSD) | `mac/Notch/Sources/Vendor/zstd/` (`LICENSE`, BSD) | BSD | Meta Platforms | Yes, compiled into the notch |
| smartmontools (GPL-2.0) | Not in the repo. `DriveHealth.swift` only looks for `smartctl` in Homebrew paths and in `Contents/Helpers/smartctl`. `scripts/release/` never adds it. | GPL-2.0 (as stated in the review brief) | n/a | **No** (not bundled) |
| Pearcleaner (Apache-2.0 with Commons Clause) | Reference only (`docs/donors.md`). `core/src/app_manager.rs:18` says it was read, not copied. | Apache-2.0 + Commons Clause | n/a | **No** code copied per the repo. The Commons Clause forbids selling it; nothing from it ships. |
| Codenotch glyph licence (LobeIcons MIT) | Only in `vendor/codenotch/Sources/Resources/LobeIcons-LICENSE.txt`; no icon files shipped | MIT (LobeHub) | Copyright (c) 2023 LobeHub | No icon files shipped |
| Claude and OpenAI marks | `mac/Notch/Sources/Providers/GlyphOutline.swift` (outlines traced from a Codenotch design frame, `docs/design/frame-124-…png`, which is not in Pulse) | Trademarks of Anthropic and OpenAI | n/a | Yes, as provider marks. Trademark, not copyright, issue. |

Also in the repo: 770 SPDX GPL-3.0-or-later headers, all under `vendor/vorssaint/` except the two notch files. The `vendor/` copies are pristine donor trees, not Pulse code.

Not checked in this review: dependency licences from `Cargo.lock`, `pnpm-lock.yaml`, `hub/pnpm-lock.yaml`, `Package.resolved`, and `hub/src-tauri`'s crates. Those need a separate licence scan.

### Consequences, plainly

1. **GPL-3.0-or-later combined work.** `DiskImageInstaller.swift` and `DiskImageInstallerSupport.swift` are GPL-3.0-or-later and sit in the notch. Because they are compiled into the notch app, the notch is a combined work. Distributing the notch (the signed Pulse.app or Pulse.dmg) therefore requires offering its complete corresponding source under GPL-3.0-or-later (or a GPL-compatible licence that allows GPL-3.0). The MIT notch code can go under GPL-3.0-or-later without conflict, but the reverse is not true. Publishing the repo under MIT or Apache-2.0 would be a licence violation for the shipped app.
2. **MIT notices must travel with the code.** Codenotch, Petal, and Uninstally are MIT. Each requires that the copyright line and permission text be included in copies or substantial portions. `mac/Notch/LICENSE` covers Codenotch. Petal and Uninstally have only in-file comments and no full text in the repo (the donors doc says "Original MIT notice is included in app bundle" but that was not verified in the bundle). Add their full texts to a NOTICE file.
3. **Tinycast AGPL is a conditional blocker.** If any Tinycast code had been copied into the notch, the notch would have to be AGPL-3.0-or-later, not just GPL-3.0-or-later. AGPL adds a source-offer obligation for networked use, and the licence would govern the whole combined app. The repo shows no copied code: a line-level comparison of all 778 Tinycast Swift files against `mac/Notch/Sources` found only generic AppKit one-liners, and no Tinycast-specific identifiers (`AppIndex`, `PaletteState`, `HotKeyManager`, `AppCore`, `HyperKeyTap`) appear in any product path in history. `docs/implementation-plan.md` and `docs/composition.md` describe the Launcher as a planned Tinycast extraction, and `mac/Notch/Sources/Launcher/` (six files, added in `a5f1c4b7`) has no attribution. **Ask Adrian** whether the Launcher was written from scratch or extracted from Tinycast. The answer decides whether AGPL applies.
4. **Vendored AGPL and GPL trees.** Keeping `vendor/tinycast` and `vendor/vorssaint` in a public repo redistributes those licensed works. That is allowed if their notices and licence texts stay in place, which they do. It is not allowed to remove those notices. Removing `vendor/` avoids the question and shrinks the repo by 169 MB.
5. **smartmontools (GPL-2.0), if it is ever bundled.** It is not bundled today. If `Contents/Helpers/smartctl` is added to the release payload, the app would need GPL-2.0 licence text and source offers for smartmontools.
6. **Trademarks.** Claude and OpenAI logos are drawn as traced outlines in `GlyphOutline.swift`. Licence-wise they are not MIT-licensed assets, and the provider marks are third-party trademarks. Plan to add a "not affiliated with Anthropic or OpenAI" line and nominative-use wording, or drop the marks.

---

## 4. Recommendations (ordered checklist)

1. **Confirm two facts with Adrian before publishing.** (a) Was the Launcher written from scratch or extracted from Tinycast? (b) Is the phone-link `deviceSecret` (commit `f7dfd810`) a documented test vector or a real pairing secret? If real, rotate it. Nothing else needs rotation.
2. **History rewrite: not required.** No live secrets, no `.env` or key files, no personal paths or emails in Pulse code, and donor data is already public upstream. A rewrite would only shrink `.git` from 370 MB by dropping the donor copies. If you want that, use `git filter-repo` before the first public push, because it changes every SHA. Do not do it after anyone forks the repo.
3. **Remove `vendor/` from HEAD.** 2,632 files, 169 MB, including the 12 MB Codenotch `.dmg`, the 30 MB Vorssaint `demo.gif`, and 27 MB of Tinycast video. Keep `upstream.lock.json` and `docs/donors.md` as the pin record. Recreate donor copies only on demand, in a scratch directory outside the repo, if you still need diffs.
4. **Trim private and process content.** Remove the HeardRight paragraph and references (`docs/donors.md`, `docs/composition.md`, `docs/feasibility.md`, `docs/implementation-plan.md`). Delete `docs/design-refresh/` briefs and reviews. Trim RightKit, CI run IDs, and "Adrian" process text from `docs/`. Replace `AGENTS.md` with a `CONTRIBUTING.md` that has build and test commands and the donor-code rules.
5. **Remove dead Sparkle config.** Delete `SUPublicEDKey` (and any `SUFeedURL`) from `mac/Notch/Sources/Info.plist`. Sparkle was removed from the fork.
6. **Add `.build/` to `.gitignore`** (the root-anchored `/.build/` rule misses `mac/.build/`). Do not publish the four uncommitted changes without review.
7. **Pick the licence.** See the options below. Recommended: GPL-3.0-or-later for the whole repository.
8. **Add LICENSE and NOTICE** (layout below). Keep `mac/Notch/LICENSE` (Codenotch MIT).
9. **Verify the shipped app carries the MIT notices.** The donors doc claims the Codenotch notice is in the app bundle. Confirm this in a built `Pulse.app`, and add Petal and Uninstally notices too.
10. **Resolve the trademark items.** Add a non-affiliation line for the Claude and OpenAI marks, or remove the outlines. Also fix the stale comment in `GlyphOutline.swift` that points to a design frame that is not in the repo.
11. **Check CI exposure.** `ci.yml` runs on `pull_request` with `runs-on: xcode-27`. Confirm this is a GitHub-hosted label. If it is self-hosted, do not run it for fork pull requests on a public repo. In either case, set Actions to require approval for outside contributors.
12. **Run a dependency licence scan** over `Cargo.lock`, `pnpm-lock.yaml`, `hub/pnpm-lock.yaml`, and `Package.resolved`. This review did not do it. Use `cargo deny` or the pnpm licence checker only if they are already installed.
13. **Re-scan the final tree and enable protection.** Run gitleaks on HEAD after the cleanup, enable GitHub secret scanning and push protection on the new public repo, and re-check design screenshots (`docs/design-refresh/*.html`, `current/monitor.png`) for real machine data.

### Licence options

| Option | What it means | Fit |
|---|---|---|
| **A. GPL-3.0-or-later for the whole repo (recommended)** | One licence. Compatible with the GPL-3.0-or-later Vorssaint files, the MIT Codenotch, Petal, and Uninstally code, and BSD zstd. Matches what the shipped notch must already be. | Simplest and legally safe for the current code |
| B. Split: GPL-3.0-or-later for `mac/Notch/` and MIT or Apache-2.0 for `core/`, `hub/`, `scripts/` | Possible, since `core/` and `hub/` are separate processes. But the notch app ships them in one bundle, and per-directory licences are easy to get wrong. | Only if you want the core to be reusable under a permissive licence. Needs a careful per-directory LICENSE index. |
| C. AGPL-3.0-or-later | Required only if Tinycast code is absorbed (point 3 above). Otherwise stronger than needed. | Hold in reserve |
| D. Proprietary or "source-available" | Not open source. The GPL-3.0 notch and MIT donors already force source disclosure for the notch. | Not recommended |

Copyright holder for Pulse-owned code is a decision for Adrian (for example "Orthic Labs" or the individual). Consider a DCO sign-off in `CONTRIBUTING.md` instead of a CLA.

### LICENSE and NOTICE layout

```
LICENSE                      GPL-3.0 text (for GPL-3.0-or-later, all Pulse-owned code)
NOTICE                       Third-party notices, with full texts:
                               Codenotch  MIT  (Copyright (c) 2026 Vinz)
                               Petal      MIT  (Copyright (c) 2026 Henry Dennis)
                               Uninstally MIT  ((c) 2026 Codenta)
                               Vorssaint  GPL-3.0-or-later (two files, SPDX headers kept)
                               zstd       BSD  (Meta Platforms)
                               Trademark statement (Claude, OpenAI, Pulse, Codenotch)
mac/Notch/LICENSE            Keep: Codenotch MIT text (required copy of notice)
mac/Notch/Sources/Vendor/zstd/LICENSE   Keep: BSD text
upstream.lock.json           Keep: donor pins (no secrets)
docs/donors.md              Keep, minus the HeardRight paragraph
```

Per-file SPDX headers stay on the Vorssaint-derived files. Add `SPDX-License-Identifier: GPL-3.0-or-later` headers to any new Pulse files, if you want per-file markers.

---

## Method notes and limits

- gitleaks ran with `--redact`. Its report was written to the scratchpad and read only through masked summaries. No secret value appears in this file.
- Not verified: whether the Tinycast Launcher was derived from Tinycast (answer from the owner, see checklist item 1), and whether the Codenotch MIT notice ships inside the built app.
- The sweep read additions only (`git log -p`). Deleted-only content is not in the scan but is still in history, so the same checks apply to it.
- One Bash command was blocked by a policy hook on credential-related terms in its command text. The sweep was rerun from a scratchpad script with masked output, and no credential was read outside the scan.
- Dependency licences, the full Pulse-owned `docs/` text, and the design images other than the blank `hub-now.png` were not reviewed.
