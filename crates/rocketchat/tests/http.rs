//! REST client behaviour against in-process HTTP servers: redirects, timeouts,
//! error envelopes.

#![allow(clippy::unwrap_used)]

mod common;

use std::time::{Duration, Instant};

use common::{body_json, http_response, joined, json_ok, serve, serve_noread};
use rocketchat::rest::RestConfig;
use rocketchat::rest::{HistoryCursor, UploadSource};
use rocketchat::{Credentials, Error, RestClient, ServerUrl, TlsConfig};
use serde_json::{Value, json};

fn client_with(base: &str, config: RestConfig) -> RestClient {
    let tls = TlsConfig::new().build().unwrap();
    let c = RestClient::with_config(ServerUrl::parse(base).unwrap(), &tls, config).unwrap();
    c.set_credentials(Some(Credentials::new("UID123", "SECRET-AUTH-TOKEN")));
    c
}

fn client(base: &str) -> RestClient {
    client_with(base, RestConfig::default())
}

fn redirect_to(status: &'static str, location: String) -> impl Fn(&str) -> Option<String> {
    move |_| Some(http_response(status, &[("Location", &location)], ""))
}

#[tokio::test]
async fn api_cross_origin_redirect_is_refused() {
    let (pb, log_b) = serve(|_| Some(json_ok(r#"{"_id":"x","success":true}"#))).await;
    let (pa, _) = serve(redirect_to(
        "307 Temporary Redirect",
        format!("http://localhost:{pb}/api/v1/me"),
    ))
    .await;
    let c = client(&format!("http://127.0.0.1:{pa}"));
    match c.me().await {
        Err(Error::Redirect {
            location,
            cross_origin: true,
        }) => assert!(location.contains(&pb.to_string())),
        other => panic!("{other:?}"),
    }
    assert!(
        log_b.lock().unwrap().is_empty(),
        "foreign host was contacted"
    );
}

#[tokio::test]
async fn login_body_is_not_reposted_cross_origin() {
    let (pb, log_b) = serve(|_| Some(json_ok("{}"))).await;
    let (pa, _) = serve(redirect_to(
        "307 Temporary Redirect",
        format!("http://localhost:{pb}/api/v1/login"),
    ))
    .await;
    let tls = TlsConfig::new().build().unwrap();
    let c = RestClient::new(
        ServerUrl::parse(&format!("http://127.0.0.1:{pa}")).unwrap(),
        &tls,
    )
    .unwrap();
    let e = c
        .login_password("alice", "alicepass", None)
        .await
        .unwrap_err();
    assert!(matches!(e, Error::Redirect { .. }), "{e:?}");
    assert!(log_b.lock().unwrap().is_empty());
}

#[tokio::test]
async fn same_origin_redirect_keeps_auth() {
    let (pa, log) = serve(|req| {
        let authed = req
            .to_ascii_lowercase()
            .contains("x-auth-token: secret-auth-token");
        if req.starts_with("GET /api/v1/me ") {
            Some(http_response(
                "302 Found",
                &[("Location", "/api/v1/me2")],
                "",
            ))
        } else if req.starts_with("GET /api/v1/me2 ") && authed {
            Some(json_ok(
                r#"{"_id":"UID123","username":"alice","success":true}"#,
            ))
        } else {
            Some(http_response("401 Unauthorized", &[], ""))
        }
    })
    .await;
    let c = client(&format!("http://127.0.0.1:{pa}"));
    let me = c.me().await.unwrap();
    assert_eq!(me.username.as_deref(), Some("alice"));
    assert_eq!(log.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn download_redirect_to_foreign_storage_drops_credentials() {
    let (pb, log_b) = serve(|_| {
        Some(http_response(
            "200 OK",
            &[("Content-Type", "image/png")],
            "ok",
        ))
    })
    .await;
    let (pa, log_a) = serve(redirect_to(
        "302 Found",
        format!("http://localhost:{pb}/bucket/x.png?X-Amz-Signature=abc"),
    ))
    .await;
    let c = client(&format!("http://127.0.0.1:{pa}"));
    let d = c.download("/file-upload/abc/x.png").await.unwrap();
    assert_eq!(d.content_type.as_deref(), Some("image/png"));
    assert_eq!(&d.bytes().await.unwrap()[..], b"ok");
    assert!(joined(&log_a).contains("x-auth-token: secret-auth-token"));
    // `/avatar/` only reads the credentials from cookies.
    assert!(
        joined(&log_a).contains("cookie: rc_uid=uid123; rc_token=secret-auth-token"),
        "{}",
        joined(&log_a)
    );
    let b = joined(&log_b);
    assert!(b.contains("x-amz-signature=abc"));
    for h in [
        "x-auth-token",
        "x-user-id",
        "cookie",
        "referer",
        "authorization",
    ] {
        assert!(!b.contains(h), "{h} sent to foreign storage:\n{b}");
    }
}

#[tokio::test]
async fn foreign_download_never_gets_credentials() {
    let (pb, log_b) = serve(|_| Some(http_response("200 OK", &[], "img"))).await;
    let c = client("http://127.0.0.1:9");
    let d = c
        .download(&format!("http://localhost:{pb}/preview.png"))
        .await
        .unwrap();
    assert_eq!(&d.bytes().await.unwrap()[..], b"img");
    assert!(!joined(&log_b).contains("x-auth-token"));
}

#[tokio::test]
async fn same_origin_non_asset_download_is_anonymous() {
    let (pa, log) = serve(|_| Some(http_response("200 OK", &[], "x"))).await;
    let c = client(&format!("http://127.0.0.1:{pa}"));
    for path in [
        "/api/v1/logout",
        "/api/apps/private/app/hash/act",
        "/file-upload/../api/v1/logout",
    ] {
        c.download(path).await.unwrap();
        c.download(&format!("http://127.0.0.1:{pa}{path}"))
            .await
            .unwrap();
    }
    let sent = joined(&log);
    assert!(sent.contains("get /api/v1/logout"), "{sent}");
    for h in ["x-auth-token", "x-user-id", "cookie"] {
        assert!(!sent.contains(h), "{h} sent to a non-asset path:\n{sent}");
    }
    log.lock().unwrap().clear();
    c.download("/avatar/alice?size=64").await.unwrap();
    assert!(joined(&log).contains("rc_token=secret-auth-token"));
}

#[tokio::test]
async fn hung_server_times_out() {
    let (pa, _) = serve(|_| None).await;
    let c = client_with(
        &format!("http://127.0.0.1:{pa}"),
        RestConfig {
            api_timeout: Duration::from_secs(1),
            read_timeout: Duration::from_millis(800),
            ..RestConfig::default()
        },
    );
    let start = Instant::now();
    let e = c.me().await.unwrap_err();
    assert!(start.elapsed() < Duration::from_secs(5));
    assert!(matches!(&e, Error::Http(h) if h.is_timeout()), "{e:?}");
    assert!(e.is_transient());
    let start = Instant::now();
    let e = c.download("/file-upload/x").await.unwrap_err();
    assert!(
        start.elapsed() < Duration::from_secs(5),
        "download read timeout"
    );
    assert!(matches!(&e, Error::Http(h) if h.is_timeout()), "{e:?}");
}

#[tokio::test]
async fn method_call_http_errors() {
    let (pa, _) = serve(|_| {
        Some(http_response(
            "500 Internal Server Error",
            &[("Content-Type", "application/json")],
            r#"{"success":false,"message":"404"}"#,
        ))
    })
    .await;
    let c = client(&format!("http://127.0.0.1:{pa}"));
    let r = c
        .method_call_raw("loadHistory", serde_json::json!([]), false)
        .await;
    assert!(r.is_err(), "{r:?}");

    let (pa, _) = serve(|_| {
        Some(json_ok(
            r#"{"success":true,"message":"{\"msg\":\"result\",\"id\":\"1\",\"result\":false}"}"#,
        ))
    })
    .await;
    let c = client(&format!("http://127.0.0.1:{pa}"));
    let e = c.load_history("nope", None, 10, None).await.unwrap_err();
    assert_eq!(e.api_error_code(), Some("error-not-allowed"));
}

#[tokio::test]
async fn http_200_with_success_false() {
    let (pa, _) = serve(|_| {
        Some(json_ok(
            r#"{"success":false,"error":"no [error-x]","errorType":"error-x"}"#,
        ))
    })
    .await;
    let c = client(&format!("http://127.0.0.1:{pa}"));
    assert_eq!(c.me().await.unwrap_err().api_error_code(), Some("error-x"));
}

#[tokio::test]
async fn redirect_loops_and_protocol_relative() {
    let (pb, log_b) = serve(|_| Some(json_ok("{}"))).await;
    let (pa, _) = serve(move |req| {
        if req.starts_with("GET /api/v1/me ") {
            Some(http_response(
                "302 Found",
                &[("Location", &format!("//localhost:{pb}/api/v1/me"))],
                "",
            ))
        } else {
            Some(http_response(
                "302 Found",
                &[("Location", "/api/v1/loop")],
                "",
            ))
        }
    })
    .await;
    let c = client(&format!("http://127.0.0.1:{pa}"));
    let e = c.me().await.unwrap_err();
    assert!(
        matches!(
            e,
            Error::Redirect {
                cross_origin: true,
                ..
            }
        ),
        "{e:?}"
    );
    assert!(log_b.lock().unwrap().is_empty());
    let e = c
        .get::<Value>("loop", &rocketchat::rest::Query::new())
        .await
        .unwrap_err();
    assert!(
        matches!(
            e,
            Error::Redirect {
                cross_origin: false,
                ..
            }
        ),
        "{e:?}"
    );
    assert!(e.to_string().contains("loop"));
}

fn slow(ms: u64, body: &'static str) -> impl Fn(&str) -> Option<String> {
    move |req| {
        std::thread::sleep(Duration::from_millis(ms));
        if req.starts_with("POST /api/v1/rooms.media/") {
            Some(json_ok(
                r#"{"file":{"_id":"F","url":"/file-upload/F/x"},"success":true}"#,
            ))
        } else {
            Some(json_ok(body))
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn read_timeout_does_not_cap_api_calls() {
    let (pa, _) = serve(slow(2000, r#"{"_id":"u","success":true}"#)).await;
    let c = client_with(
        &format!("http://127.0.0.1:{pa}"),
        RestConfig {
            api_timeout: Duration::from_secs(10),
            read_timeout: Duration::from_secs(1),
            ..RestConfig::default()
        },
    );
    assert_eq!(c.me().await.unwrap().id, "u");
}

#[tokio::test(flavor = "multi_thread")]
async fn transfers_are_not_bound_by_api_timeout() {
    let (pa, _) = serve(slow(2500, "ok")).await;
    let c = client_with(
        &format!("http://127.0.0.1:{pa}"),
        RestConfig {
            api_timeout: Duration::from_secs(1),
            read_timeout: Duration::from_secs(5),
            ..RestConfig::default()
        },
    );
    let up = c
        .upload_media(
            "R",
            "x.png".into(),
            "image/png".into(),
            UploadSource::Bytes(vec![0u8; 300_000].into()),
            None,
        )
        .await
        .unwrap();
    assert_eq!(up.id, "F");
    let d = c.download("/file-upload/F/x").await.unwrap();
    assert_eq!(&d.bytes().await.unwrap()[..], b"ok");
}

#[tokio::test]
async fn upload_without_response_times_out() {
    let (pa, log) = serve(|_| None).await;
    let c = client_with(
        &format!("http://127.0.0.1:{pa}"),
        RestConfig {
            upload_response_timeout: Duration::from_secs(1),
            ..RestConfig::default()
        },
    );
    let start = Instant::now();
    let e = c
        .upload_media(
            "R",
            "x.txt".into(),
            "text/plain".into(),
            UploadSource::Bytes(vec![b'a'; 20_000].into()),
            None,
        )
        .await
        .unwrap_err();
    assert!(
        matches!(e, Error::Timeout("no response after upload")),
        "{e:?}"
    );
    assert!(start.elapsed() < Duration::from_secs(5));
    assert!(log.lock().unwrap()[0].contains("x.txt"));
}

#[tokio::test]
async fn stalled_upload_times_out() {
    let pa = serve_noread().await;
    let c = client_with(
        &format!("http://127.0.0.1:{pa}"),
        RestConfig {
            upload_stall_timeout: Duration::from_secs(1),
            ..RestConfig::default()
        },
    );
    let start = Instant::now();
    let e = c
        .upload_media(
            "R",
            "big.bin".into(),
            "application/octet-stream".into(),
            UploadSource::Bytes(vec![0u8; 64 << 20].into()),
            None,
        )
        .await
        .unwrap_err();
    assert!(matches!(e, Error::Timeout("upload stalled")), "{e:?}");
    assert!(start.elapsed() < Duration::from_secs(10));
}

/// Fake `loadHistory`: messages `(id, ts_ms)`, `end` exclusive, newest first.
fn history_server(msgs: Vec<(String, i64)>) -> impl Fn(&str) -> Option<String> {
    move |req| {
        let body = body_json(req);
        let frame: Value = serde_json::from_str(body["message"].as_str().unwrap()).unwrap();
        let p = &frame["params"];
        let end = p[1]["$date"].as_i64().unwrap_or(i64::MAX);
        let limit = p[2].as_u64().unwrap() as usize;
        let mut page: Vec<&(String, i64)> = msgs.iter().filter(|(_, ts)| *ts < end).collect();
        page.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        let out: Vec<Value> = page
            .into_iter()
            .take(limit)
            .map(|(id, ts)| json!({"_id": id, "rid": "R", "msg": id, "ts": {"$date": ts}, "u": {"_id": "x"}}))
            .collect();
        let inner = json!({"msg": "result", "id": frame["id"], "result": {"messages": out}});
        Some(json_ok(
            &json!({"message": inner.to_string(), "success": true}).to_string(),
        ))
    }
}

async fn collect_all(c: &RestClient, limit: u32) -> Vec<String> {
    let mut cursor = HistoryCursor::new("R", limit);
    let mut ids = Vec::new();
    for _ in 0..200 {
        let page = cursor.next_page(c).await.unwrap();
        if page.is_empty() {
            assert!(cursor.is_done());
            break;
        }
        ids.extend(page.into_iter().map(|m| m.id));
    }
    ids
}

#[tokio::test]
async fn history_cursor_handles_same_millisecond_runs() {
    let mut msgs: Vec<(String, i64)> = (0..50)
        .map(|i| (format!("new{i:03}"), 10_000 + i * 10))
        .collect();
    msgs.extend((0..60).map(|i| (format!("same{i:03}"), 5_000)));
    msgs.extend((0..5).map(|i| (format!("old{i}"), 1_000 + i)));
    let total = msgs.len();
    let (pa, _) = serve(history_server(msgs)).await;
    let c = client(&format!("http://127.0.0.1:{pa}"));
    let ids = collect_all(&c, 20).await;
    let unique: std::collections::HashSet<_> = ids.iter().collect();
    assert_eq!(unique.len(), ids.len(), "no duplicates");
    assert_eq!(ids.len(), total, "nothing skipped");
    assert_eq!(ids.last().unwrap(), "old0");
}

#[tokio::test]
async fn history_cursor_steps_past_huge_same_millisecond_runs() {
    let mut msgs: Vec<(String, i64)> = (0..600).map(|i| (format!("same{i:03}"), 5_000)).collect();
    msgs.extend((0..3).map(|i| (format!("old{i}"), 1_000 + i)));
    let (pa, _) = serve(history_server(msgs)).await;
    let c = client(&format!("http://127.0.0.1:{pa}"));
    let ids = collect_all(&c, 20).await;
    assert!(ids.iter().any(|i| i == "old0"), "reached older messages");
    assert!(ids.len() >= 403, "{}", ids.len());
}

#[tokio::test]
async fn set_avatar_sends_multipart_image() {
    let (pa, log) = serve(|_| Some(json_ok(r#"{"success":true}"#))).await;
    let c = client(&format!("http://127.0.0.1:{pa}"));
    c.users_set_avatar("me.png", "image/png", b"\x89PNG-bytes".to_vec())
        .await
        .unwrap();
    // `joined` lowercases the log.
    let req = joined(&log);
    assert!(req.starts_with("post /api/v1/users.setavatar "), "{req}");
    assert!(req.contains("multipart/form-data; boundary="));
    assert!(req.contains(r#"name="image"; filename="me.png""#), "{req}");
    assert!(req.contains("content-type: image/png"));
    assert!(req.contains("png-bytes"));
    assert!(req.contains("secret-auth-token"));

    c.users_reset_avatar("UID123").await.unwrap();
    let req = log.lock().unwrap().last().cloned().unwrap();
    assert!(req.starts_with("POST /api/v1/users.resetAvatar "));
    assert_eq!(body_json(&req), json!({"userId": "UID123"}));
}
