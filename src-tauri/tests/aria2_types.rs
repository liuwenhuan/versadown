use alldownloader_lib::aria2::types::*;
use alldownloader_lib::TaskStatus;
use serde_json::json;

fn raw() -> serde_json::Value {
    json!({"gid":"abc","status":"active","totalLength":"1000","completedLength":"250",
        "downloadSpeed":"50","uploadSpeed":"0","connections":"2","files":[]})
}

#[test]
fn parses_checked_lengths_progress_and_eta() {
    let snapshot =
        TransferSnapshot::try_from(serde_json::from_value::<Aria2Status>(raw()).unwrap()).unwrap();
    assert_eq!(snapshot.progress_percent, Some(25));
    assert_eq!(snapshot.eta_seconds, Some(15));
    assert_eq!(snapshot.status, TaskStatus::Downloading);
}

#[test]
fn malformed_and_overflowing_numbers_are_errors() {
    for field in [
        "totalLength",
        "completedLength",
        "downloadSpeed",
        "uploadSpeed",
        "connections",
    ] {
        for value in ["not-a-number", "-1", "18446744073709551616", "+1", " 1"] {
            let mut value_json = raw();
            value_json[field] = json!(value);
            assert!(TransferSnapshot::try_from(
                serde_json::from_value::<Aria2Status>(value_json).unwrap()
            )
            .is_err());
        }
    }
}

#[test]
fn unknown_total_and_zero_speed_do_not_invent_estimates() {
    let mut value = raw();
    value["totalLength"] = json!("0");
    let snapshot =
        TransferSnapshot::try_from(serde_json::from_value::<Aria2Status>(value.clone()).unwrap())
            .unwrap();
    assert_eq!(snapshot.progress_percent, None);
    assert_eq!(snapshot.eta_seconds, None);
    value["totalLength"] = json!("1000");
    value["downloadSpeed"] = json!("0");
    assert_eq!(
        TransferSnapshot::try_from(serde_json::from_value::<Aria2Status>(value).unwrap())
            .unwrap()
            .eta_seconds,
        None
    );
}

#[test]
fn status_mapping_is_exact() {
    for (wire, expected) in [
        ("waiting", TaskStatus::Queued),
        ("active", TaskStatus::Downloading),
        ("paused", TaskStatus::Paused),
        ("complete", TaskStatus::Processing),
        ("error", TaskStatus::Failed),
        ("removed", TaskStatus::Cancelled),
    ] {
        assert_eq!(map_aria2_status(wire).unwrap(), expected);
    }
    assert!(map_aria2_status("unknown").is_err());
}

#[test]
fn parses_files_followed_by_and_error_without_losing_fields() {
    let mut value = raw();
    value["files"] = json!([{"index":"1","path":"C:/downloads/a.bin","length":"1000","completedLength":"250","selected":"true","uris":[]}]);
    value["followedBy"] = json!(["next-gid"]);
    value["errorCode"] = json!("3");
    value["errorMessage"] = json!("not found");
    let snapshot =
        TransferSnapshot::try_from(serde_json::from_value::<Aria2Status>(value.clone()).unwrap())
            .unwrap();
    assert_eq!(snapshot.files[0].index, 1);
    assert!(snapshot.files[0].selected);
    assert_eq!(snapshot.followed_by, ["next-gid"]);
    assert_eq!(snapshot.error_code.as_deref(), Some("3"));
    value["files"][0]["selected"] = json!("yes");
    assert!(
        TransferSnapshot::try_from(serde_json::from_value::<Aria2Status>(value).unwrap()).is_err()
    );
}

#[test]
fn max_lengths_do_not_overflow_progress_math() {
    let mut value = raw();
    value["totalLength"] = json!(u64::MAX.to_string());
    value["completedLength"] = json!(u64::MAX.to_string());
    assert_eq!(
        TransferSnapshot::try_from(serde_json::from_value::<Aria2Status>(value).unwrap())
            .unwrap()
            .progress_percent,
        Some(100)
    );
}

#[test]
fn secret_debug_is_redacted() {
    let secret = RpcSecret::from_bytes([0xab; 32]);
    assert_eq!(
        secret.authorization_param(),
        Some(format!("token:{}", "ab".repeat(32)))
    );
    assert_eq!(format!("{secret:?}"), "RpcSecret([REDACTED])");
}

#[test]
fn global_statistics_reject_invalid_wire_numbers() {
    let mut value = json!({"downloadSpeed":"50","uploadSpeed":"0","numActive":"2","numWaiting":"3","numStopped":"4","numStoppedTotal":"5"});
    let stat: Aria2GlobalStat = serde_json::from_value(value.clone()).unwrap();
    assert_eq!(
        (stat.download_speed, stat.num_active, stat.num_stopped_total),
        (50, 2, 5)
    );
    value["numActive"] = json!("bad");
    assert!(serde_json::from_value::<Aria2GlobalStat>(value).is_err());
}
