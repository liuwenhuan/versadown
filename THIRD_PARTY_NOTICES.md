# Third-party protocol references

- [Aria2 Next 2.8.0](https://github.com/AnInsomniacy/aria2-next/releases/tag/v2.8.0), GPL-2.0: bundled official Windows x86_64 transfer engine for HTTP/HTTPS, BitTorrent, ED2K, and native HLS/MPEG-DASH segments. The pinned executable SHA-256 is `74183154fa993a96ffd8c21b971b4783e05f0b2ec189e134a3b6f92292865758`; corresponding source is available from the linked upstream release tag.
- [suppaftp 11.0.0](https://github.com/veeso/suppaftp), MIT OR Apache-2.0: linked as the in-process asynchronous FTP client, including binary transfers and REST-based resume.

- Quark CloudDrive official skill (`quark-clouddrive/quarkclouddrive_offical`), Apache-2.0: request shapes and file-operation protocol reference. No upstream source is copied verbatim.
- `lnzx/pikpak`, MIT: PikPak non-official mobile API protocol reference (password sign-in, token refresh and file operations). Provider protocol may change and captcha/risk controls require interactive web authorization.
- `Bengerthelorf/pikpaktui`, Apache-2.0: PikPak Android-client compatibility constants, captcha lifecycle, and resumable OSS multipart upload protocol reference. No upstream source is copied verbatim.

PikPak defaults to the public Android compatibility client identifiers used by the cited open-source clients. Packagers can override them with `VERSADOWN_PIKPAK_CLIENT_ID` and `VERSADOWN_PIKPAK_CLIENT_SECRET`.

User passwords are used only for an authentication request and are never persisted. Refresh tokens and browser cookies must be stored through the operating-system credential store.
