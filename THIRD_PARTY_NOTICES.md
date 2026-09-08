# Third-party protocol references

- Quark CloudDrive official skill (`quark-clouddrive/quarkclouddrive_offical`), Apache-2.0: request shapes and file-operation protocol reference. No upstream source is copied verbatim.
- `lnzx/pikpak`, MIT: PikPak non-official mobile API protocol reference (password sign-in, token refresh and file operations). Provider protocol may change and captcha/risk controls require interactive web authorization.
- `Bengerthelorf/pikpaktui`, Apache-2.0: PikPak Android-client compatibility constants, captcha lifecycle, and resumable OSS multipart upload protocol reference. No upstream source is copied verbatim.

PikPak defaults to the public Android compatibility client identifiers used by the cited open-source clients. Packagers can override them with `ALLDOWNLOADER_PIKPAK_CLIENT_ID` and `ALLDOWNLOADER_PIKPAK_CLIENT_SECRET`.

User passwords are used only for an authentication request and are never persisted. Refresh tokens and browser cookies must be stored through the operating-system credential store.
