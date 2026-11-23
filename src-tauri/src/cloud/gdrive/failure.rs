//! User-actionable Google Drive failures. Their wire text is a stable
//! localization key (never provider text), so the UI can translate it.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GdriveFailure {
    /// A Google link whose kind cannot be downloaded (forms, sites, the Drive
    /// home page, published-to-web documents, ...).
    LinkUnsupported,
    /// The file or folder is not shared with "anyone with the link", or the
    /// link needs a signed-in Google account.
    AccessDenied,
    /// The item was deleted or the id never existed.
    NotFound,
    /// Google refuses further downloads of a popular file for about a day.
    QuotaExceeded,
    /// The folder is larger than the walker's defensive limits.
    FolderTooLarge,
    /// The build carries no Google OAuth client; sign-in is unavailable.
    NotConfigured,
    /// The user declined the consent screen.
    ConsentDenied,
    /// No answer arrived on the loopback address in time.
    ConsentTimeout,
    /// The account is not allowed to use this OAuth client (testing mode, or
    /// the app is not verified for this user).
    ClientNotAllowed,
    /// A Google-native document in a signed-in drive: the Drive API can only
    /// export small ones and its export endpoint cannot be resumed.
    NativeNotDownloadable,
    /// The API asks the client to slow down.
    RateLimited,
    /// The embedded folder page answered in a way this build does not
    /// understand: it is a web page, not a published API, and may have changed.
    FolderPageChanged,
}

impl GdriveFailure {
    pub fn code(&self) -> &'static str {
        match self {
            Self::LinkUnsupported => "gdrive_link_unsupported",
            Self::AccessDenied => "gdrive_access_denied",
            Self::NotFound => "gdrive_not_found",
            Self::QuotaExceeded => "gdrive_quota_exceeded",
            Self::FolderTooLarge => "gdrive_folder_too_large",
            Self::NotConfigured => "gdrive_not_configured",
            Self::ConsentDenied => "gdrive_consent_denied",
            Self::ConsentTimeout => "gdrive_consent_timeout",
            Self::ClientNotAllowed => "gdrive_client_not_allowed",
            Self::NativeNotDownloadable => "gdrive_native_not_downloadable",
            Self::RateLimited => "gdrive_rate_limited",
            Self::FolderPageChanged => "gdrive_folder_page_changed",
        }
    }

    /// Localization key; it is also the wire message.
    pub fn message_key(&self) -> &'static str {
        match self {
            Self::LinkUnsupported => "cloud.error.gdrive.linkUnsupported",
            Self::AccessDenied => "cloud.error.gdrive.accessDenied",
            Self::NotFound => "cloud.error.gdrive.notFound",
            Self::QuotaExceeded => "cloud.error.gdrive.quotaExceeded",
            Self::FolderTooLarge => "cloud.error.gdrive.folderTooLarge",
            Self::NotConfigured => "cloud.error.gdrive.notConfigured",
            Self::ConsentDenied => "cloud.error.gdrive.consentDenied",
            Self::ConsentTimeout => "cloud.error.gdrive.consentTimeout",
            Self::ClientNotAllowed => "cloud.error.gdrive.clientNotAllowed",
            Self::NativeNotDownloadable => "cloud.error.gdrive.nativeNotDownloadable",
            Self::RateLimited => "cloud.error.gdrive.rateLimited",
            Self::FolderPageChanged => "cloud.error.gdrive.folderPageChanged",
        }
    }

    pub fn message(&self) -> String {
        self.message_key().to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_are_stable_localization_keys() {
        assert_eq!(
            GdriveFailure::QuotaExceeded.message(),
            "cloud.error.gdrive.quotaExceeded"
        );
        assert_eq!(GdriveFailure::AccessDenied.code(), "gdrive_access_denied");
    }
}
