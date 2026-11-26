use super::*;

fn pairs(entries: &[(&str, &str)]) -> Proxies {
    entries
        .iter()
        .map(|(key, value)| (key.to_string(), value.to_string()))
        .collect()
}

fn manual_settings() -> ProxySettings {
    ProxySettings {
        enabled: true,
        host: "manual.invalid".into(),
        port: 8080,
        ..Default::default()
    }
}

const MANUAL: &str = "http://manual.invalid:8080/";
const ENVIRONMENT: &str = "http://environment.invalid:10808";
const SYSTEM: &str = "http://system.invalid:3128";

#[test]
fn authenticated_manual_proxy_is_validated_and_encoded() {
    let proxy = ProxySettings {
        enabled: true,
        protocol: "https".into(),
        host: "127.0.0.1".into(),
        port: 1080,
        username: "user@example.com".into(),
        password: "p@ss word".into(),
        ..Default::default()
    };
    assert_eq!(
        manual_proxy_url(&proxy).unwrap().unwrap(),
        "https://user%40example.com:p%40ss%20word@127.0.0.1:1080/"
    );
    assert_eq!(manual_proxy_url(&ProxySettings::default()).unwrap(), None);
}

#[test]
fn settings_files_without_the_direct_field_keep_their_meaning() {
    let parse = |json: &str| serde_json::from_str::<ProxySettings>(json).unwrap();
    let manual = parse(r#"{"enabled":true,"protocol":"http","host":"manual.invalid","port":8080}"#);
    assert_eq!(global_route(&manual).unwrap(), ProxyRoute::Manual(MANUAL.into()));
    assert_eq!(global_route(&parse(r#"{"enabled":false}"#)).unwrap(), ProxyRoute::System);
    // Direct wins over a manual address kept for later use.
    let direct = parse(r#"{"enabled":true,"direct":true,"host":"manual.invalid","port":8080}"#);
    assert_eq!(global_route(&direct).unwrap(), ProxyRoute::Direct);

    let saved = ProxySettings { direct: true, ..manual_settings() };
    let round_trip: ProxySettings =
        serde_json::from_value(serde_json::to_value(&saved).unwrap()).unwrap();
    assert!(round_trip.direct);
    assert_eq!(global_route(&round_trip).unwrap(), ProxyRoute::Direct);
}

#[test]
fn task_choice_combines_with_every_global_mode() {
    let system = ProxySettings::default();
    let manual = manual_settings();
    let direct = ProxySettings { enabled: false, direct: true, ..manual_settings() };
    let follow_system_with_address = ProxySettings { enabled: false, ..manual_settings() };
    let manual_route = ProxyRoute::Manual(MANUAL.into());

    assert_eq!(task_route(&system, TaskNetwork::Follow).unwrap(), ProxyRoute::System);
    assert_eq!(task_route(&manual, TaskNetwork::Follow).unwrap(), manual_route);
    assert_eq!(task_route(&direct, TaskNetwork::Follow).unwrap(), ProxyRoute::Direct);

    for settings in [&system, &manual, &direct] {
        assert_eq!(task_route(settings, TaskNetwork::Direct).unwrap(), ProxyRoute::Direct);
    }

    // "Use proxy" needs a manual address, but not the manual mode.
    assert!(task_route(&system, TaskNetwork::Proxy).is_err());
    assert_eq!(task_route(&manual, TaskNetwork::Proxy).unwrap(), manual_route);
    assert_eq!(task_route(&direct, TaskNetwork::Proxy).unwrap(), manual_route);
    assert_eq!(
        task_route(&follow_system_with_address, TaskNetwork::Proxy).unwrap(),
        manual_route
    );
}

#[test]
fn task_network_round_trips_its_stored_names() {
    for network in [TaskNetwork::Follow, TaskNetwork::Direct, TaskNetwork::Proxy] {
        assert_eq!(TaskNetwork::parse(network.as_str()), Some(network));
        let json = serde_json::to_string(&network).unwrap();
        assert_eq!(json, format!("\"{}\"", network.as_str()));
    }
    assert_eq!(TaskNetwork::parse("other"), None);
}

#[test]
fn route_debug_output_never_contains_the_manual_url() {
    let route = ProxyRoute::Manual("http://user:secret@proxy.invalid:1/".into());
    assert!(!format!("{route:?}").contains("secret"));
}

/// The three backdrops a child process can inherit a proxy from.
fn backdrops() -> [(&'static str, Proxies, Proxies); 3] {
    [
        ("environment", pairs(&[("http", ENVIRONMENT), ("https", ENVIRONMENT)]), Proxies::new()),
        ("system", Proxies::new(), pairs(&[("http", SYSTEM)])),
        ("none", Proxies::new(), Proxies::new()),
    ]
}

fn video_command(route: &ProxyRoute, environment: Proxies, system: Proxies) -> (tokio::process::Command, AppliedProxy) {
    let mut cmd = tokio::process::Command::new("yt-dlp");
    let keys = ["HTTP_PROXY", "http_proxy", "https_proxy", "ALL_PROXY", "no_proxy"]
        .map(std::ffi::OsString::from)
        .to_vec();
    let applied = configure_command(&mut cmd, route, keys, environment, || system).unwrap();
    (cmd, applied)
}

fn command_environment(cmd: &tokio::process::Command) -> Vec<(String, Option<String>)> {
    cmd.as_std()
        .get_envs()
        .map(|(key, value)| {
            (
                key.to_string_lossy().into_owned(),
                value.map(|value| value.to_string_lossy().into_owned()),
            )
        })
        .collect()
}

fn command_arguments(cmd: &tokio::process::Command) -> Vec<String> {
    cmd.as_std().get_args().map(|arg| arg.to_string_lossy().into_owned()).collect()
}

#[test]
fn video_direct_route_clears_every_inherited_proxy_and_disables_os_lookup() {
    for (backdrop, environment, system) in backdrops() {
        let (cmd, applied) = video_command(&ProxyRoute::Direct, environment, system);
        let envs = command_environment(&cmd);
        assert!(
            envs.iter().all(|(_, value)| value.is_none()),
            "{backdrop}: {envs:?}"
        );
        for key in ["HTTP_PROXY", "http_proxy", "https_proxy", "ALL_PROXY", "no_proxy"] {
            assert!(envs.iter().any(|(name, value)| name == key && value.is_none()), "{backdrop}: {key}");
        }
        assert_eq!(command_arguments(&cmd), ["--proxy", ""], "{backdrop}");
        assert!(!applied.proxied_for("https://www.bilibili.com/video/BV1"));
    }
}

#[test]
fn video_system_route_forwards_the_inherited_proxy() {
    let expected = [Some(ENVIRONMENT), Some(SYSTEM), None];
    for ((backdrop, environment, system), expected) in backdrops().into_iter().zip(expected) {
        let (cmd, applied) = video_command(&ProxyRoute::System, environment, system);
        let envs = command_environment(&cmd);
        let https = envs
            .iter()
            .find(|(name, value)| name == "https_proxy" && value.is_some())
            .and_then(|(_, value)| value.clone());
        assert_eq!(https.as_deref(), expected, "{backdrop}");
        assert!(command_arguments(&cmd).is_empty(), "{backdrop}");
        assert_eq!(applied.proxied_for("https://www.bilibili.com/"), expected.is_some());
    }
}

#[test]
fn video_manual_route_overrides_every_backdrop() {
    for (backdrop, environment, system) in backdrops() {
        let (cmd, applied) = video_command(&ProxyRoute::Manual(MANUAL.into()), environment, system);
        let envs = command_environment(&cmd);
        for key in ["http_proxy", "https_proxy"] {
            assert!(
                envs.iter().any(|(name, value)| name == key && value.as_deref() == Some(MANUAL)),
                "{backdrop}: {key}"
            );
        }
        assert!(command_arguments(&cmd).is_empty(), "{backdrop}");
        assert!(applied.proxied_for("https://www.bilibili.com/"));
    }
}

#[test]
fn aria2_engine_never_inherits_proxy_variables() {
    let mut cmd = std::process::Command::new("aria2c");
    strip_keys(&mut cmd, vec!["http_proxy".into(), "ALL_PROXY".into()]);
    let removed: Vec<_> = cmd
        .get_envs()
        .filter(|(_, value)| value.is_none())
        .map(|(key, _)| key.to_string_lossy().into_owned())
        .collect();
    assert_eq!(removed, ["ALL_PROXY", "http_proxy"]);
}

#[test]
fn aria2_download_options_express_each_route_against_each_backdrop() {
    let environment = pairs(&[
        ("http", ENVIRONMENT),
        ("https", ENVIRONMENT),
        ("all", "socks5://environment.invalid:1080"),
        ("no", "localhost,.lan"),
    ]);
    for backdrop in [environment.clone(), Proxies::new()] {
        assert_eq!(
            aria2_options_from(&ProxyRoute::Direct, backdrop.clone()),
            [("all-proxy", String::new())]
        );
        assert_eq!(
            aria2_options_from(&ProxyRoute::Manual(MANUAL.into()), backdrop),
            [("all-proxy", MANUAL.to_owned())]
        );
    }
    assert_eq!(
        aria2_options_from(&ProxyRoute::System, environment),
        [
            ("http-proxy", ENVIRONMENT.to_owned()),
            ("https-proxy", ENVIRONMENT.to_owned()),
            ("all-proxy", "socks5://environment.invalid:1080".to_owned()),
            ("no-proxy", "localhost,.lan".to_owned()),
        ]
    );
    // aria2 itself ignores OS proxy settings; follow-system keeps that.
    assert!(aria2_options_from(&ProxyRoute::System, Proxies::new()).is_empty());
}

#[test]
fn the_same_rejection_names_the_proxy_only_when_one_was_used() {
    let rejection = "video.error.accessDenied";
    let page = "https://www.bilibili.com/video/BV1na1YBzELQ";
    let (_, proxied) = video_command(&ProxyRoute::System, pairs(&[("https", ENVIRONMENT)]), Proxies::new());
    let (_, unproxied) = video_command(&ProxyRoute::System, Proxies::new(), Proxies::new());
    assert_eq!(proxied.diagnose(rejection, page), "video.error.accessDeniedProxy");
    assert_eq!(unproxied.diagnose(rejection, page), rejection);
    assert_eq!(proxied.diagnose("video.error.network", page), "video.error.network");

    for (_, environment, system) in backdrops() {
        let (_, direct) = video_command(&ProxyRoute::Direct, environment, system);
        assert_eq!(direct.diagnose(rejection, page), rejection);
    }
}

#[test]
fn bypass_rules_exclude_matching_hosts_from_the_proxy_hint() {
    let (_, applied) = video_command(
        &ProxyRoute::System,
        pairs(&[("https", ENVIRONMENT), ("no", "localhost, .bilibili.com,example.org:443")]),
        Proxies::new(),
    );
    assert!(!applied.proxied_for("https://www.bilibili.com/video/BV1"));
    assert!(!applied.proxied_for("https://example.org/watch"));
    assert!(applied.proxied_for("https://youtube.com/watch"));
    let (_, everything) = video_command(
        &ProxyRoute::System,
        pairs(&[("https", ENVIRONMENT), ("no", "*")]),
        Proxies::new(),
    );
    assert!(!everything.proxied_for("https://youtube.com/watch"));
}
