use crate::policy::ProcessStep;

#[derive(Clone, Debug)]
pub struct Resource { pub name: String, pub is_archive: bool }

impl Resource {
    pub fn archive(name: impl Into<String>) -> Self { Self { name: name.into(), is_archive: true } }
    pub fn file(name: impl Into<String>) -> Self { Self { name: name.into(), is_archive: false } }
}

pub enum ProcessingHint { Password(String) }

impl ProcessingHint { pub fn password(value: impl Into<String>) -> Self { Self::Password(value.into()) } }

pub fn plan_processing(resources: &[Resource], hints: &[ProcessingHint]) -> Vec<ProcessStep> {
    let has_password = hints.iter().any(|hint| matches!(hint, ProcessingHint::Password(value) if !value.is_empty()));
    resources.iter().filter(|resource| resource.is_archive).map(|resource| ProcessStep::extract(&resource.name, has_password.then(|| "discovered-password".to_string()))).collect()
}
