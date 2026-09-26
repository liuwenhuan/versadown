//! Explicit opt-in: uploads a harmless new fixture and publicly shares only that file.
//! Leaves the fixture and one-day share available for manual verification.
use versadown_lib::cloud::{
    CloudShareOptions, PikPakClient, ReqwestTransport, SystemCredentialStore,
};
use serde_json::{json, Value};
use std::{error::Error, path::PathBuf, time::Duration};

#[test]
#[ignore = "read-only inspection of live share-list schema, no private values logged"]
fn inspect_own_share_list_schema() -> Result<(), Box<dyn Error>> {
    assert_eq!(
        std::env::var("VERSADOWN_LIVE_CLOUD").as_deref(),
        Ok("1")
    );
    let credentials = SystemCredentialStore::new(PathBuf::from(std::env::var(
        "VERSADOWN_LIVE_SESSION_ROOT",
    )?))?;
    let transport = ReqwestTransport::new()?;
    let value = PikPakClient::new(&transport, &credentials).request("GET",
        "https://api-drive.mypikpak.com/drive/v1/share/list?limit=100&thumbnail_size=SIZE_SMALL&page_token=".into(), None)?;
    fn shape(value: &Value) -> Value {
        match value {
            Value::Object(fields) => fields.iter().map(|(k, v)| (k.clone(), shape(v))).collect(),
            Value::Array(items) => Value::Array(items.iter().take(1).map(shape).collect()),
            Value::String(s) => json!(if s.is_empty() { "<empty>" } else { "<string>" }),
            Value::Number(_) => json!("<number>"),
            Value::Bool(_) => json!("<bool>"),
            Value::Null => Value::Null,
        }
    }
    println!("{}", serde_json::to_string_pretty(&shape(&value))?);
    for item in value["data"].as_array().into_iter().flatten() {
        if item["title"]
            .as_str()
            .is_some_and(|name| name.starts_with("VersaDown-share-update-test-"))
        {
            println!(
                "[own-fixture-readback] name={} share_id={} code={}",
                item["title"], item["share_id"], item["pass_code"]
            );
        }
    }
    Ok(())
}

#[test]
#[ignore = "only operates on an explicitly named harmless fixture created by this test suite"]
fn verify_existing_update_fixture() -> Result<(), Box<dyn Error>> {
    assert_eq!(
        std::env::var("VERSADOWN_LIVE_CLOUD").as_deref(),
        Ok("1")
    );
    let id = std::env::var("VERSADOWN_UPDATE_FIXTURE_ID")?;
    let name = std::env::var("VERSADOWN_UPDATE_FIXTURE_NAME")?;
    assert!(name.starts_with("VersaDown-share-update-test-") && name.ends_with(".txt"));
    let credentials = SystemCredentialStore::new(PathBuf::from(std::env::var(
        "VERSADOWN_LIVE_SESSION_ROOT",
    )?))?;
    let transport = ReqwestTransport::new()?;
    let client = PikPakClient::new(&transport, &credentials);
    let share = client
        .list_shares()?
        .into_iter()
        .find(|s| s.id == id && s.name == name)
        .ok_or("exact test fixture not found")?;
    client.update_share_pass_code(&id, "a7B2")?;
    let after = client
        .list_shares()?
        .into_iter()
        .find(|s| s.id == id)
        .ok_or("test fixture disappeared")?;
    if after.pass_code.as_deref() != Some("a7B2") {
        return Err("new code not persisted".into());
    }
    let content = format!("Harmless outbound share update test.\n{name}\n");
    verify_anonymous_with_code(&id, &name, content.len(), "a7B2")?;
    client.cancel_shares(&[id])?;
    client.trash(&share.source_file_ids)?;
    println!(
        "[fixture-patch] PASS new code verified; exact fixture share revoked and file trashed"
    );
    Ok(())
}

