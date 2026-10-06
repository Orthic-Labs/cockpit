use cockpit_core::compression::*;
use std::path::PathBuf;

fn request(format: CompressionFormat) -> CompressionRequest {
    CompressionRequest::new(
        PathBuf::from("/fixture/input/source.jpg"),
        PathBuf::from("/fixture/input"),
        PathBuf::from("/fixture/output"),
        format,
        75,
        ResizePolicy::Keep,
        Some(400),
    )
}

fn source() -> SourceEvidence {
    SourceEvidence {
        media_kind: MediaKind::Image,
        logical_bytes: Some(1_000),
        identity: Some(FileIdentity {
            volume_id: "fixture-volume".into(),
            file_id: "source-1".into(),
        }),
        is_symlink: false,
        is_placeholder: false,
        metadata_complete: true,
        ancestors_safe: true,
    }
}

#[test]
fn malformed_parameters_are_rejected_before_io() {
    let mut invalid = request(CompressionFormat::Jpeg);
    invalid.resize = ResizePolicy::ScalePercent(0);
    assert_eq!(
        CompressionPlan::validate(invalid, source()),
        Err(CompressionPlanError::InvalidResize)
    );

    let mut invalid = request(CompressionFormat::Jpeg);
    invalid.target_size_bytes = Some(1_000);
    assert_eq!(
        CompressionPlan::validate(invalid, source()),
        Err(CompressionPlanError::TargetSizeNotSmaller)
    );
}

#[test]
fn unsupported_codec_is_explicit() {
    let mut unsupported = request(CompressionFormat::Webp);
    unsupported.source = PathBuf::from("/fixture/input/source.webp");
    assert_eq!(
        CompressionPlan::validate(unsupported, source()),
        Err(CompressionPlanError::UnsupportedCodec)
    );
}

#[test]
fn source_safety_failures_are_typed() {
    let mut evidence = source();
    evidence.is_symlink = true;
    assert_eq!(
        CompressionPlan::validate(request(CompressionFormat::Jpeg), evidence),
        Err(CompressionPlanError::SourceSymlink)
    );

    let mut evidence = source();
    evidence.is_placeholder = true;
    assert_eq!(
        CompressionPlan::validate(request(CompressionFormat::Jpeg), evidence),
        Err(CompressionPlanError::SourcePlaceholder)
    );

    let mut evidence = source();
    evidence.identity = None;
    assert_eq!(
        CompressionPlan::validate(request(CompressionFormat::Jpeg), evidence),
        Err(CompressionPlanError::UnknownIdentity)
    );
}

#[test]
fn failure_and_cancel_never_complete() {
    let mut plan = CompressionPlan::validate(request(CompressionFormat::Jpeg), source()).unwrap();
    plan.begin_encoding().unwrap();
    plan.begin_publishing().unwrap();
    let failure = plan
        .finalize(OutputEvidence {
            path: PathBuf::from("/fixture/output/result.jpg"),
            logical_bytes: Some(450),
            identity: Some(FileIdentity {
                volume_id: "fixture-volume".into(),
                file_id: "output-1".into(),
            }),
            is_regular_file: true,
            is_symlink: false,
            existed_before_job: true,
            ancestors_safe: true,
        })
        .unwrap_err();
    assert_eq!(failure.reason, CompressionFailureReason::OutputExists);
    assert_eq!(plan.lifecycle, CompressionLifecycle::Failed);

    let mut cancelled =
        CompressionPlan::validate(request(CompressionFormat::Jpeg), source()).unwrap();
    cancelled.begin_encoding().unwrap();
    assert_eq!(
        cancelled.cancel().reason,
        CompressionFailureReason::Cancelled
    );
    assert_eq!(cancelled.lifecycle, CompressionLifecycle::Cancelled);
}

#[test]
fn measured_output_is_read_separately_from_estimate() {
    let mut plan = CompressionPlan::validate(request(CompressionFormat::Jpeg), source()).unwrap();
    plan.begin_encoding().unwrap();
    plan.begin_publishing().unwrap();
    let result = plan
        .finalize(OutputEvidence {
            path: PathBuf::from("/fixture/output/result.jpg"),
            logical_bytes: Some(350),
            identity: Some(FileIdentity {
                volume_id: "fixture-volume".into(),
                file_id: "output-2".into(),
            }),
            is_regular_file: true,
            is_symlink: false,
            existed_before_job: false,
            ancestors_safe: true,
        })
        .unwrap();
    assert_eq!(result.source_bytes, 1_000);
    assert_eq!(result.output_bytes, 350);
    assert_eq!(result.measured_saved_bytes, 650);
    assert!(result.estimate.is_none());
}

#[test]
fn finalize_rejects_unsafe_or_source_identity_output() {
    let mut plan = CompressionPlan::validate(request(CompressionFormat::Jpeg), source()).unwrap();
    plan.begin_encoding().unwrap();
    plan.begin_publishing().unwrap();
    let failure = plan
        .finalize(OutputEvidence {
            path: PathBuf::from("/fixture/output/../source.jpg"),
            logical_bytes: Some(300),
            identity: Some(FileIdentity {
                volume_id: "fixture-volume".into(),
                file_id: "source-1".into(),
            }),
            is_regular_file: true,
            is_symlink: false,
            existed_before_job: false,
            ancestors_safe: true,
        })
        .unwrap_err();
    assert_eq!(failure.reason, CompressionFailureReason::OutputInvalid);
}
