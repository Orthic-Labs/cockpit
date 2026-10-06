# Test migration inventory

This inventory records migration of newly supplied feature tests to complete journeys. It covers only this migration batch; retained tests remain in suite until matching product-boundary journeys exist.

Counts are declarations, not assertions:

- Proposed deletions: 65 entries (49 Rust declarations, 15 Swift declarations & 1 dashboard assertion script). Review found incomplete replacement coverage; all 65 were restored.
- Deleted: 0 declarations. Candidate files were restored from baseline before Rust formatting; exact declaration names remain unchanged.
- Added: 1 packaged Mac E2E declaration, `DashboardHostE2ETests.testPackagedDashboardScansFixtureThroughNativeHostAndRetainsResults`.
- Existing journeys provide partial evidence: `core/tests/cli.rs::storage_cli_journey_is_bounded_opt_in_and_preserves_fixture_bytes`, `NativeCleanupJourneyTests.testReviewApplyRestartUndoConflictAndAncestorProtection`, `NativeStorageServicesTests.testNativeCompressionJourneyPersistsActivityAndProtectsExistingOutput`, and installed CUA journey in `scripts/qa/mac-installed-journey.mjs`.
- Exact one-for-one replacements: 0. Candidate assertions stay retained; rows below mark journey coverage without claiming conversion.

Baseline source inventory is 484 declarations: core inline 13, core tests 241, Windows 96, Mac tests 104 & Node tests 30. Dashboard helper adds one standalone assertion script outside declaration count. Current source count is 485 after adding one packaged journey; dashboard helper remains retained. [Full before/after inventory](test-inventory.json) records every name, file, hash & count. Regenerate current source inventory with `node scripts/qa/test-inventory.mjs`; it reads source without running tests.

