//! Object-safe capabilities; protocol selection happens once in the native factory.
use super::commands::CloudState;
use super::*;
use std::path::Path;

mod native;
pub use native::NativeDriverFactory;

pub trait DriveOps {
    fn account(&mut self) -> Result<CloudAccount, CloudError> {
        Err(CloudError::ProviderUnsupported)
    }
    fn list_directory(&mut self, _parent: &str) -> Result<CloudDirectory, CloudError> {
        Err(CloudError::ProviderUnsupported)
    }
    fn create_directory(&mut self, _parent: &str, _name: &str) -> Result<CloudFile, CloudError> {
        Err(CloudError::ProviderUnsupported)
    }
    fn rename(&mut self, _file: &str, _name: &str) -> Result<(), CloudError> {
        Err(CloudError::ProviderUnsupported)
    }
    fn move_files(&mut self, _files: &[String], _parent: &str) -> Result<(), CloudError> {
        Err(CloudError::ProviderUnsupported)
    }
    fn copy_files(&mut self, _files: &[String], _parent: &str) -> Result<(), CloudError> {
        Err(CloudError::ProviderUnsupported)
    }
    fn trash(&mut self, _files: &[String]) -> Result<(), CloudError> {
        Err(CloudError::ProviderUnsupported)
    }
    fn trash_transferred_files(&mut self, files: &[String]) -> Result<(), CloudError> {
        self.trash(files)
    }
    fn delete(&mut self, _files: &[String]) -> Result<(), CloudError> {
        Err(CloudError::ProviderUnsupported)
    }
    fn restore(&mut self, _files: &[String]) -> Result<(), CloudError> {
        Err(CloudError::ProviderUnsupported)
    }
}

pub trait DownloadOps {
    fn resolve_download(&mut self, _file: &str) -> Result<ResolvedCloudDownload, CloudError> {
        Err(CloudError::ProviderUnsupported)
    }
}

#[derive(Default)]
pub struct WebAuthorizationInput {
    pub cookies: Vec<String>,
    pub storage: serde_json::Value,
    pub browser_context: Option<serde_json::Value>,
}

#[derive(Clone, Copy, Default)]
pub struct WebAuthorizationRecipe {
    pub token_script: Option<&'static str>,
    pub asynchronous_tokens: bool,
    pub browser_context: bool,
}

/// Only the native login phase can request browser fallback. Validation and
/// persistence errors remain ordinary errors, even when they have the same code.
pub enum NativeLoginOutcome {
    Authenticated(CloudAccount),
    NeedBrowserFallback(CloudError),
}

pub(crate) fn complete_native_password_login<S>(
    login: Result<S, CloudError>,
    validate_and_save: impl FnOnce(S) -> Result<CloudAccount, CloudError>,
) -> Result<NativeLoginOutcome, CloudError> {
    let session = match login {
        Ok(session) => session,
        Err(error)
            if browser_login::can_fallback_to_browser(&error)
                || matches!(error, CloudError::CaptchaRequired) =>
        {
            return Ok(NativeLoginOutcome::NeedBrowserFallback(error));
        }
        Err(error) => return Err(error),
    };
    validate_and_save(session).map(NativeLoginOutcome::Authenticated)
}

/// Protocol inputs only: the command host owns windows, scripts and browser fallback.
pub trait ProviderAuth {
    fn web_authorization_recipe(&self) -> WebAuthorizationRecipe {
        WebAuthorizationRecipe::default()
    }
    fn complete_web(&mut self, _input: &WebAuthorizationInput) -> Result<CloudAccount, CloudError> {
        Err(CloudError::ProviderUnsupported)
    }
    fn login_password(
        &mut self,
        _username: &str,
        _password: &str,
    ) -> Result<NativeLoginOutcome, CloudError> {
        Err(CloudError::WebAuthorizationRequired)
    }
    fn recover_password(
        &mut self,
        _credential: &CloudPasswordCredential,
    ) -> Result<(CloudAccount, Vec<u8>), CloudError> {
        Err(CloudError::WebAuthorizationRequired)
    }
    fn validate_browser_password(
        &mut self,
        _cookies: &[String],
        _context: Option<&serde_json::Value>,
        _report: &mut dyn FnMut(&'static str),
    ) -> Result<(CloudAccount, Vec<u8>), CloudError> {
        Err(CloudError::WebAuthorizationRequired)
    }
    fn web_session_snapshot(
        &mut self,
        _input: &WebAuthorizationInput,
    ) -> Result<Option<Vec<u8>>, CloudError> {
        Ok(None)
    }
}

pub trait UploadHttp: CloudHttpTransport + CloudUploadTransport {}
impl<T: CloudHttpTransport + CloudUploadTransport> UploadHttp for T {}

pub trait UploadOps {
    fn upload(
        &mut self,
        _state: &CloudState,
        _http: &dyn UploadHttp,
        _parent: &str,
        _path: &Path,
    ) -> Result<CloudFile, CloudError> {
        Err(CloudError::ProviderUnsupported)
    }
}

pub trait ProviderBackend: DriveOps + DownloadOps + ProviderAuth + UploadOps {}

pub trait ProviderDriverFactory: Send + Sync {
    fn create<'a>(
        &self,
        provider: CloudProvider,
        store: &'a dyn CredentialStore,
        http: &'a dyn CloudHttpTransport,
    ) -> Box<dyn ProviderBackend + 'a>;
}

