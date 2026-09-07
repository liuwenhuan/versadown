use thiserror::Error;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Provider { Quark, PikPak }

#[derive(Clone, Debug)]
pub struct CloudItem { pub id: String, pub name: String, pub is_folder: bool, pub size: Option<String> }

#[derive(Clone, Debug)]
pub struct CloudDirectory { pub path: String, pub items: Vec<CloudItem> }

#[derive(Debug, Error)]
pub enum ConnectorError { #[error("user authorization is required")] AuthorizationRequired, #[error("operation is unavailable for this connector")] Unavailable }

pub trait CloudConnector {
    fn provider(&self) -> Provider;
    fn list_directory(&self, path: &str) -> Result<CloudDirectory, ConnectorError>;
    fn create_directory(&self, parent: &str, name: &str) -> Result<CloudItem, ConnectorError>;
}

pub struct MockConnector { provider: Provider, connected: bool }

impl MockConnector {
    pub fn connected(provider: Provider) -> Self { Self { provider, connected: true } }
    pub fn disconnected(provider: Provider) -> Self { Self { provider, connected: false } }
}

impl CloudConnector for MockConnector {
    fn provider(&self) -> Provider { self.provider }
    fn list_directory(&self, path: &str) -> Result<CloudDirectory, ConnectorError> {
        if !self.connected { return Err(ConnectorError::AuthorizationRequired); }
        Ok(CloudDirectory { path: path.to_string(), items: vec![CloudItem { id: "samples".into(), name: "示例文件夹".into(), is_folder: true, size: None }, CloudItem { id: "guide".into(), name: "使用说明.pdf".into(), is_folder: false, size: Some("1.2 MB".into()) }] })
    }
    fn create_directory(&self, _parent: &str, name: &str) -> Result<CloudItem, ConnectorError> {
        if !self.connected { return Err(ConnectorError::AuthorizationRequired); }
        Ok(CloudItem { id: format!("new-{name}"), name: name.to_string(), is_folder: true, size: None })
    }
}