New packaged journey & all-section/native filename-index extension passed hosted Mac candidate [37470104466](https://github.com/Orthic-Labs/cockpit/actions/runs/37470104466) at source `b682deedef71ebf28473b71c4ff0ee44f4b0332c`. It exercises real helper → native host → WKWebView → retained window state, with automated JavaScript interactions. Installed picker/accessibility journeys remain separate evidence: two runner scripts are present. Storage delivery journey completed with partial receipt: 18 phases passed, six Trash-dependent phases blocked, hover unrun; [qualification scope](testing.md) records observed behavior. No retained declaration has been deleted or counted as replaced.

The packaged journey builds `dashboard/app.js` and `index.html` from current source using release packaging transformation, copies `style.css`, launches `ProcessScanRunner` through `DashboardHost.verifyBundledScan` under `NSApplication.shared`, scans two real files including `subfolder/example.txt`, checks one and zero search results, closes/reopens dashboard state, and keeps persistent state outside selected scan root. It requires `COCKPIT_TEST_HELPER`; missing helper is a failure.

## Rust candidate declarations retained

| File / declaration | Existing journey evidence | Conversion status |
| --- | --- | --- |
| `core/tests/storage_browser.rs::search_includes_hidden_files_filters_extension_and_paginates` | CLI storage journey `find` | Partial: real scan/find is covered; hidden-file, extension, and pagination assertions remain open. |
| `core/tests/storage_browser.rs::malformed_query_and_limits_are_rejected` | None | Open: browser query, limit, offset, and size-range validation has no product-boundary assertion. |
| `core/tests/storage_browser.rs::overlap_duplicates_collapse_without_merging_distinct_hardlinks` | CLI storage journey `duplicates` | Partial: real duplicate files are grouped; hardlink identity and browser overlap behavior remain open. |
| `core/tests/storage_browser.rs::incomplete_and_placeholder_state_is_preserved_without_hydration` | Installed CUA import/recovery journey | Partial: real import and failure recovery are covered; placeholder and incomplete browser rendering remain open. |
| `core/tests/storage_browser.rs::drilldown_is_direct_child_only_and_rejects_outside_path` | CLI storage journey `browse` | Partial: real root browse is covered; direct-child filtering and outside-root rejection remain open. |
| `core/tests/storage_browser.rs::largest_folders_are_deterministic_and_report_incompleteness` | CLI storage journey `export` | Partial: real export is covered; deterministic largest-folder and incomplete flags remain open. |
| `core/tests/folder_growth.rs::signed_changes_added_removed_and_deterministic_order_are_reported` | CLI storage journey second scan and `history` | Partial: real growth and history comparison are covered; full added/removed ordering is open. |
| `core/tests/folder_growth.rs::incomplete_snapshot_refuses_folder_totals` | None | Open: incomplete folder comparison refusal is not exercised at a product boundary. |
| `core/tests/folder_growth.rs::remount_and_root_change_use_history_compatibility_guard` | None | Open: remount and root compatibility rejection is not exercised. |
| `core/tests/folder_growth.rs::missing_identity_duplicate_and_out_of_scope_rows_refuse_ambiguity` | None | Open: ambiguous identity, duplicate, and out-of-scope rows are not exercised. |
| `core/tests/folder_growth.rs::u64_extremes_use_signed_i128_without_underflow` | None | Open: integer-boundary encoding is not exercised. |
| `core/tests/folder_growth.rs::each_output_list_is_bounded_and_truncation_is_explicit` | None | Open: folder comparison list bounds are not exercised. |
| `core/tests/folder_growth.rs::caller_limit_is_capped_and_reported` | None | Open: caller limit capping is not exercised. |
| `core/tests/folder_growth.rs::dot_components_in_roots_or_folder_rows_are_rejected` | None | Open: dot-component path rejection is not exercised. |
| `core/tests/folder_growth.rs::empty_folder_coverage_for_nonempty_report_is_rejected` | None | Open: missing folder coverage rejection is not exercised. |
| `core/tests/folder_growth.rs::skipped_symlink_note_alone_does_not_invalidate_complete_comparison` | None | Open: skipped-link comparison semantics are not exercised. |
| `core/tests/folder_growth.rs::attributed_direction_wins_when_logical_direction_differs` | None | Open: attributed-direction selection is not exercised. |
| `core/tests/duplicates.rs::exact_bytes_form_group_and_changed_content_does_not` | CLI storage journey `duplicates` | Covered at boundary for equal bytes vs changed bytes. |
| `core/tests/duplicates.rs::hardlinks_are_skipped_by_identity_and_symlinks_are_refused` | None | Open: duplicate command hardlink and symlink handling is not exercised. |
| `core/tests/duplicates.rs::injected_partial_reader_is_reported_without_group` | None | Open: injected-reader failure seam has no real adapter journey. |
| `core/tests/duplicates.rs::read_budget_and_deadline_truncate_before_unbounded_content_reads` | None | Open: duplicate read budget and deadline behavior is not exercised. |
| `core/tests/apps.rs::ownership_requires_exact_identity_and_excludes_shared_user_data` | CLI storage journey exported apps module | Partial: real module envelope is present; ownership exclusion semantics remain open. |
| `core/tests/apps.rs::projector_uses_supplied_records_and_deduplicates_overlap_and_hardlinks` | CLI storage journey exported apps module | Partial: real module export is present; projection and identity deduplication remain open. |
| `core/tests/apps.rs::missing_external_or_portable_coverage_keeps_disappearance_unknown` | None | Open: coverage-qualified disappearance is not exercised. |
| `core/tests/apps.rs::malformed_and_unavailable_updates_are_distinct_and_network_free` | None | Open: update failure distinctions and network-free behavior are not exercised. |
| `core/tests/apps.rs::bounded_history_keys_samples_by_incarnation_and_session` | None | Open: app history incarnation/session keys are not exercised. |
| `core/tests/apps.rs::byte_totals_report_units_and_saturating_overflow` | None | Open: app byte totals and overflow are not exercised. |
| `core/tests/activity.rs::duplicate_event_is_idempotent_but_changed_duplicate_is_rejected` | CLI storage journey exported activity module | Partial: activity events cross real CLI export; idempotence and conflict rejection remain open. |
| `core/tests/activity.rs::moved_logical_and_observed_delta_stay_separate` | CLI storage journey `history` | Partial: real history delta is covered; moved-vs-observed separation remains open. |
| `core/tests/activity.rs::weekly_and_monthly_windows_are_utc_and_have_explicit_bounds` | None | Open: UTC window boundaries are not exercised. |
| `core/tests/cleanup.rs::overlapping_targets_are_deduplicated_to_outer_target` | Native cleanup journey | Partial: native rename/undo is covered; overlapping target planning remains open. |
| `core/tests/cleanup.rs::protected_descendant_refuses_plan_before_claim` | Native cleanup journey | Partial: native ancestor replacement protection is covered; pre-claim protected-descendant refusal remains open. |
| `core/tests/cleanup.rs::claim_is_one_time_and_expiry_is_enforced` | Native cleanup journey | Partial: one-time apply/undo claims are covered; expiry is open. |
| `core/tests/cleanup.rs::changed_effect_cannot_reuse_claim` | None | Open: changed-effect claim invalidation is not exercised. |
| `core/tests/cleanup.rs::interrupted_item_is_indeterminate_and_never_replayed` | Native cleanup journey | Covered: real rename interruption, restart, recovery, and no replay are exercised. |
| `core/tests/cleanup.rs::changed_identity_is_recorded_as_item_failure_without_execution` | Native cleanup journey | Partial: real conflict outcome is covered; pre-execution identity change remains open. |
| `core/tests/cleanup.rs::undo_only_uses_confirmed_moves_and_reports_restore_conflict` | Native cleanup journey | Covered: real undo and occupied-path conflict are exercised. |
| `core/tests/compression.rs::malformed_parameters_are_rejected_before_io` | Native storage journey | Partial: real ImageIO request runs; malformed parameter rejection remains open. |
| `core/tests/compression.rs::unsupported_codec_is_explicit` | None | Open: unsupported codec handling is not exercised. |
| `core/tests/compression.rs::source_safety_failures_are_typed` | Native storage journey | Partial: real source preservation is covered; typed safety failures remain open. |
| `core/tests/compression.rs::failure_and_cancel_never_complete` | None | Open: native compression failure and cancellation paths are not exercised. |
| `core/tests/compression.rs::measured_output_is_read_separately_from_estimate` | Native storage journey | Covered: real output bytes, measured savings, and persisted activity are exercised. |
| `core/tests/compression.rs::finalize_rejects_unsafe_or_source_identity_output` | Native storage journey | Partial: real output protection is covered; unsafe/source-identity rejection is open. |
| `core/tests/monitor.rs::rate_requires_interval_and_handles_zero_time` | CLI storage journey exported monitor module | Partial: real monitor envelope is exported; rate interval behavior remains open. |
| `core/tests/monitor.rs::rate_rejects_time_backwards_counter_reset_and_overflow_shape` | None | Open: counter reset and time reversal are not exercised. |
| `core/tests/monitor.rs::battery_rejects_invalid_units_and_preserves_valid_reading` | None | Open: battery normalization is not exercised. |
| `core/tests/monitor.rs::listener_owner_requires_start_time_and_rejects_pid_reuse` | None | Open: listener identity ownership is not exercised. |
| `core/tests/monitor.rs::history_is_bounded_and_pid_reuse_gets_new_identity` | None | Open: process history incarnation handling is not exercised. |
| `core/tests/monitor.rs::process_history_skips_invalid_cpu_or_unknown_memory` | None | Open: invalid process metrics are not exercised. |

## Dashboard candidate declaration retained

| File / declaration | Existing journey evidence | Conversion status |
| --- | --- | --- |
| `dashboard/app.test.mjs` (one executable helper-test script) | New packaged Mac E2E journey | Partial: actual classic bundle loads in WKWebView, scan import works, one/zero search results work, and close/reopen retains state; pure normalization, path, formatting, and envelope edge cases are no longer asserted directly. |

## Swift candidate declarations retained

| File / declaration | Existing journey evidence | Conversion status |
| --- | --- | --- |
| `mac/Tests/CockpitMacPrototypeCoreTests/DashboardHostTests.swift::testScanRequestArgvIsExplicitRootNoShell` | New packaged Mac E2E journey | Covered at boundary: real helper is launched with real scan root and no shell. |
| `mac/Tests/CockpitMacPrototypeCoreTests/DashboardHostTests.swift::testRunnerCapturesBoundedStdout` | New packaged Mac E2E journey | Partial: real bounded runner returns actual scan output; synthetic echo output is not asserted. |
| `mac/Tests/CockpitMacPrototypeCoreTests/DashboardHostTests.swift::testRunnerTruncatesOverLimitStdout` | None | Open: runner output-limit failure is not exercised. |
| `mac/Tests/CockpitMacPrototypeCoreTests/DashboardHostTests.swift::testRunnerKillsChildBeforeDeadline` | None | Open: deadline termination and orphan prevention are not exercised. |
| `mac/Tests/CockpitMacPrototypeCoreTests/DashboardHostTests.swift::testRunnerNonZeroExit` | None | Open: nonzero helper exit mapping is not exercised. |
| `mac/Tests/CockpitMacPrototypeCoreTests/DashboardHostTests.swift::testRunnerCancelTerminatesChild` | None | Open: cancellation termination is not exercised. |
| `mac/Tests/CockpitMacPrototypeCoreTests/DashboardHostTests.swift::testScanObjectAcceptsSnapshotEnvelope` | New packaged Mac E2E journey | Covered at boundary: real CLI snapshot envelope is parsed and imported. |
| `mac/Tests/CockpitMacPrototypeCoreTests/DashboardHostTests.swift::testScanObjectRejectsTruncated` | None | Open: truncated output rejection is not exercised. |
| `mac/Tests/CockpitMacPrototypeCoreTests/DashboardHostTests.swift::testScanObjectRejectsEmptyAndMalformed` | None | Open: empty and malformed envelope mapping is not exercised. |
| `mac/Tests/CockpitMacPrototypeCoreTests/DashboardHostTests.swift::testImportJavaScriptCallsAllowlistedEntry` | New packaged Mac E2E journey | Covered at boundary: generated import script invokes real dashboard importer. |
| `mac/Tests/CockpitMacPrototypeCoreTests/DashboardHostTests.swift::testImportJavaScriptEscapesBreakoutSequences` | None | Open: hostile serialized payload escaping is not exercised. |
| `mac/Tests/CockpitMacPrototypeCoreTests/DashboardHostTests.swift::testNavigationAllowsOnlyBundledSubtree` | New packaged Mac E2E journey | Partial: real bundled file navigation is exercised; hostile URL rejection remains open. |
| `mac/Tests/CockpitMacPrototypeCoreTests/DashboardHostTests.swift::testScriptMessageAllowlistIsClosed` | None | Open: script-message allowlist is not exercised. |
| `mac/Tests/CockpitMacPrototypeCoreTests/DashboardHostTests.swift::testCoordinatorMissingHelper` | None | Open: missing-helper error is intentionally a required environment failure, not a skipped test. |
| `mac/Tests/CockpitMacPrototypeCoreTests/DashboardHostTests.swift::testCoordinatorBuildsImportScript` | New packaged Mac E2E journey | Covered at boundary: actual helper request produces actual dashboard import. |

## Core retained declaration inventory

`core/src/compression.rs` — 4 declarations: `plan_keeps_estimate_separate_from_measured_savings`, `unsafe_and_unknown_sources_are_refused`, `cancellation_is_terminal_and_does_not_publish`, `existing_output_is_refused`

`core/src/ipc/mod.rs` — 2 declarations: `frame_round_trip_and_bounds`, `request_ids_are_bounded_tokens`

`core/src/ipc/unix.rs` — 2 declarations: `buffered_response_survives_peer_shutdown`, `peer_shutdown_mid_body_is_transport_closed`

`core/src/platform/mod.rs` — 1 declarations: `uuid_format_is_canonical`

`core/src/rules.rs` — 3 declarations: `running_item_is_reported_but_ineligible`, `unknown_item_is_reported_but_ineligible`, `app_backup_requires_newer_replacement`

`core/src/scan.rs` — 1 declarations: `swapped_directory_is_refused_before_listing`

`core/tests/activity.rs` — 3 declarations: `duplicate_event_is_idempotent_but_changed_duplicate_is_rejected`, `moved_logical_and_observed_delta_stay_separate`, `weekly_and_monthly_windows_are_utc_and_have_explicit_bounds`

`core/tests/apfs_fixture.rs` — 1 declarations: `apfs_fixture_accounting_is_conservative`

`core/tests/apps.rs` — 6 declarations: `ownership_requires_exact_identity_and_excludes_shared_user_data`, `projector_uses_supplied_records_and_deduplicates_overlap_and_hardlinks`, `missing_external_or_portable_coverage_keeps_disappearance_unknown`, `malformed_and_unavailable_updates_are_distinct_and_network_free`, `bounded_history_keys_samples_by_incarnation_and_session`, `byte_totals_report_units_and_saturating_overflow`

`core/tests/cleanup.rs` — 7 declarations: `overlapping_targets_are_deduplicated_to_outer_target`, `protected_descendant_refuses_plan_before_claim`, `claim_is_one_time_and_expiry_is_enforced`, `changed_effect_cannot_reuse_claim`, `interrupted_item_is_indeterminate_and_never_replayed`, `changed_identity_is_recorded_as_item_failure_without_execution`, `undo_only_uses_confirmed_moves_and_reports_restore_conflict`

`core/tests/cli.rs` — 15 declarations: `apply_never_accepts_paths_or_unissued_ids`, `missing_usage_is_unavailable_not_zero`, `scan_is_metadata_only_and_requires_opt_in_for_history`, `corrupt_history_is_reported_without_breaking_json_stdout`, `save_then_history_findings_explain_round_trip`, `corrupt_history_file_appears_in_history_diagnostics`, `storage_cli_journey_is_bounded_opt_in_and_preserves_fixture_bytes`, `option_validation_errors_are_typed_and_exit_two`, `mutation_commands_refuse`, `human_output_is_bounded_with_truncation_notice`, `worker_serves_status_then_exits_after_idle`, `exec_op_round_trip_one_bounded_request_one_response`, `exec_op_oversized_request_is_typed_not_fatal`, `exec_op_rejects_bad_or_nonviable_limits`, `history_outputs_capability_notes_field`

`core/tests/compression.rs` — 6 declarations: `malformed_parameters_are_rejected_before_io`, `unsupported_codec_is_explicit`, `source_safety_failures_are_typed`, `failure_and_cancel_never_complete`, `measured_output_is_read_separately_from_estimate`, `finalize_rejects_unsafe_or_source_identity_output`

`core/tests/descriptor_race.rs` — 7 declarations: `swapped_ancestor_symlink_refuses_descendant_listing`, `renamed_replacement_is_listed_as_itself_not_stale_object`, `final_component_symlink_swap_is_refused`, `bounded_listing_still_reports_truncation`, `listing_survives_parent_rename_after_acceptance`, `junction_ancestor_is_refused`, `bounded_listing_still_reports_truncation`

`core/tests/duplicates.rs` — 4 declarations: `exact_bytes_form_group_and_changed_content_does_not`, `hardlinks_are_skipped_by_identity_and_symlinks_are_refused`, `injected_partial_reader_is_reported_without_group`, `read_budget_and_deadline_truncate_before_unbounded_content_reads`

`core/tests/durable_storage.rs` — 1 declarations: `durable_filesystem_journey_restarts_rejects_replay_holds_namespace_and_rejects_unknown_state`

`core/tests/folder_growth.rs` — 11 declarations: `signed_changes_added_removed_and_deterministic_order_are_reported`, `incomplete_snapshot_refuses_folder_totals`, `remount_and_root_change_use_history_compatibility_guard`, `missing_identity_duplicate_and_out_of_scope_rows_refuse_ambiguity`, `u64_extremes_use_signed_i128_without_underflow`, `each_output_list_is_bounded_and_truncation_is_explicit`, `caller_limit_is_capped_and_reported`, `dot_components_in_roots_or_folder_rows_are_rejected`, `empty_folder_coverage_for_nonempty_report_is_rejected`, `skipped_symlink_note_alone_does_not_invalidate_complete_comparison`, `attributed_direction_wins_when_logical_direction_differs`

`core/tests/history.rs` — 10 declarations: `same_scope_growth_is_signed_and_positive`, `same_scope_shrink_is_negative_without_underflow`, `changed_roots_are_not_comparable`, `reordered_and_duplicated_roots_still_compare`, `changed_volume_set_is_not_comparable`, `empty_volume_identity_is_not_comparable`, `no_observed_volumes_is_not_comparable`, `incomplete_accounting_is_not_comparable`, `differing_schema_versions_are_not_comparable`, `unknown_sharing_does_not_block_comparison`

`core/tests/ipc_unix.rs` — 19 declarations: `round_trip_and_cleanup`, `invalid_limits_are_refused_before_creating_endpoint`, `symlinked_ancestor_cannot_create_endpoint_parent`, `cleanup_uses_original_parent_after_rename`, `oversized_request_is_rejected`, `malformed_connection_does_not_kill_server`, `slowly_arriving_request_cannot_extend_frame_deadline`, `symlinked_parent_is_refused`, `world_writable_parent_is_refused`, `missing_parent_is_created_private`, `live_endpoint_is_in_use`, `unrelated_files_and_stale_sockets_are_not_removed`, `idle_exit`, `shutdown_flag_stops_server`, `socket_is_unlinked_only_when_owned`, `saturated_backlog_is_in_use_not_stale`, `connect_to_saturated_backlog_is_bounded`, `stale_socket_fails_fast`, `oversized_endpoint_path_is_refused`

`core/tests/ipc_windows.rs` — 10 declarations: `round_trip_and_shutdown_flag`, `default_endpoint_is_a_safe_pipe_name`, `oversized_request_is_rejected_both_sides`, `second_server_reports_endpoint_in_use`, `bad_endpoint_names_are_unsafe`, `idle_exit`, `silent_peer_is_dropped_within_bounded_wait`, `tiny_response_limit_is_rejected`, `oversized_handler_reply_becomes_capped_error`, `client_times_out_without_server`

`core/tests/monitor.rs` — 6 declarations: `rate_requires_interval_and_handles_zero_time`, `rate_rejects_time_backwards_counter_reset_and_overflow_shape`, `battery_rejects_invalid_units_and_preserves_valid_reading`, `listener_owner_requires_start_time_and_rejects_pid_reuse`, `history_is_bounded_and_pid_reuse_gets_new_identity`, `process_history_skips_invalid_cpu_or_unknown_memory`

`core/tests/native_filesystem.rs` — 2 declarations: `native_sparse_file_and_hardlink_use_unique_stat_allocation`, `native_symlink_ancestor_blocks_requested_descendant`

`core/tests/platform_metadata.rs` — 7 declarations: `same_inode_text_on_different_volumes_is_not_a_hard_link`, `missing_native_fields_are_reported_with_reasons`, `missing_fields_without_provider_reasons_get_generic_reasons`, `placeholder_is_refused_and_never_enumerated_or_counted`, `hard_link_shares_file_id_and_directory_has_one`, `bounded_enumeration_stops_and_reports_truncation`, `macos_volume_identity_is_uuid_or_flagged_unstable`

`core/tests/presentation.rs` — 12 declarations: `default_options_and_trailing_newline`, `scan_aggregates_folders_and_escapes_paths`, `scan_keeps_reclaim_unknown_and_reports_incomplete_coverage`, `scan_bounds_rows_with_notice`, `status_shows_unavailable_not_zero`, `procs_show_pid_start_time_and_metric_label`, `findings_explain_history_usage_render`, `scan_uses_provider_subtree_folder_totals`, `growth_extremes_render_full_range`, `evidence_sufficiency_is_not_actions_enabled`, `capability_notes_render_bounded_and_escaped`, `worker_response_shows_truncation_and_errors`

`core/tests/process_groups.rs` — 10 declarations: `table_of_group_shapes`, `duplicate_pid_both_incarnations_retained`, `ambiguous_parent_is_not_attached`, `duplicate_parent_pid_remains_ambiguous_when_one_started_later`, `cycle_reports_identities_as_evidence`, `cpu_sums_and_memory_sums_when_complete`, `partial_memory_is_unavailable_not_fabricated`, `single_process_evidence_and_name_never_groups`, `deterministic_regardless_of_input_order`, `deep_chain_is_bounded`

`core/tests/rule_evidence.rs` — 16 declarations: `pack_is_structurally_safe_and_report_only`, `explanation_rules_never_attach_or_become_eligible`, `fully_evidenced_rows_are_the_only_eligible_rows`, `every_declared_requirement_unknown_failed_or_in_use_blocks`, `baseline_protections_apply_to_every_rule`, `unknown_liveness_never_hides_in_use_and_has_a_cause`, `contradictory_evidence_is_never_eligible`, `volume_scope_must_be_confirmed_not_assumed`, `missing_measured_size_is_reported_not_invented`, `report_only_findings_name_every_unknown_signal`, `path_matching_is_segment_exact_and_case_insensitive`, `relative_and_traversing_paths_match_but_never_become_eligible`, `finding_ids_are_stable_and_order_independent`, `duplicate_rows_collapse_and_conflicts_are_ineligible`, `explicit_unknown_defaults_are_never_eligible`, `findings_carry_rule_text_for_the_report`

`core/tests/scan_runtime.rs` — 6 declarations: `per_scan_cache_is_reset_and_results_are_consistent`, `bounded_listing_truncates_after_limit`, `listing_does_not_follow_symlinked_directory`, `special_file_is_unknown_not_zero_allocated`, `removed_between_listing_and_inspection_is_rejected`, `one_scan_reports_consistent_identities_on_one_volume`

`core/tests/scan_safety.rs` — 19 declarations: `denied_ancestor_blocks_root_and_is_not_reported_as_symlink`, `placeholder_ancestor_blocks_root`, `cross_volume_descendant_is_rejected_explicitly`, `depth_limit_is_explicit`, `entry_limit_is_explicit_and_deterministic`, `entry_limit_applies_across_roots`, `placeholder_file_rejected_by_default`, `placeholder_directory_is_never_enumerated_even_when_allowed`, `overlapping_roots_count_once`, `duplicate_children_are_visited_once`, `hard_links_dedupe_within_volume_but_not_across_volumes`, `cross_volume_hard_link_alias_is_not_counted`, `missing_metadata_is_incomplete_even_if_provider_claims_complete`, `inspect_failure_is_recorded_and_incomplete`, `revisited_directory_identity_is_explicit`, `scanning_never_marks_anything_cleanup_eligible`, `parent_swapped_after_listing_blocks_child_inspection`, `directory_swapped_before_listing_is_not_enumerated`, `unidentified_files_never_claim_unique_allocation`

`core/tests/storage.rs` — 6 declarations: `hardlinks_are_attributed_once_in_lexical_order`, `entry_limit_is_explicitly_incomplete`, `placeholder_root_is_rejected_without_enumeration`, `links_are_reported_and_overlapping_roots_are_deduplicated`, `missing_allocation_metadata_has_no_guessed_upper_bound`, `placeholder_ancestor_stops_before_descendant_inspection`

`core/tests/storage_browser.rs` — 6 declarations: `search_includes_hidden_files_filters_extension_and_paginates`, `malformed_query_and_limits_are_rejected`, `overlap_duplicates_collapse_without_merging_distinct_hardlinks`, `incomplete_and_placeholder_state_is_preserved_without_hydration`, `drilldown_is_direct_child_only_and_rejects_outside_path`, `largest_folders_are_deterministic_and_report_incompleteness`

`core/tests/store_safety.rs` — 25 declarations: `round_trip_and_deterministic_order`, `rejects_path_like_and_malformed_ids`, `save_never_overwrites_existing_file`, `refuses_state_path_that_is_a_file`, `malformed_and_future_snapshots_are_skipped_with_reasons`, `oversized_snapshot_is_skipped_not_parsed`, `history_rejects_too_many_unrelated_directory_entries`, `interrupted_temp_files_are_reported_never_parsed`, `size_cap_boundary_exact_vs_plus_one`, `snapshot_count_cap_boundary`, `temp_files_count_toward_candidate_cap`, `aggregate_read_budget_stops_and_records_skips`, `case_variant_names_are_duplicate_id_skips`, `concurrent_distinct_ids_all_publish`, `dangling_symlink_destination_is_not_written_through`, `windows_capability_note_is_reported`, `missing_directory_is_empty_history`, `refuses_symlinked_state_directory_and_snapshot_links`, `permissions_new_dir_private_existing_dir_preserved`, `windows_directory_junction_is_refused`, `windows_rejects_reserved_and_stream_ids`, `windows_real_directory_replacement_keeps_operations_inside_owned_dirs`, `parent_replacement_never_redirects_pinned_writes_or_reads`, `refuses_group_or_other_writable_existing_directory`, `concurrent_save_publishes_exactly_one_snapshot`

`core/tests/windows_filesystem.rs` — 6 declarations: `listing_is_bounded_and_reports_truncation`, `listing_refuses_junction_or_symlink`, `listing_refuses_files`, `listing_identity_matches_inspection`, `volume_usage_for_temp_volume_is_consistent`, `volume_usage_rejects_unmatched_identity`

`core/tests/worker_protocol.rs` — 20 declarations: `oversized_request_is_rejected`, `malformed_requests_have_typed_errors_and_unreadable_ids_are_none`, `unsupported_version_is_rejected`, `reused_request_id_conflicts_even_after_failure`, `mutating_and_settings_operations_are_unsupported`, `invalid_arguments_are_rejected`, `scan_on_temp_dir_with_small_limits`, `scan_entry_limit_is_reported_not_hidden`, `oversized_scan_response_is_truncated_and_marked_incomplete`, `reused_request_id_conflicts_after_version_failure`, `tiny_but_viable_error_limit_uses_minimal_error`, `maximal_request_id_retains_correlation_at_error_budget_floor`, `impossible_response_limit_closes_instead_of_overrunning`, `bounded_worker_executes_ops_in_killable_child`, `blocked_stdin_child_is_killed_at_deadline`, `flooding_stdout_is_capped_and_killed`, `exec_op_spawn_failure_is_typed`, `status_and_processes_use_core_data`, `events_follow_request_lifecycle`, `limits_expose_idle_and_transport_bounds`

## Windows retained declaration inventory

`windows/src/diag.rs` — 4 declarations: `plain_values_are_unquoted`, `spaces_quotes_and_newlines_are_escaped`, `failure_line_is_single_structured_line`, `latch_reports_edges_only`

`windows/src/lifecycle.rs` — 11 declarations: `pill_is_right_edge_vertically_centred`, `pill_on_negative_origin_monitor`, `unchanged_set_plans_nothing`, `disconnect_destroys_only_missing_monitor`, `reconnect_creates_panel`, `all_monitors_gone_then_back`, `rearranged_or_resized_monitor_moves_panel`, `destroys_precede_creates_and_duplicate_ids_collapse`, `cadence_slows_only_when_every_panel_hidden`, `cpu_fraction_cases`, `gate_coalesces_reentrant_requests`

`windows/src/main.rs` — 5 declarations: `exact_monitor_bounds_are_fullscreen`, `inset_window_is_not_fullscreen`, `captioned_window_is_not_borderless`, `percent_text_formats_known_and_unknown`, `production_sampling_cadence_matches_helper`

`windows/src/runtime.rs` — 20 declarations: `anchor_math_on_primary`, `anchor_math_on_negative_origin_monitor`, `anchor_math_never_leaves_origin_on_tiny_monitor`, `startup_creates_with_stored_anchor`, `unchanged_placement_plans_nothing`, `anchor_change_on_same_bounds_moves`, `bounds_and_anchor_change_yields_single_move`, `disabled_or_unplugged_monitor_is_destroyed_and_reconnect_recreates`, `failed_move_is_replanned_until_applied`, `retry_stays_pending_until_success_and_reports_edges`, `retry_target_can_change_between_attempts`, `hidden_policy`, `cadence_uses_setting_only_while_a_panel_is_visible`, `clamp_keeps_pill_inside_negative_origin_monitor`, `clamp_on_tiny_negative_origin_monitor_hugs_origin`, `mutex_name_is_sid_scoped`, `restrictive_sddl_grants_only_user_and_system`, `live_current_user_security_yields_user_sid`, `live_second_acquire_reports_already_running`, `live_restricted_dir_passes_and_world_dacl_is_refused`

`windows/src/settings.rs` — 22 declarations: `parses_full_schema`, `defaults_when_optional_fields_missing`, `unknown_fields_are_ignored_at_every_level`, `cadence_mutators_are_clamped_but_file_values_are_strict`, `unknown_version_and_missing_version`, `malformed_inputs_are_rejected`, `depth_is_bounded`, `oversized_input_is_rejected`, `too_many_monitors_rejected`, `string_escapes_and_surrogates_decode`, `bom_is_tolerated`, `encode_roundtrips_and_is_exact`, `encode_escapes_control_characters_and_roundtrips`, `encode_clamps_cadence`, `encode_refuses_oversize_instead_of_truncating`, `mutators_report_change_and_enforce_bounds`, `anchor_names_roundtrip`, `reparse_attribute_detection_and_paths`, `load_missing_is_writable_default`, `live_save_then_load_roundtrips_in_restricted_tree`, `live_malformed_file_is_never_truncated`, `live_broad_file_acl_is_refused_and_preserved`

`windows/src/visibility_cases.rs` — 34 declarations: `secondary_right_monitor_exact_bounds`, `primary_sized_window_on_secondary_is_not_fullscreen`, `fullscreen_on_primary_does_not_cover_secondary`, `negative_origin_monitor_exact_bounds`, `negative_monitor_inset_by_one_is_not_fullscreen`, `monitor_above_primary_negative_top`, `oversized_window_with_negative_overhang_is_fullscreen`, `half_screen_snap_is_not_fullscreen`, `taskbar_height_shortfall_is_not_fullscreen`, `partial_window_still_overlaps_for_enumeration`, `degenerate_zero_size_rect_is_not_fullscreen`, `spanning_borderless_window_covers_both_monitors`, `spanning_window_covers_only_monitors_fully_inside`, `spanning_left_and_primary_negative_coordinates`, `spanning_window_taller_monitor_not_covered`, `captioned_maximized_window_is_geometry_fullscreen_but_not_borderless`, `captioned_maximized_on_negative_secondary_is_not_hiding`, `caption_alone_or_thickframe_alone_blocks_hiding`, `lone_border_or_dlgframe_bit_is_not_a_caption`, `resize_frame_blocks_even_without_caption`, `classify_outside_partial_and_covers`, `classify_captioned_maximized_is_partial_not_covers`, `classify_minimized_offscreen_window_is_outside`, `classify_spanning_window_per_monitor`, `shell_class_names_are_excluded`, `tool_window_ex_style_is_excluded`, `popup_borderless_exact_monitor_hides`, `borderless_fullscreen_on_secondary_and_negative_monitors`, `borderless_but_smaller_than_monitor_does_not_hide`, `zero_style_is_borderless`, `unrelated_style_bits_do_not_affect_borderless`, `monitor_id_stops_at_nul`, `monitor_id_empty_when_unset`, `monitor_id_full_length_without_nul`

## Mac retained declaration inventory

`mac/Tests/CockpitMacPrototypeCoreTests/AccessibilityGeometryTests.swift` — 5 declarations: `testElementRejectsNonAXUIElement`, `testBoolDecoding`, `testPointDecoding`, `testSizeDecoding`, `testCopyAttributeOnForeignTypeFails`

`mac/Tests/CockpitMacPrototypeCoreTests/DashboardHostTests.swift` — 15 declarations: `testScanRequestArgvIsExplicitRootNoShell`, `testRunnerCapturesBoundedStdout`, `testRunnerTruncatesOverLimitStdout`, `testRunnerKillsChildBeforeDeadline`, `testRunnerNonZeroExit`, `testRunnerCancelTerminatesChild`, `testScanObjectAcceptsSnapshotEnvelope`, `testScanObjectRejectsTruncated`, `testScanObjectRejectsEmptyAndMalformed`, `testImportJavaScriptCallsAllowlistedEntry`, `testImportJavaScriptEscapesBreakoutSequences`, `testNavigationAllowsOnlyBundledSubtree`, `testScriptMessageAllowlistIsClosed`, `testCoordinatorMissingHelper`, `testCoordinatorBuildsImportScript`

`mac/Tests/CockpitMacPrototypeCoreTests/MediaCompressionTests.swift` — 5 declarations: `testInvalidParametersAreRejectedWithoutEncoding`, `testCancellationStopsBeforePublishAndLeavesOriginal`, `testImageEncodePublishesUniqueOutputAndPreservesOriginal`, `testTargetSizeFailureRemovesOnlyJobOutputAndPreservesOriginal`, `testSymlinkedSourceIsRejected`

`mac/Tests/CockpitMacPrototypeCoreTests/NativeCleanupJourneyTests.swift` — 1 declarations: `testReviewApplyRestartUndoConflictAndAncestorProtection`

`mac/Tests/CockpitMacPrototypeCoreTests/NativeStorageServicesTests.swift` — 1 declarations: `testNativeCompressionJourneyPersistsActivityAndProtectsExistingOutput`

`mac/Tests/CockpitMacPrototypeCoreTests/PillLifecycleTests.swift` — 13 declarations: `testCPUNoPreviousIsUnavailable`, `testCPUDelta`, `testCPUZeroTotalIsUnavailable`, `testCPUWraparound`, `testDisplayDiff`, `testDisplayDiffEmpty`, `testRedrawGate`, `testUnavailableRendersDashes`, `testArcQuantizationMatchesRedrawLabels`, `testPlacementStaysInsideVisibleFrame`, `testFailureTrackerReportsTransitionsOnly`, `testStructuredEventLine`, `testTimestampFormat`

`mac/Tests/CockpitMacPrototypeCoreTests/PillRuntimeTests.swift` — 8 declarations: `testMonitorCapIsAnExplicitRefusal`, `testInvalidMonitorKeysAreRefused`, `testSettingDefaultsOnAbsentKeyIsANoOp`, `testDoubleStartTakesNoSecondOwnerRole`, `testStartAfterShutdownIsRefused`, `testShutdownIsIdempotentAndOrdered`, `testLockReacquireAfterShutdownSucceeds`, `testRefusedDirectoryFailsStart`

`mac/Tests/CockpitMacPrototypeCoreTests/PillSettingsStoreTests.swift` — 16 declarations: `testPermissiveDirectoryIsRefusedAndUntouched`, `testForeignOwnedDirectoryIsRefused`, `testPermissiveFileIsProtectedOnLoadAndRefusedOnSave`, `testGroupWritableDirectoryIsRefused`, `testMalformedFileIsPreservedByteForByte`, `testSymlinkedDirectoryAndFileAreRefused`, `testSymlinkedAncestorIsRefusedBeforeCreation`, `testFreshSaveUsesOwnerOnlyModes`, `testAcquireCreatesOwnerOnlyLockFile`, `testPermissiveDirectoryRefusesAcquire`, `testForeignOwnedDirectoryRefusesAcquire`, `testPermissivePreexistingLockFileIsRefusedAndPreserved`, `testSymlinkedLockIsRefusedAndPreserved`, `testSymlinkedLockAncestorIsRefused`, `testNonRegularLockIsRefusedAndPreserved`, `testMissingDirectoryFailsWithoutCreatingAnything`

`mac/Tests/CockpitMacPrototypeCoreTests/PillSettingsTests.swift` — 26 declarations: `testFullDocument`, `testDefaultsForAbsentFields`, `testMalformedFieldsRejectWholePayload`, `testDecodeFailureYieldsNoSettings`, `testFailures`, `testMonitorCountCapIsStrict`, `testMonitorKeyUTF8ByteLimit`, `testSetMonitorMutatorEnforcesBounds`, `testEncodeRefusesUnserializableMonitors`, `testEncodeRoundTripAndDeterminism`, `testEncodeClampsCadenceAndRejectsOversize`, `testDecisionTable`, `testShutdownOrder`, `testVisibilityAndSchedule`, `testAnchoredPlacement`, `testMissingDirectoryLoadsDefaults`, `testSaveCreatesPrivateDirectoryAndFileAndRoundTrips`, `testExistingDirectoryModeIsPreserved`, `testUnreadableFilesAreProtectedAndNeverOverwritten`, `testSymlinkedFileAndDirectoryAreRefused`, `testSecondAcquireIsRejectedAndReleaseKeepsFile`, `testSymlinkedLockIsRefused`, `testSecondRuntimeReportsAlreadyRunning`, `testShutdownOrderAndNoWriteWhenUnchanged`, `testChangedSettingsPersistBeforeLockRelease`, `testCorruptFileUsesDefaultsAndIsPreserved`

`mac/Tests/CockpitMacPrototypeCoreTests/VisibilityEdgeCasesTests.swift` — 8 declarations: `testNegativeCoordinatesCover`, `testMixedMonitorSizes`, `testPartialCoverageIsNotCovered`, `testDegenerateRectsNeverCover`, `testFocusedWindowOnAnotherMonitorFallsBackToGeometry`, `testExplicitAXFalseWinsOnSameMonitor`, `testMissingAXObservations`, `testDeniedAccessibilityNeverHides`

`mac/Tests/CockpitMacPrototypeCoreTests/VisibilityGeometryTests.swift` — 6 declarations: `testCoverageRequiresCompleteMonitor`, `testFallbackRequiresUntitledBorderlessCandidate`, `testAccessibilityDenialKeepsPillVisible`, `testExplicitAXFalseWinsOverGeometry`, `testMissingAXUsesGeometryFallback`, `testOtherMonitorAXFalseDoesNotOverrideFullscreenOccupancy`

## JavaScript retained declaration inventory

`dashboard/app.test.mjs` — 1 executable helper-test script (assertions are listed in file; script has no `node:test` declaration).

`scripts/probes/footprint-report.test.mjs` — 25 declarations: `parseArgs refuses without a pid and bounds duration`, `parseArgs defaults to a 10 sample 1s smoke window`, `ps parsing`, `powershell parsing: ISO (7.x) and legacy 5.1 shapes`, `powershell: missing cpu time is explicit, missing start time is unavailable`, `malformed output`, `empty output means vanished`, `cpuPercent from deltas`, `stats: min/median/p95/max with empty case`, `aggregation over a short run`, `PID reuse stops the run and is not counted as a measurement`, `vanished process terminates with process_exited`, `malformed output tolerated twice, third consecutive stops; success resets`, `failed samples retain parser reason for unavailable metrics`, `windows tracker: null cpu time is recorded as unavailable`, `probe: ok sample from another PID reports Mach ports unavailable with a reason`, `probe: self sample carries the Mach port count`, `probe: reused, vanished, unavailable and malformed documents`, `probe samples flow into a report with footprint and no private bytes`, `powershell: private bytes, handles; GDI unavailable with a reason`, `powershell: start time changed or missing after the sample`, `probe start time differing between samples stops the run as pid_reused`, `parseArgs soak rules`, `stream stat: exact while small, bounded and sane when large`, `soak summary keeps memory bounded and tracker retains no records`

`scripts/upstream-report.test.mjs` — 5 declarations: `fixture: ${name}`, `parseLsRemote rejects conflicting duplicate lines and ignores other refs`, `validateDonor flags non-https repositories and option-like refs`, `classifyPollError and lock read failure are structured and bounded`, `upstream-check keeps git bounded and shell-free`

The inventory intentionally exposes open behavior. No candidate declaration is deleted until matching E2E evidence exists; no tests are added solely to restore counts.
