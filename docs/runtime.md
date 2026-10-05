# Runtime ownership

Target architecture retains native always-on pills. No background webview or always-on scanner is introduced.

| Owner | Reads/owns | Write boundary |
| --- | --- | --- |
| Native pill (one per user) | Cheap counters, provider usage snapshots, settings, monitor placement | Sole settings writer; atomic schema-versioned preferences |
| On-demand worker | Bounded scans, findings, plans, job journal | Sole mutations executor after plan validation; exits idle |
| Shared dashboard | Resources/Storage/Cleanup/Uninstall/Settings presentation | Sends settings requests to pill & jobs to worker |
| CLI | Same core & versioned output | Read-only bootstrap; explicit `scan --save` writes private metadata only |

Implemented bootstrap storage is versioned JSON snapshots, with owner-only Unix files & atomic no-replace publication. Newly created Unix state directories use private modes; existing directory permissions are preserved. Windows private ACL creation remains pending. Snapshot IDs are validated, files are size-bounded & skipped corrupt history is reported through structured stderr events. CLI findings read last stored snapshot. No destructive jobs, plan database, worker endpoint, pill settings channel or shared usage-reader owner is enabled yet.

Local IPC design: Unix-domain socket (Mac) / per-user named pipe (Windows), peer identity checked before requests; no TCP listener. Envelope carries schema version, request ID, operation & bounded payload. Each surface obtains its own per-user instance lock; later launches forward activation then exit. Worker job lifecycle emits started/progress/completed/failed events with job ID, inspected target identity & per-item outcome. Unknown or lost state cannot become completed.

Mutation journal design: persist plan effect, recursive inspected identities, expiry & exact executor arguments before claim. Atomic one-time claim precedes per-item started/completed records. Recovery marks interrupted effects indeterminate until observed state resolves them; it never repeats a possibly completed removal. Settings migrations & job schema upgrades remain separate from metadata snapshots.

## Platform identity

Product bundle identity: `dev.orthic.cockpit`; native pill & bundled worker keep stable bundle/code identity across RightKit updates. Permission testing must use signed app identity, not an unsigned SwiftPM executable. Accessibility/Input Monitoring/Automation belong to Mac pill; Full Disk Access coverage for bundled worker & Terminal-launched CLI must be tested separately. Existing provisioned RightKit signing owns signing & updater integration; this repository adds no signer or installer machinery.
