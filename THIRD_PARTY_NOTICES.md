# Third-party protocol references

- [Aria2 Next 2.7.5](https://github.com/AnInsomniacy/aria2-next), GPL-2.0: bundled Windows x86_64 transfer engine for HTTP/HTTPS, BitTorrent, ED2K, and application-orchestrated HLS/MPEG-DASH segments. The pinned executable SHA-256 is `7c1f49bf9f22f15f684ce1dcc76768f04b5dc3795281aef750d1e5ad78ff1459`; corresponding source is available from the linked upstream release/tag.
- [suppaftp 11.0.0](https://github.com/veeso/suppaftp), MIT OR Apache-2.0: linked as the in-process asynchronous FTP client, including binary transfers and REST-based resume.

- Quark CloudDrive official skill (`quark-clouddrive/quarkclouddrive_offical`), Apache-2.0: request shapes and file-operation protocol reference. No upstream source is copied verbatim.
- `lnzx/pikpak`, MIT: PikPak non-official mobile API protocol reference (password sign-in, token refresh and file operations). Provider protocol may change and captcha/risk controls require interactive web authorization.
- `Bengerthelorf/pikpaktui`, Apache-2.0: PikPak Android-client compatibility constants, captcha lifecycle, and resumable OSS multipart upload protocol reference. No upstream source is copied verbatim.

PikPak defaults to the public Android compatibility client identifiers used by the cited open-source clients. Packagers can override them with `ALLDOWNLOADER_PIKPAK_CLIENT_ID` and `ALLDOWNLOADER_PIKPAK_CLIENT_SECRET`.

User passwords are used only for an authentication request and are never persisted. Refresh tokens and browser cookies must be stored through the operating-system credential store.
