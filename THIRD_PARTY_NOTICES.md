# Third-party protocol references

- [Aria2 Next commit 2521bb0](https://github.com/AnInsomniacy/aria2-next/commit/2521bb0cebbe2feedd096879c54a73efd818fa43), GPL-2.0: bundled Windows x86_64 transfer engine for HTTP/HTTPS, BitTorrent, ED2K, and native HLS/MPEG-DASH segments. The pinned executable SHA-256 is `ad5159a68b3132d83444d4bb8285d750f5c3579fe581fc14211426b30bd39d35`; corresponding source is available from the linked upstream commit.
- [suppaftp 11.0.0](https://github.com/veeso/suppaftp), MIT OR Apache-2.0: linked as the in-process asynchronous FTP client, including binary transfers and REST-based resume.

- Quark CloudDrive official skill (`quark-clouddrive/quarkclouddrive_offical`), Apache-2.0: request shapes and file-operation protocol reference. No upstream source is copied verbatim.
- `lnzx/pikpak`, MIT: PikPak non-official mobile API protocol reference (password sign-in, token refresh and file operations). Provider protocol may change and captcha/risk controls require interactive web authorization.
- `Bengerthelorf/pikpaktui`, Apache-2.0: PikPak Android-client compatibility constants, captcha lifecycle, and resumable OSS multipart upload protocol reference. No upstream source is copied verbatim.

PikPak defaults to the public Android compatibility client identifiers used by the cited open-source clients. Packagers can override them with `ALLDOWNLOADER_PIKPAK_CLIENT_ID` and `ALLDOWNLOADER_PIKPAK_CLIENT_SECRET`.

User passwords are used only for an authentication request and are never persisted. Refresh tokens and browser cookies must be stored through the operating-system credential store.
