//! User-actionable OneDrive failures. Their wire text is a stable localization
//! key (never provider text) so the UI can translate it.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OnedriveFailure {
    /// The link was removed, expired, is private or password protected.
    LinkUnavailable,
    /// A recognised link that this app cannot read (business / SharePoint).
    LinkUnsupported,
    /// The shared folder is larger than the listing limits.
    ShareTooLarge,
    /// This build carries no application id, so signing in is impossible.
    ClientIdMissing,
    /// The user refused the consent screen.
    AuthorizationDenied,
    /// The browser sign-in was not completed in time.
    AuthorizationTimeout,
    /// The browser callback was unusable (state mismatch, error response).
    AuthorizationFailed,
    /// Microsoft throttled or briefly failed the request; retrying later helps.
    ServiceBusy,
    /// The anonymous web-page interface answered in a way this build does not
    /// understand: it is not an official Microsoft API and may have changed.
    AnonymousInterfaceChanged,
}

impl OnedriveFailure {
    pub fn code(&self) -> &'static str {
        match self {
            Self::LinkUnavailable => "onedrive_link_unavailable",
            Self::LinkUnsupported => "onedrive_link_unsupported",
            Self::ShareTooLarge => "onedrive_share_too_large",
            Self::ClientIdMissing => "onedrive_client_id_missing",
            Self::AuthorizationDenied => "onedrive_authorization_denied",
            Self::AuthorizationTimeout => "onedrive_authorization_timeout",
            Self::AuthorizationFailed => "onedrive_authorization_failed",
            Self::ServiceBusy => "onedrive_service_busy",
            Self::AnonymousInterfaceChanged => "onedrive_anonymous_interface_changed",
        }
    }

    /// Localization key.
    pub fn message(&self) -> &'static str {
        match self {
            Self::LinkUnavailable => "cloud.error.onedrive.linkUnavailable",
            Self::LinkUnsupported => "cloud.error.onedrive.linkUnsupported",
            Self::ShareTooLarge => "cloud.error.onedrive.shareTooLarge",
            Self::ClientIdMissing => "cloud.error.onedrive.clientIdMissing",
            Self::AuthorizationDenied => "cloud.error.onedrive.authorizationDenied",
            Self::AuthorizationTimeout => "cloud.error.onedrive.authorizationTimeout",
            Self::AuthorizationFailed => "cloud.error.onedrive.authorizationFailed",
            Self::ServiceBusy => "cloud.error.onedrive.serviceBusy",
            Self::AnonymousInterfaceChanged => "cloud.error.onedrive.anonymousInterfaceChanged",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_are_stable_localization_keys() {
        assert_eq!(
            OnedriveFailure::LinkUnavailable.message(),
            "cloud.error.onedrive.linkUnavailable"
        );
        assert_eq!(
            OnedriveFailure::ClientIdMissing.code(),
            "onedrive_client_id_missing"
        );
    }
}
