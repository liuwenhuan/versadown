use alldownloader_lib::cloud::{
    baidu::BaiduClient, load_saved_password_login, CloudProvider, DpapiCredentialStore,
    ReqwestTransport,
};
use std::path::PathBuf;

fn main() {
    match probe() {
        Ok((display_name, item_count)) => {
            println!("百度原生登录后台探针通过：账号 {display_name}，根目录 {item_count} 项。")
        }
        Err(stage) => {
            eprintln!("百度原生登录后台探针失败：{stage}");
            std::process::exit(1);
        }
    }
}

fn probe() -> Result<(String, usize), String> {
    let credential_root = std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .map(|path| {
            path.join("com.alldownloader.desktop")
                .join("cloud-sessions")
        })
        .ok_or_else(|| "无法定位应用凭据目录".to_owned())?;
    let store = DpapiCredentialStore::new(credential_root)
        .map_err(|_| "无法读取加密凭据存储".to_owned())?;
    let credentials = load_saved_password_login(&store, CloudProvider::Baidu)
        .map_err(|_| "无法解密已保存的百度登录信息".to_owned())?
        .ok_or_else(|| "没有已保存的百度登录信息".to_owned())?;
    let http = ReqwestTransport::new().map_err(|_| "无法初始化 HTTPS 客户端".to_owned())?;
    let session =
        BaiduClient::login_password(credentials.username(), credentials.password(), &http)
            .map_err(|error| format!("原生登录或登录后校验失败：{error:?}"))?;
    drop(credentials);

    let client = BaiduClient::new(session, &http);
    let account = client
        .account()
        .map_err(|error| format!("账号读取失败：{error:?}"))?;
    let root = client
        .directory("/")
        .map_err(|error| format!("根目录读取失败：{error:?}"))?;
    Ok((account.display_name, root.items.len()))
}