pub struct ProviderDriver<'a> {
    capabilities: ProviderCapabilities,
    backend: Box<dyn ProviderBackend + 'a>,
}

impl<'a> ProviderDriver<'a> {
    pub fn new(capabilities: ProviderCapabilities, backend: Box<dyn ProviderBackend + 'a>) -> Self {
        Self {
            capabilities,
            backend,
        }
    }
    pub fn auth(&mut self) -> &mut dyn ProviderAuth {
        &mut *self.backend
    }
    pub fn account(&mut self) -> Result<CloudAccount, CloudError> {
        self.backend.account()
    }
    pub fn list_directory(&mut self, parent: &str) -> Result<CloudDirectory, CloudError> {
        require(self.capabilities.list)?;
        self.backend.list_directory(parent)
    }
    pub fn create_directory(&mut self, parent: &str, name: &str) -> Result<CloudFile, CloudError> {
        require(self.capabilities.create_directory)?;
        self.backend.create_directory(parent, name)
    }
    pub fn rename(&mut self, file: &str, name: &str) -> Result<(), CloudError> {
        require(self.capabilities.rename)?;
        self.backend.rename(file, name)
    }
    pub fn move_files(&mut self, files: &[String], parent: &str) -> Result<(), CloudError> {
        require(self.capabilities.r#move)?;
        self.backend.move_files(files, parent)
    }
    pub fn copy_files(&mut self, files: &[String], parent: &str) -> Result<(), CloudError> {
        require(self.capabilities.copy)?;
        self.backend.copy_files(files, parent)
    }
    pub fn trash(&mut self, files: &[String]) -> Result<(), CloudError> {
        require(self.capabilities.trash)?;
        self.backend.trash(files)
    }
    pub fn trash_transferred_files(&mut self, files: &[String]) -> Result<(), CloudError> {
        require(self.capabilities.trash)?;
        self.backend.trash_transferred_files(files)
    }
    pub fn delete(&mut self, files: &[String]) -> Result<(), CloudError> {
        require(self.capabilities.delete)?;
        self.backend.delete(files)
    }
    pub fn restore(&mut self, files: &[String]) -> Result<(), CloudError> {
        require(self.capabilities.restore)?;
        self.backend.restore(files)
    }
    pub fn resolve_download(&mut self, file: &str) -> Result<ResolvedCloudDownload, CloudError> {
        require(self.capabilities.download)?;
        self.backend.resolve_download(file)
    }
    pub fn upload(
        &mut self,
        state: &CloudState,
        http: &dyn UploadHttp,
        parent: &str,
        path: &Path,
    ) -> Result<CloudFile, CloudError> {
        require(self.capabilities.upload)?;
        self.backend.upload(state, http, parent, path)
    }
}

fn require(enabled: bool) -> Result<(), CloudError> {
    if enabled {
        Ok(())
    } else {
        Err(CloudError::ProviderUnsupported)
    }
}

struct StoreRef<'a>(&'a dyn CredentialStore);
impl CredentialStore for StoreRef<'_> {
    fn save(&self, key: &str, bytes: &[u8]) -> Result<(), CloudError> {
        self.0.save(key, bytes)
    }
    fn load(&self, key: &str) -> Result<Option<Vec<u8>>, CloudError> {
        self.0.load(key)
    }
    fn remove(&self, key: &str) -> Result<(), CloudError> {
        self.0.remove(key)
    }
}

struct HttpRef<'a>(&'a dyn CloudHttpTransport);
impl CloudHttpTransport for HttpRef<'_> {
    fn send(&self, request: CloudHttpRequest) -> Result<CloudHttpResponse, CloudError> {
        self.0.send(request)
    }
    fn isolated_session(&self) -> Result<Option<Box<dyn CloudHttpTransport>>, CloudError> {
        self.0.isolated_session()
    }
    fn manages_cookie_session(&self) -> bool {
        self.0.manages_cookie_session()
    }
    fn cookies_for_url(&self, url: &str) -> Result<Vec<String>, CloudError> {
        self.0.cookies_for_url(url)
    }
}

struct UploadHttpRef<'a>(&'a dyn UploadHttp);
impl CloudHttpTransport for UploadHttpRef<'_> {
    fn send(&self, request: CloudHttpRequest) -> Result<CloudHttpResponse, CloudError> {
        self.0.send(request)
    }
    fn isolated_session(&self) -> Result<Option<Box<dyn CloudHttpTransport>>, CloudError> {
        self.0.isolated_session()
    }
    fn manages_cookie_session(&self) -> bool {
        self.0.manages_cookie_session()
    }
    fn cookies_for_url(&self, url: &str) -> Result<Vec<String>, CloudError> {
        self.0.cookies_for_url(url)
    }
}
impl CloudUploadTransport for UploadHttpRef<'_> {
    fn send_upload(&self, request: UploadHttpRequest) -> Result<UploadHttpResponse, CloudError> {
        self.0.send_upload(request)
    }
}