#[test]
#[ignore = "uploads a harmless fixture; creates, edits and revokes only its own share"]
fn own_file_share_update_roundtrip() -> Result<(), Box<dyn Error>> {
    assert_eq!(
        std::env::var("VERSADOWN_LIVE_CLOUD").as_deref(),
        Ok("1")
    );
    let credentials = SystemCredentialStore::new(PathBuf::from(std::env::var(
        "VERSADOWN_LIVE_SESSION_ROOT",
    )?))?;
    let transport = ReqwestTransport::new()?;
    let client = PikPakClient::new(&transport, &credentials);
    let name = format!(
        "VersaDown-share-update-test-{}.txt",
        uuid::Uuid::new_v4()
    );
    let content = format!("Harmless outbound share update test.\n{name}\n");
    let local = tempfile::tempdir()?;
    let path = local.path().join(&name);
    std::fs::write(&path, &content)?;
    let file = client.upload_file("0", &path)?;
    let share = client.create_share(
        &[file.id.clone()],
        &CloudShareOptions {
            name: name.clone(),
            pass_code: None,
            generate_pass_code: true,
            expires_in_days: Some(1),
        },
    )?;
    println!(
        "[share-update] generated random code for new harmless fixture name={name} id={}",
        share.id
    );
    let result = (|| -> Result<(), Box<dyn Error>> {
        let before = client
            .list_shares()?
            .into_iter()
            .find(|s| s.id == share.id)
            .ok_or("new share missing from list")?;
        if before.name != name
            || before.source_file_ids != vec![file.id.clone()]
            || before.created_at.is_none()
        {
            return Err("listed share metadata mismatch".into());
        }
        let new_code = if share.pass_code.as_deref() == Some("a7B2") {
            "c8D3"
        } else {
            "a7B2"
        };
        client.update_share_pass_code(&share.id, new_code)?;
        let after = client
            .list_shares()?
            .into_iter()
            .find(|s| s.id == share.id)
            .ok_or("updated share missing from list")?;
        if after.pass_code.as_deref() != Some(new_code) || after.name != name {
            return Err("server readback did not confirm requested code/name".into());
        }
        println!("[share-update] list file name/time/source and saved new code verified");
        verify_anonymous_with_codes(
            &share.id,
            &name,
            content.len(),
            new_code,
            share.pass_code.as_deref(),
        )?;
        println!("[share-update] PASS changed code is usable by anonymous recipient");
        Ok(())
    })();
    // Only these ids were created by this invocation; never touch prior user data.
    let revoke = client.cancel_shares(&[share.id]);
    if revoke.is_ok() {
        println!("[share-update] fixture share revoked");
    }
    let trash = client.trash(&[file.id]);
    if trash.is_ok() {
        println!("[share-update] fixture file moved to recoverable trash");
    }
    result?;
    revoke?;
    trash?;
    Ok(())
}

#[test]
#[ignore = "creates a real one-day public share of a newly uploaded test file"]
fn own_file_creates_public_share_visible_without_login() -> Result<(), Box<dyn Error>> {
    assert_eq!(
        std::env::var("VERSADOWN_LIVE_CLOUD").as_deref(),
        Ok("1")
    );
    if let Ok(share_id) = std::env::var("VERSADOWN_VERIFY_EXISTING_TEST_SHARE") {
        let name = std::env::var("VERSADOWN_VERIFY_TEST_NAME")?;
        assert!(name.starts_with("VersaDown-public-share-test-"));
        let contents = format!("Harmless VersaDown outbound sharing test.\n{name}\n");
        return verify_anonymous(&share_id, &name, contents.len());
    }
    let root = PathBuf::from(std::env::var("VERSADOWN_LIVE_SESSION_ROOT")?);
    assert!(root.is_dir(), "explicit real session directory must exist");
    let credentials = SystemCredentialStore::new(root)?;
    let transport = ReqwestTransport::new()?;
    let client = PikPakClient::new(&transport, &credentials);
    client.account()?;
    println!("[outbound-share] saved-session-ready");

    let name = format!(
        "VersaDown-public-share-test-{}.txt",
        uuid::Uuid::new_v4()
    );
    let contents = format!("Harmless VersaDown outbound sharing test.\n{name}\n");
    let local = tempfile::tempdir()?;
    let path = local.path().join(&name);
    std::fs::write(&path, contents.as_bytes())?;
    let file = client.upload_file("0", &path)?;
    assert_eq!(file.name, name);
    assert!(!file.id.is_empty());
    println!(
        "[outbound-share] uploaded name={name} bytes={}",
        contents.len()
    );

    let created = client.request(
        "POST",
        "https://api-drive.mypikpak.com/drive/v1/share".into(),
        Some(json!({"file_ids":[file.id],"share_to":"publiclink",
            "expiration_days":1,"pass_code_option":"NOT_REQUIRED"})),
    )?;
    let share_id = created["share_id"].as_str().ok_or("missing share_id")?;
    let share_url = created["share_url"].as_str().ok_or("missing share_url")?;
    let parsed = reqwest::Url::parse(share_url)?;
    assert_eq!(parsed.scheme(), "https");
    assert_eq!(parsed.host_str(), Some("mypikpak.com"));
    println!("[outbound-share] created expiration_days=1 url={share_url}");
    verify_anonymous(share_id, &name, contents.len())
}

fn verify_anonymous(
    share_id: &str,
    name: &str,
    expected_size: usize,
) -> Result<(), Box<dyn Error>> {
    verify_anonymous_with_code(share_id, name, expected_size, "")
}

fn verify_anonymous_with_code(
    share_id: &str,
    name: &str,
    expected_size: usize,
    code: &str,
) -> Result<(), Box<dyn Error>> {
    verify_anonymous_with_codes(share_id, name, expected_size, code, None)
}

