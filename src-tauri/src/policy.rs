use std::fmt;

use thiserror::Error;

#[derive(Clone, Eq, PartialEq)]
pub enum ProcessAction { ExtractArchive, MergeParts, Rename, VerifyChecksum, Execute }

#[derive(Clone)]
pub struct ProcessStep { pub action: ProcessAction, pub input: String, pub output: Option<String>, pub password_reference: Option<String> }

impl ProcessStep {
    pub fn execute(input: impl Into<String>) -> Self { Self { action: ProcessAction::Execute, input: input.into(), output: None, password_reference: None } }
    pub fn extract(input: impl Into<String>, password_reference: Option<String>) -> Self { Self { action: ProcessAction::ExtractArchive, input: input.into(), output: None, password_reference } }
}

impl fmt::Debug for ProcessStep {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_struct("ProcessStep").field("action", &"redacted-safe-action").field("input", &self.input).field("has_password_reference", &self.password_reference.is_some()).finish()
    }
}

#[derive(Debug, Error)]
pub enum PolicyError { #[error("processing action is not allowed")] Rejected, #[error("path traversal is not allowed")] UnsafePath }

pub fn validate_processing(step: &ProcessStep) -> Result<(), PolicyError> {
    if step.input.contains("..") || step.output.as_deref().is_some_and(|output| output.contains("..")) { return Err(PolicyError::UnsafePath); }
    match step.action { ProcessAction::ExtractArchive | ProcessAction::MergeParts | ProcessAction::Rename | ProcessAction::VerifyChecksum => Ok(()), ProcessAction::Execute => Err(PolicyError::Rejected) }
}
