//! Portable contracts for bounded media compression.
//!
//! This module deliberately does not encode media or mutate paths.  A native
//! adapter supplies source/output evidence, performs encoding in its own
//! temporary file, then calls `CompressionPlan::finalize` only after it has
//! revalidated the source and inspected the completed output.

use serde::{Deserialize, Serialize};
use std::path::{Component, Path, PathBuf};

pub const COMPRESSION_SCHEMA_VERSION: u32 = 1;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MediaKind {
    Image,
    Video,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CompressionFormat {
    Jpeg,
    Heic,
    Png,
    Mp4,
    Mov,
    /// Reserved for a future native adapter; never accepted by current Mac
    /// implementation.
    Webp,
    /// Reserved for a future native adapter; never accepted by current Mac
    /// implementation.
    Mkv,
}

impl CompressionFormat {
    pub const fn media_kind(self) -> MediaKind {
        match self {
            Self::Jpeg | Self::Heic | Self::Png => MediaKind::Image,
            Self::Mp4 | Self::Mov | Self::Mkv => MediaKind::Video,
            Self::Webp => MediaKind::Image,
        }
    }

    pub const fn extension(self) -> &'static str {
        match self {
            Self::Jpeg => "jpg",
            Self::Heic => "heic",
            Self::Png => "png",
            Self::Mp4 => "mp4",
            Self::Mov => "mov",
            Self::Webp => "webp",
            Self::Mkv => "mkv",
        }
    }

    /// Portable contract for formats currently implemented by the Mac
    /// adapter.  This list is intentionally narrow; adding a codec requires
    /// native capability checks and a new qualification pass.
    pub const fn supported_on_mac(self) -> bool {
        matches!(
            self,
            Self::Jpeg | Self::Heic | Self::Png | Self::Mp4 | Self::Mov
        )
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ResizePolicy {
    Keep,
    MaxDimension(u32),
    ScalePercent(u16),
}

impl ResizePolicy {
    fn validate(self) -> Result<(), CompressionPlanError> {
        match self {
            Self::Keep => Ok(()),
            Self::MaxDimension(value) if value > 0 => Ok(()),
            Self::MaxDimension(_) => Err(CompressionPlanError::InvalidResize),
            Self::ScalePercent(value) if (1..=100).contains(&value) => Ok(()),
            Self::ScalePercent(_) => Err(CompressionPlanError::InvalidResize),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CompressionRequest {
    pub schema_version: u32,
    pub source: PathBuf,
    pub input_folder: PathBuf,
    pub output_folder: PathBuf,
    pub format: CompressionFormat,
    /// Lossy quality in the closed interval 0..=100.  PNG adapters may ignore
    /// this value, but still validate it so contracts remain deterministic.
    pub quality_percent: u8,
    pub resize: ResizePolicy,
    pub target_size_bytes: Option<u64>,
}

impl CompressionRequest {
    pub fn new(
        source: PathBuf,
        input_folder: PathBuf,
        output_folder: PathBuf,
        format: CompressionFormat,
        quality_percent: u8,
        resize: ResizePolicy,
        target_size_bytes: Option<u64>,
    ) -> Self {
        Self {
            schema_version: COMPRESSION_SCHEMA_VERSION,
            source,
            input_folder,
            output_folder,
            format,
            quality_percent,
            resize,
            target_size_bytes,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct FileIdentity {
    pub volume_id: String,
    pub file_id: String,
}

impl FileIdentity {
    pub fn is_known(&self) -> bool {
        !self.volume_id.is_empty() && !self.file_id.is_empty()
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SourceEvidence {
    pub media_kind: MediaKind,
    pub logical_bytes: Option<u64>,
    pub identity: Option<FileIdentity>,
    pub is_symlink: bool,
    pub is_placeholder: bool,
    pub metadata_complete: bool,
    /// False means an ancestor is a symlink, reparse point, or otherwise
    /// unsafe to use.  Unknown ancestry must be reported as false.
    pub ancestors_safe: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct OutputEvidence {
    pub path: PathBuf,
    pub logical_bytes: Option<u64>,
    pub identity: Option<FileIdentity>,
    pub is_regular_file: bool,
    pub is_symlink: bool,
    pub existed_before_job: bool,
    pub ancestors_safe: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CompressionEstimate {
    pub source_bytes: u64,
    pub estimated_output_bytes: u64,
    pub estimated_saved_bytes: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CompressionResult {
    pub source: PathBuf,
    pub output: PathBuf,
    pub source_bytes: u64,
    pub output_bytes: u64,
    /// Estimate is retained separately from measured output.  It must never
    /// be presented as observed savings.
    pub estimate: Option<CompressionEstimate>,
    pub measured_saved_bytes: u64,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CompressionLifecycle {
    Planned,
    Encoding,
    Publishing,
    Completed,
    Cancelled,
    Failed,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CompressionFailureReason {
    InvalidRequest,
    SourceChanged,
    OutputExists,
    OutputInvalid,
    TargetSizeNotMet,
    UnsupportedCodec,
    Cancelled,
    EncodeFailed,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CompressionFailure {
    pub lifecycle: CompressionLifecycle,
    pub reason: CompressionFailureReason,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum CompressionCompletion {
    Completed(CompressionResult),
    Failed(CompressionFailure),
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum CompressionPlanError {
    InvalidSchema(u32),
    RelativePath,
    UnsafePath,
    SourceOutsideInputFolder,
    SameInputAndOutputFolder,
    InvalidQuality,
    InvalidResize,
    InvalidTargetSize,
    TargetSizeNotSmaller,
    SourceSymlink,
    SourcePlaceholder,
    UnknownIdentity,
    IncompleteMetadata,
    UnknownSourceSize,
    MediaFormatMismatch,
    UnsupportedCodec,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CompressionPlan {
    pub request: CompressionRequest,
    pub source_bytes: u64,
    pub source_identity: FileIdentity,
    pub estimate: Option<CompressionEstimate>,
    pub lifecycle: CompressionLifecycle,
}

impl CompressionPlan {
    /// Validate all portable preconditions before an adapter opens source or
    /// output bytes.  Filesystem metadata is supplied as evidence so this
    /// method remains deterministic and testable on every host.
    pub fn validate(
        request: CompressionRequest,
        evidence: SourceEvidence,
    ) -> Result<Self, CompressionPlanError> {
        if request.schema_version != COMPRESSION_SCHEMA_VERSION {
            return Err(CompressionPlanError::InvalidSchema(request.schema_version));
        }
        if !absolute_clean(&request.source)
            || !absolute_clean(&request.input_folder)
            || !absolute_clean(&request.output_folder)
        {
            return Err(CompressionPlanError::RelativePath);
        }
        if !request.source.starts_with(&request.input_folder) {
            return Err(CompressionPlanError::SourceOutsideInputFolder);
        }
        if request.input_folder == request.output_folder {
            return Err(CompressionPlanError::SameInputAndOutputFolder);
        }
        if request.quality_percent > 100 {
            return Err(CompressionPlanError::InvalidQuality);
        }
        request.resize.validate()?;
        if let Some(target) = request.target_size_bytes {
            if target == 0 {
                return Err(CompressionPlanError::InvalidTargetSize);
            }
        }
        if evidence.is_symlink {
            return Err(CompressionPlanError::SourceSymlink);
        }
        if evidence.is_placeholder {
            return Err(CompressionPlanError::SourcePlaceholder);
        }
        if !evidence.ancestors_safe {
            return Err(CompressionPlanError::UnsafePath);
        }
        if !evidence.metadata_complete {
            return Err(CompressionPlanError::IncompleteMetadata);
        }
        let identity = evidence
            .identity
            .filter(FileIdentity::is_known)
            .ok_or(CompressionPlanError::UnknownIdentity)?;
        if evidence.media_kind != request.format.media_kind() {
            return Err(CompressionPlanError::MediaFormatMismatch);
        }
        if !request.format.supported_on_mac() {
            return Err(CompressionPlanError::UnsupportedCodec);
        }
        let source_bytes = evidence
            .logical_bytes
            .ok_or(CompressionPlanError::UnknownSourceSize)?;
        if let Some(target) = request.target_size_bytes {
            if target >= source_bytes {
                return Err(CompressionPlanError::TargetSizeNotSmaller);
            }
        }
        Ok(Self {
            request,
            source_bytes,
            source_identity: identity,
            // No codec probe has run at this layer, so estimate is unavailable.
            estimate: None,
            lifecycle: CompressionLifecycle::Planned,
        })
    }

    pub fn begin_encoding(&mut self) -> Result<(), CompressionFailure> {
        if self.lifecycle != CompressionLifecycle::Planned {
            return Err(self.failure(CompressionFailureReason::InvalidRequest));
        }
        self.lifecycle = CompressionLifecycle::Encoding;
        Ok(())
    }

    pub fn begin_publishing(&mut self) -> Result<(), CompressionFailure> {
        if self.lifecycle != CompressionLifecycle::Encoding {
            return Err(self.failure(CompressionFailureReason::InvalidRequest));
        }
        self.lifecycle = CompressionLifecycle::Publishing;
        Ok(())
    }

    pub fn cancel(&mut self) -> CompressionFailure {
        self.lifecycle = CompressionLifecycle::Cancelled;
        self.failure(CompressionFailureReason::Cancelled)
    }

    /// Finalize only after the adapter has atomically published its own temp
    /// file and re-read output metadata.  Existing targets are always refused.
    pub fn finalize(
        &mut self,
        output: OutputEvidence,
    ) -> Result<CompressionResult, CompressionFailure> {
        if self.lifecycle != CompressionLifecycle::Publishing {
            return Err(self.failure(CompressionFailureReason::InvalidRequest));
        }
        if output.existed_before_job {
            self.lifecycle = CompressionLifecycle::Failed;
            return Err(self.failure(CompressionFailureReason::OutputExists));
        }
        if output.is_symlink
            || !output.is_regular_file
            || !output.ancestors_safe
            || !absolute_clean(&output.path)
            || !output.path.starts_with(&self.request.output_folder)
            || output.path == self.request.source
            || !output
                .identity
                .as_ref()
                .is_some_and(|identity| identity.is_known())
            || output.identity.as_ref() == Some(&self.source_identity)
        {
            self.lifecycle = CompressionLifecycle::Failed;
            return Err(self.failure(CompressionFailureReason::OutputInvalid));
        }
        let output_bytes = match output.logical_bytes {
            Some(value) if value > 0 => value,
            _ => {
                self.lifecycle = CompressionLifecycle::Failed;
                return Err(self.failure(CompressionFailureReason::OutputInvalid));
            }
        };
        if let Some(target) = self.request.target_size_bytes {
            if output_bytes > target {
                self.lifecycle = CompressionLifecycle::Failed;
                return Err(self.failure(CompressionFailureReason::TargetSizeNotMet));
            }
        }
        self.lifecycle = CompressionLifecycle::Completed;
        Ok(CompressionResult {
            source: self.request.source.clone(),
            output: output.path,
            source_bytes: self.source_bytes,
            output_bytes,
            estimate: self.estimate.clone(),
            measured_saved_bytes: self.source_bytes.saturating_sub(output_bytes),
        })
    }

    fn failure(&self, reason: CompressionFailureReason) -> CompressionFailure {
        CompressionFailure {
            lifecycle: self.lifecycle,
            reason,
        }
    }
}

fn absolute_clean(path: &Path) -> bool {
    if !path.is_absolute() {
        return false;
    }
    path.components()
        .all(|component| !matches!(component, Component::CurDir | Component::ParentDir))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(format: CompressionFormat) -> CompressionRequest {
        CompressionRequest::new(
            PathBuf::from("/input/photo.jpg"),
            PathBuf::from("/input"),
            PathBuf::from("/output"),
            format,
            80,
            ResizePolicy::Keep,
            Some(500),
        )
    }

    fn evidence(kind: MediaKind) -> SourceEvidence {
        SourceEvidence {
            media_kind: kind,
            logical_bytes: Some(1_000),
            identity: Some(FileIdentity {
                volume_id: "v".into(),
                file_id: "f".into(),
            }),
            is_symlink: false,
            is_placeholder: false,
            metadata_complete: true,
            ancestors_safe: true,
        }
    }

    #[test]
    fn plan_keeps_estimate_separate_from_measured_savings() {
        let mut plan =
            CompressionPlan::validate(request(CompressionFormat::Jpeg), evidence(MediaKind::Image))
                .unwrap();
        plan.begin_encoding().unwrap();
        plan.begin_publishing().unwrap();
        let result = plan
            .finalize(OutputEvidence {
                path: PathBuf::from("/output/photo.jpg"),
                logical_bytes: Some(400),
                identity: Some(FileIdentity {
                    volume_id: "v".into(),
                    file_id: "new".into(),
                }),
                is_regular_file: true,
                is_symlink: false,
                existed_before_job: false,
                ancestors_safe: true,
            })
            .unwrap();
        assert!(result.estimate.is_none());
        assert_eq!(plan.lifecycle, CompressionLifecycle::Completed);
    }

    #[test]
    fn unsafe_and_unknown_sources_are_refused() {
        let mut e = evidence(MediaKind::Image);
        e.ancestors_safe = false;
        assert_eq!(
            CompressionPlan::validate(request(CompressionFormat::Jpeg), e),
            Err(CompressionPlanError::UnsafePath)
        );
        let mut e = evidence(MediaKind::Image);
        e.identity = None;
        assert_eq!(
            CompressionPlan::validate(request(CompressionFormat::Jpeg), e),
            Err(CompressionPlanError::UnknownIdentity)
        );
    }

    #[test]
    fn cancellation_is_terminal_and_does_not_publish() {
        let mut plan =
            CompressionPlan::validate(request(CompressionFormat::Jpeg), evidence(MediaKind::Image))
                .unwrap();
        plan.begin_encoding().unwrap();
        let failure = plan.cancel();
        assert_eq!(failure.reason, CompressionFailureReason::Cancelled);
        assert_eq!(plan.lifecycle, CompressionLifecycle::Cancelled);
        assert!(plan.begin_publishing().is_err());
    }

    #[test]
    fn existing_output_is_refused() {
        let mut plan =
            CompressionPlan::validate(request(CompressionFormat::Jpeg), evidence(MediaKind::Image))
                .unwrap();
        plan.begin_encoding().unwrap();
        plan.begin_publishing().unwrap();
        let failure = plan
            .finalize(OutputEvidence {
                path: PathBuf::from("/output/photo.jpg"),
                logical_bytes: Some(400),
                identity: None,
                is_regular_file: true,
                is_symlink: false,
                existed_before_job: true,
                ancestors_safe: true,
            })
            .unwrap_err();
        assert_eq!(failure.reason, CompressionFailureReason::OutputExists);
        assert_eq!(plan.lifecycle, CompressionLifecycle::Failed);
    }
}