fn verify_anonymous_with_codes(
    share_id: &str,
    name: &str,
    expected_size: usize,
    code: &str,
    previous_code: Option<&str>,
) -> Result<(), Box<dyn Error>> {
    // This separate client has neither Authorization nor Cookie nor a cookie jar.
    let anonymous = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(60))
        .build()?;
    let device = uuid::Uuid::new_v4().simple().to_string();
    // Follow the provider's anonymous SDK initialization; do not solve challenges.
    let client_id = "YUMx5nI8ZU8Ap8pm";
    let timestamp = "1789110095225";
    let mut sign = format!("{client_id}undefineddrive.mypikpak.com{device}{timestamp}");
    for salt in [
        "fyZ4+p77W1U4zcWBUwefAIFhFxvADWtT1wzolCxhg9q7etmGUjXr",
        "uSUX02HYJ1IkyLdhINEFcCf7l2",
        "iWt97bqD/qvjIaPXB2Ja5rsBWtQtBZZmaHH2rMR41",
        "3binT1s/5a1pu3fGsN",
        "8YCCU+AIr7pg+yd7CkQEY16lDMwi8Rh4WNp5",
        "DYS3StqnAEKdGddRP8CJrxUSFh",
        "crquW+4",
        "ryKqvW9B9hly+JAymXCIfag5Z",
        "Hr08T/NDTX1oSJfHk90c",
        "i",
    ] {
        sign = format!("{:x}", md5::compute(format!("{sign}{salt}")));
    }
    let init = anonymous
        .post("https://user.mypikpak.com/v1/shield/captcha/init")
        .header("X-Client-Id", client_id)
        .header("X-Device-Id", &device)
        .json(&json!({"client_id":client_id,"device_id":device,
            "action":"GET:/drive/v1/share","captcha_token":"",
            "redirect_uri":"https://mypikpak.com/loading",
            "meta":{"client_version":"undefined","package_name":"drive.mypikpak.com",
                "timestamp":timestamp,"captcha_sign":format!("1.{sign}")}}))
        .send()?;
    let init_status = init.status();
    let init_body: Value = init.json()?;
    println!(
        "[outbound-share] anonymous-sdk-init HTTP={} code={}",
        init_status.as_u16(),
        init_body["error_code"].as_i64().unwrap_or(0)
    );
    assert!(
        init_status.is_success(),
        "anonymous SDK initialization failed"
    );
    assert!(
        init_body["url"].as_str().unwrap_or("").is_empty(),
        "anonymous access requires human verification; stop here"
    );
    let token = init_body["captcha_token"]
        .as_str()
        .ok_or("missing anonymous SDK token")?;
    let response = anonymous
        .get("https://api-drive.mypikpak.com/drive/v1/share")
        .header("X-Client-Id", client_id)
        .header("X-Device-Id", &device)
        .header("X-Captcha-Token", token)
        .query(&[
            ("share_id", share_id),
            ("pass_code", code),
            ("limit", "100"),
            ("thumbnail_size", "SIZE_LARGE"),
        ])
        .send()?;
    let status = response.status();
    let public: Value = response.json()?;
    println!(
        "[outbound-share] anonymous-inspect HTTP={} status={} code={}",
        status.as_u16(),
        public["share_status"].as_str().unwrap_or("missing"),
        public["error_code"].as_i64().unwrap_or(0)
    );
    assert!(status.is_success(), "anonymous share inspection failed");
    assert_eq!(public["share_status"].as_str(), Some("OK"));
    let files = public["files"].as_array().ok_or("missing public files")?;
    let visible = files
        .iter()
        .find(|f| f["name"].as_str() == Some(name))
        .ok_or("new fixture not visible anonymously")?;
    assert_eq!(visible["name"].as_str(), Some(name));
    let size = visible["size"]
        .as_u64()
        .or_else(|| visible["size"].as_str()?.parse().ok());
    assert_eq!(size, Some(expected_size as u64));
    if let Some(previous) = previous_code.filter(|previous| *previous != code) {
        let denied: Value = anonymous
            .get("https://api-drive.mypikpak.com/drive/v1/share")
            .header("X-Client-Id", client_id)
            .header("X-Device-Id", &device)
            .header("X-Captcha-Token", token)
            .query(&[
                ("share_id", share_id),
                ("pass_code", previous),
                ("limit", "100"),
            ])
            .send()?
            .json()?;
        if denied["share_status"].as_str() != Some("PASS_CODE_ERROR") {
            return Err("old code rejection not confirmed".into());
        }
        println!("[outbound-share] old code rejected by anonymous access");
    }
    println!("[outbound-share] PASS anonymous file name and size match uploaded fixture");
    Ok(())
}
