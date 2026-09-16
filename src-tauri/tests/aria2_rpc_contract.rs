use alldownloader_lib::aria2::{rpc, types};
use axum::{http::StatusCode, routing::post, Json, Router};
use rpc::{Aria2Api, Aria2RpcClient};
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
    time::Duration,
};
use types::{Aria2Error, RpcSecret};

async fn fixture(
    reply: Value,
    status: StatusCode,
    delay: Duration,
) -> (String, Arc<Mutex<Vec<Value>>>, tokio::task::JoinHandle<()>) {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let recorded = calls.clone();
    let app = Router::new().route(
        "/jsonrpc",
        post(move |Json(body): Json<Value>| {
            let recorded = recorded.clone();
            let reply = reply.clone();
            async move {
                recorded.lock().unwrap().push(body);
                tokio::time::sleep(delay).await;
                (status, Json(reply))
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/jsonrpc", listener.local_addr().unwrap());
    let handle = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (url, calls, handle)
}

async fn escaped_response_fixture(body: String) -> (Aria2RpcClient, tokio::task::JoinHandle<()>) {
    let app = Router::new().route(
        "/jsonrpc",
        post(move || {
            let body = body.clone();
            async move { ([("content-type", "application/json")], body) }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let client = Aria2RpcClient::new(
        format!("http://{}/jsonrpc", listener.local_addr().unwrap()),
        RpcSecret::from_bytes([0xab; 32]),
    )
    .unwrap();
    let handle = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (client, handle)
}

#[tokio::test]
async fn escaped_rpc_fault_is_redacted_after_json_decoding() {
    let body = format!(
        r#"{{"jsonrpc":"2.0","id":"1","error":{{"code":1,"message":"denied token:{}"}}}}"#,
        r"\u0061b".repeat(32)
    );
    let (client, handle) = escaped_response_fixture(body).await;
    let error = client.pause("gid").await.unwrap_err();
    handle.abort();
    assert!(!format!("{error:?} {error}").contains(&"ab".repeat(32)));
    let Aria2Error::Rpc(fault) = error else {
        panic!("Expected an RPC fault");
    };
    assert_eq!(fault.message, "denied token:[REDACTED]");
}

#[tokio::test]
async fn escaped_multicall_fault_is_redacted_after_json_decoding() {
    let body = format!(
        r#"{{"jsonrpc":"2.0","id":"1","result":[{{"code":1,"message":"denied token:{}"}}]}}"#,
        r"a\u0062".repeat(32)
    );
    let (client, handle) = escaped_response_fixture(body).await;
    let result = client
        .multicall(vec![rpc::RpcCall::new(
            "aria2.tellStatus",
            vec![json!("gid")],
        )])
        .await
        .unwrap();
    handle.abort();
    assert!(!format!("{result:?}").contains(&"ab".repeat(32)));
    assert_eq!(
        result[0].as_ref().unwrap_err().message,
        "denied token:[REDACTED]"
    );
}

#[tokio::test]
async fn escaped_multicall_result_redacts_nested_arrays_values_and_object_keys() {
    let escaped = r"\u0061\u0062".repeat(32);
    let body = format!(
        r#"{{"jsonrpc":"2.0","id":"1","result":[[{{"outer":[{{"{escaped}":"token:{escaped}"}}]}}]]}}"#
    );
    let (client, handle) = escaped_response_fixture(body).await;
    let result = client
        .multicall(vec![rpc::RpcCall::new(
            "aria2.tellStatus",
            vec![json!("gid")],
        )])
        .await
        .unwrap();
    handle.abort();
    assert!(!format!("{result:?}").contains(&"ab".repeat(32)));
    assert_eq!(
        result[0].as_ref().unwrap(),
        &json!({"outer":[{"[REDACTED]":"token:[REDACTED]"}]})
    );
}

#[tokio::test]
async fn protected_call_prepends_secret_and_preserves_options() {
    let (url, calls, handle) = fixture(
        json!({"jsonrpc":"2.0","id":"1","result":"gid-1"}),
        StatusCode::OK,
        Duration::ZERO,
    )
    .await;
    let client = Aria2RpcClient::new(url, RpcSecret::from_bytes([0xab; 32])).unwrap();
    let gid = client
        .add_uri(
            vec!["http://127.0.0.1/file".into()],
            BTreeMap::from([("out".into(), "file.bin".into())]),
        )
        .await
        .unwrap();
    assert_eq!(gid, "gid-1");
    let request = calls.lock().unwrap()[0].clone();
    assert_eq!(request["method"], "aria2.addUri");
    assert_eq!(
        request["params"],
        json!([format!("token:{}", "ab".repeat(32)), ["http://127.0.0.1/file"], {"out":"file.bin"}])
    );
    handle.abort();
}

#[tokio::test]
async fn change_global_option_authenticates_and_sends_the_global_bandwidth_cap() {
    let (url, calls, handle) = fixture(
        json!({"jsonrpc":"2.0","id":"1","result":"OK"}),
        StatusCode::OK,
        Duration::ZERO,
    )
    .await;
    let client = Aria2RpcClient::new(url, RpcSecret::from_bytes([0xab; 32])).unwrap();

    client
        .change_global_option(BTreeMap::from([(
            "max-overall-upload-limit".into(),
            "1024".into(),
        )]))
        .await
        .unwrap();

    let request = calls.lock().unwrap()[0].clone();
    assert_eq!(request["method"], "aria2.changeGlobalOption");
    assert_eq!(
        request["params"],
        json!([format!("token:{}", "ab".repeat(32)), {"max-overall-upload-limit":"1024"}])
    );
    handle.abort();
}

#[tokio::test]
async fn get_peers_sends_the_gid_and_decodes_peer_addresses() {
    let (url, calls, handle) = fixture(
        json!({"jsonrpc":"2.0","id":"1","result":[
            {"ip":"203.0.113.10","port":"51413"},
            {"ip":"2001:db8::7","port":"51414"}
        ]}),
        StatusCode::OK,
        Duration::ZERO,
    )
    .await;
    let client = Aria2RpcClient::new(url, RpcSecret::from_bytes([0xab; 32])).unwrap();

    let peers = client.get_peers("bt-gid").await.unwrap();
    handle.abort();

    assert_eq!(peers.len(), 2);
    assert_eq!(peers[0].ip, "203.0.113.10");
    assert_eq!(peers[1].port, "51414");
    let calls = calls.lock().unwrap();
    assert_eq!(calls[0]["method"], "aria2.getPeers");
    assert_eq!(calls[0]["params"][1], "bt-gid");
    assert!(calls[0]["params"][0]
        .as_str()
        .unwrap()
        .starts_with("token:"));
}

#[tokio::test]
async fn response_failures_are_typed() {
    for (reply, status, expected) in [
        (
            json!({"jsonrpc":"2.0","id":"1","error":{"code":1,"message":"denied"}}),
            StatusCode::OK,
            "rpc",
        ),
        (
            json!({"jsonrpc":"2.0","id":"2","result":"OK"}),
            StatusCode::OK,
            "id",
        ),
        (
            json!({"jsonrpc":"1.0","id":"1","result":"OK"}),
            StatusCode::OK,
            "malformed",
        ),
        (
            json!({"jsonrpc":"2.0","id":"1","result":25}),
            StatusCode::OK,
            "malformed",
        ),
        (
            json!({"jsonrpc":"2.0","id":"1","result":"OK","error":{"code":1,"message":"bad"}}),
            StatusCode::OK,
            "malformed",
        ),
        (json!({}), StatusCode::BAD_GATEWAY, "http"),
    ] {
        let (url, _, handle) = fixture(reply, status, Duration::ZERO).await;
        let error = Aria2RpcClient::new(url, RpcSecret::from_bytes([1; 32]))
            .unwrap()
            .pause("gid")
            .await
            .unwrap_err();
        assert!(match expected {
            "rpc" => matches!(error, Aria2Error::Rpc(_)),
            "id" => matches!(error, Aria2Error::MismatchedResponseId),
            "http" => matches!(error, Aria2Error::HttpStatus(502)),
            _ => matches!(error, Aria2Error::MalformedResponse),
        });
        handle.abort();
    }
}

#[tokio::test]
async fn timeout_is_bounded_and_errors_do_not_echo_secrets() {
    let (url, _, handle) = fixture(json!({}), StatusCode::OK, Duration::from_secs(2)).await;
    let client = Aria2RpcClient::with_timeout(
        url,
        RpcSecret::from_bytes([0xab; 32]),
        Duration::from_millis(30),
    )
    .unwrap();
    assert!(matches!(
        client.pause("gid").await.unwrap_err(),
        Aria2Error::Timeout
    ));
    handle.abort();
    let (url, _, handle) = fixture(json!({"jsonrpc":"2.0","id":"1","error":{"code":1,"message":format!("token:{}", "ab".repeat(32))}}), StatusCode::OK, Duration::ZERO).await;
    let error = Aria2RpcClient::new(url, RpcSecret::from_bytes([0xab; 32]))
        .unwrap()
        .pause("gid")
        .await
        .unwrap_err();
    assert!(!format!("{error:?} {error}").contains(&"ab".repeat(32)));
    handle.abort();
}

#[test]
fn rejects_non_loopback_and_credential_endpoints_before_io() {
    for url in [
        "http://example.org/jsonrpc",
        "http://192.168.1.2/jsonrpc",
        "http://user:pass@127.0.0.1/jsonrpc",
        "http://127.0.0.1/jsonrpc?secret=x",
        "https://127.0.0.1/jsonrpc",
    ] {
        assert!(Aria2RpcClient::new(url, RpcSecret::from_bytes([1; 32])).is_err());
    }
}

#[tokio::test]
async fn multicall_authenticates_each_nested_call_and_preserves_faults() {
    let (url, calls, handle) = fixture(
        json!({"jsonrpc":"2.0","id":"1","result":[[{"gid":"g"}],{"code":1,"message":"missing"}]}),
        StatusCode::OK,
        Duration::ZERO,
    )
    .await;
    let client = Aria2RpcClient::new(url, RpcSecret::from_bytes([0xab; 32])).unwrap();
    let result = client
        .multicall(vec![
            rpc::RpcCall::new("aria2.tellStatus", vec![json!("g")]),
            rpc::RpcCall::new("aria2.tellStatus", vec![json!("missing")]),
        ])
        .await
        .unwrap();
    assert!(result[0].is_ok());
    assert!(result[1].is_err());
    let request = calls.lock().unwrap()[0].clone();
    assert_eq!(request["method"], "system.multicall");
    assert_eq!(request["params"].as_array().unwrap().len(), 1);
    assert_eq!(
        request["params"][0][0]["params"][0],
        format!("token:{}", "ab".repeat(32))
    );
    assert_eq!(
        request["params"][0][1]["params"][0],
        format!("token:{}", "ab".repeat(32))
    );
    handle.abort();
}

#[tokio::test]
async fn every_wrapper_uses_the_documented_method_and_parameter_order() {
    let calls = Arc::new(Mutex::new(Vec::<Value>::new()));
    let recorded = calls.clone();
    let app = Router::new().route("/jsonrpc", post(move |Json(body): Json<Value>| {
        let recorded = recorded.clone();
        async move {
            recorded.lock().unwrap().push(body.clone());
            let result = match body["method"].as_str().unwrap() {
                "aria2.tellStatus" => json!({"gid":"g","status":"paused","totalLength":"0","completedLength":"0","downloadSpeed":"0","uploadSpeed":"0","connections":"0","files":[]}),
                "aria2.tellActive" | "aria2.tellWaiting" | "aria2.tellStopped" => json!([]),
                "aria2.getGlobalStat" => json!({"downloadSpeed":"1","uploadSpeed":"0","numActive":"1","numWaiting":"0","numStopped":"0","numStoppedTotal":"0"}),
                "aria2.getOption" => json!({"dir":"downloads"}),
                _ => json!("OK"),
            };
            Json(json!({"jsonrpc":"2.0","id":body["id"],"result":result}))
        }
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let client = Aria2RpcClient::new(
        format!("http://{}/jsonrpc", listener.local_addr().unwrap()),
        RpcSecret::from_bytes([1; 32]),
    )
    .unwrap();
    let handle = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    client
        .add_torrent("dG9ycmVudA==".into(), vec![], BTreeMap::new())
        .await
        .unwrap();
    client.tell_status("g").await.unwrap();
    client.tell_active().await.unwrap();
    client.tell_waiting(-1, 10).await.unwrap();
    client.tell_stopped(0, 20).await.unwrap();
    assert_eq!(client.get_global_stat().await.unwrap().download_speed, 1);
    client.pause("g").await.unwrap();
    client.unpause("g").await.unwrap();
    client.remove("g").await.unwrap();
    client.force_remove("g").await.unwrap();
    client
        .change_option("g", BTreeMap::from([("select-file".into(), "1,3".into())]))
        .await
        .unwrap();
    assert_eq!(client.get_option("g").await.unwrap()["dir"], "downloads");
    client.save_session().await.unwrap();
    client.shutdown().await.unwrap();
    let expected = [
        ("aria2.addTorrent", json!(["dG9ycmVudA==", [], {}])),
        ("aria2.tellStatus", json!(["g"])),
        ("aria2.tellActive", json!([])),
        ("aria2.tellWaiting", json!([-1, 10])),
        ("aria2.tellStopped", json!([0, 20])),
        ("aria2.getGlobalStat", json!([])),
        ("aria2.pause", json!(["g"])),
        ("aria2.unpause", json!(["g"])),
        ("aria2.remove", json!(["g"])),
        ("aria2.forceRemove", json!(["g"])),
        ("aria2.changeOption", json!(["g",{"select-file":"1,3"}])),
        ("aria2.getOption", json!(["g"])),
        ("aria2.saveSession", json!([])),
        ("aria2.shutdown", json!([])),
    ];
    for (index, (request, (method, params))) in
        calls.lock().unwrap().iter().zip(expected).enumerate()
    {
        assert_eq!(request["method"], method);
        assert_eq!(request["id"], (index + 1).to_string());
        assert_eq!(request["params"][0], format!("token:{}", "01".repeat(32)));
        assert_eq!(
            &request["params"].as_array().unwrap()[1..],
            params.as_array().unwrap()
        );
    }
    handle.abort();
}
