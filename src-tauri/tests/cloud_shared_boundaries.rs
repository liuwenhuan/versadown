use regex::Regex;

const SHARED_RUNTIME_MODULES: &[(&str, &str)] = &[
    (
        "cloud/auth_supervisor.rs",
        include_str!("../src/cloud/auth_supervisor.rs"),
    ),
    (
        "cloud/browser_login.rs",
        include_str!("../src/cloud/browser_login.rs"),
    ),
    (
        "cloud/commands.rs",
        include_str!("../src/cloud/commands.rs"),
    ),
    (
        "cloud/commands/auth.rs",
        include_str!("../src/cloud/commands/auth.rs"),
    ),
    (
        "cloud/commands/browser.rs",
        include_str!("../src/cloud/commands/browser.rs"),
    ),
    (
        "cloud/commands/common.rs",
        include_str!("../src/cloud/commands/common.rs"),
    ),
    (
        "cloud/commands/download.rs",
        include_str!("../src/cloud/commands/download.rs"),
    ),
    (
        "cloud/commands/drive.rs",
        include_str!("../src/cloud/commands/drive.rs"),
    ),
    (
        "cloud/commands/share.rs",
        include_str!("../src/cloud/commands/share.rs"),
    ),
    (
        "cloud/commands/upload.rs",
        include_str!("../src/cloud/commands/upload.rs"),
    ),
    ("cloud/common.rs", include_str!("../src/cloud/common.rs")),
    (
        "cloud/credentials.rs",
        include_str!("../src/cloud/credentials.rs"),
    ),
    ("cloud/driver.rs", include_str!("../src/cloud/driver.rs")),
    (
        "cloud/driver/native.rs",
        include_str!("../src/cloud/driver/native.rs"),
    ),
    ("cloud/http.rs", include_str!("../src/cloud/http.rs")),
    (
        "cloud/password-login.js",
        include_str!("../src/cloud/password-login.js"),
    ),
    (
        "cloud/xunlei-web-capture.js",
        include_str!("../src/cloud/xunlei-web-capture.js"),
    ),
    ("cloud/session.rs", include_str!("../src/cloud/session.rs")),
    ("cloud/share.rs", include_str!("../src/cloud/share.rs")),
    (
        "cloud/share/capabilities.rs",
        include_str!("../src/cloud/share/capabilities.rs"),
    ),
    (
        "cloud/share/models.rs",
        include_str!("../src/cloud/share/models.rs"),
    ),
    (
        "cloud/share/repository.rs",
        include_str!("../src/cloud/share/repository.rs"),
    ),
    (
        "cloud/share/service.rs",
        include_str!("../src/cloud/share/service.rs"),
    ),
    ("cloud/types.rs", include_str!("../src/cloud/types.rs")),
    ("cloud/upload.rs", include_str!("../src/cloud/upload.rs")),
    ("parser.rs", include_str!("../src/parser.rs")),
];

fn runtime_source<'a>(name: &str, source: &'a str) -> &'a str {
    // These two modules keep unit-only URL fixtures at the end of the file.
    // Provider URLs are useful in tests, but must not enter their runtime code.
    if matches!(
        name,
        "cloud/browser_login.rs" | "cloud/http.rs" | "parser.rs"
    ) {
        source.split("#[cfg(test)]").next().unwrap_or(source)
    } else {
        source
    }
}

#[test]
fn shared_cloud_runtime_contains_no_domain_literals() {
    let domain = Regex::new(
        r#"(?i)(?:https?://|["'])(?:[a-z0-9](?:[a-z0-9-]*[a-z0-9])?\.)+(?:app|cloud|cn|co|com|dev|io|me|net|org|tv)\b"#,
    )
    .unwrap();
    let mut violations = Vec::new();

    for (name, source) in SHARED_RUNTIME_MODULES {
        for (index, line) in runtime_source(name, source).lines().enumerate() {
            if let Some(found) = domain.find(line) {
                violations.push(format!("{name}:{}: {}", index + 1, found.as_str()));
            }
        }
    }

    assert!(
        violations.is_empty(),
        "provider domains belong in provider adapters or registry, not shared runtime modules:\n{}",
        violations.join("\n")
    );
}
