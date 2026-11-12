//! 真的二进制：不需要网络的子命令，以及清单经宿主的校验和调用。

use std::path::PathBuf;
use std::process::Command;

use serde_json::Value;
use usuyo_plugin::protocol::{Access, Account, Canonicalized, ErrorCode, Identity, Login};
use usuyo_plugin::{
    CallLimits, CallOptions, FetchInput, FetchRequest, HostConfig, PluginError, PluginHost,
    discover,
};

const BIN: &str = env!("CARGO_BIN_EXE_usuyo-fetch-wechat-mp");
const ID: &str = "tr38EieqyftubIx2FMWUWw";

/// 清空环境后运行，和宿主一样。
fn run(args: &[&str]) -> (i32, Value) {
    let output = Command::new(BIN).args(args).env_clear().output().unwrap();
    let stdout = String::from_utf8(output.stdout).unwrap();
    let json = if stdout.trim().is_empty() {
        Value::Null
    } else {
        serde_json::from_str(&stdout).unwrap()
    };
    (output.status.code().unwrap(), json)
}

#[test]
fn canonicalize_needs_no_network() {
    let (code, out) = run(&[
        "canonicalize",
        &format!("https://mp.weixin.qq.com/s/{ID}?scene=1&chksm=ab#wechat_redirect"),
    ]);
    assert_eq!(code, 0);
    assert_eq!(out["ok"], true);
    assert_eq!(out["match"], true);
    assert_eq!(out["identity"]["platform"], "wechat-mp");
    assert_eq!(out["identity"]["contentId"], ID);
    assert_eq!(
        out["identity"]["url"],
        format!("https://mp.weixin.qq.com/s/{ID}")
    );
    assert!(out.get("resolvedUrl").is_none());

    let (code, out) = run(&[
        "canonicalize",
        "https://mp.weixin.qq.com/s?__biz=MzI4NjAxNjY4Nw==&mid=2650242275&idx=1&sn=522b4a6728fcde7b55e73a5568e2c028&chksm=1&scene=21#wechat_redirect",
    ]);
    assert_eq!(code, 0);
    assert_eq!(
        out["identity"]["contentId"],
        "MzI4NjAxNjY4Nw==:2650242275:1"
    );
    assert_eq!(
        out["identity"]["url"],
        "https://mp.weixin.qq.com/s?__biz=MzI4NjAxNjY4Nw%3D%3D&mid=2650242275&idx=1&sn=522b4a6728fcde7b55e73a5568e2c028"
    );

    let (code, out) = run(&[
        "canonicalize",
        "https://example.com/s/tr38EieqyftubIx2FMWUWw",
    ]);
    assert_eq!((code, &out["match"]), (0, &Value::Bool(false)));
}

#[test]
fn health_does_not_touch_the_network() {
    for args in [&["health"][..], &["health", "--probe"][..]] {
        let (code, out) = run(args);
        assert_eq!(code, 0);
        assert_eq!(
            (out["ready"].as_bool(), out["protocol"].as_u64()),
            (Some(true), Some(1))
        );
        assert_eq!(out["version"], env!("CARGO_PKG_VERSION"));
        assert_eq!(out["login"], serde_json::json!([]));
        assert_eq!(out["checks"], serde_json::json!([]));
    }
}

#[test]
fn bad_usage_exits_2() {
    for args in [
        &[][..],
        &["nope"][..],
        &["fetch", "x"][..],
        &["fetch", "x", "--out", "/o"][..],
        &["sources"][..],
        &["canonicalize"][..],
    ] {
        let (code, out) = run(args);
        assert_eq!((code, out), (2, Value::Null), "{args:?}");
    }
}

#[test]
fn each_call_goes_to_its_method() {
    // 方法和调用对不上的回 invalid-args，不发请求
    let link = format!("https://mp.weixin.qq.com/s/{ID}");
    for args in [
        &["sources", "--method", "article"][..],
        &[
            "list",
            "wechat-mp:favorites",
            "--method",
            "favorites-iphone",
        ][..],
        &[
            "fetch",
            &link,
            "--out",
            "/o",
            "--method",
            "favorites-iphone",
        ][..],
        &["fetch", &link, "--out", "/o", "--method", "favorites-mac"][..],
        &["fetch", &link, "--out", "/o", "--method", "nope"][..],
    ] {
        let (code, out) = run(args);
        assert_eq!(
            (code, &out["error"]["code"]),
            (1, &Value::from("invalid-args")),
            "{args:?}"
        );
    }
    // 不是公众号的链接：不发请求
    let tmp = tempfile::tempdir().unwrap();
    let (code, out) = run(&[
        "fetch",
        "https://example.com/x",
        "--out",
        tmp.path().to_str().unwrap(),
        "--method",
        "article",
    ]);
    assert_eq!(code, 1);
    assert_eq!(out["error"]["code"], "unsupported-input");
}

/// 清单装成宿主认的样子：`<root>/usuyo-fetch/wechat-mp/<version>/`。
fn host() -> (tempfile::TempDir, PluginHost) {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("plugins/usuyo-fetch/wechat-mp/0.1.0");
    std::fs::create_dir_all(&dir).unwrap();
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("usuyo-plugin.yaml");
    std::fs::copy(manifest, dir.join("usuyo-plugin.yaml")).unwrap();
    std::fs::copy(BIN, dir.join("usuyo-fetch-wechat-mp")).unwrap();
    let found = discover(&tmp.path().join("plugins")).unwrap();
    assert!(found.rejected.is_empty(), "{:?}", found.rejected);
    let host = PluginHost::new(
        HostConfig {
            work_dir: tmp.path().join("work"),
            cache_dir: tmp.path().join("cache"),
            locale: "zh-CN".into(),
            limits: CallLimits::default(),
        },
        found.fetch,
    );
    (tmp, host)
}

#[tokio::test]
async fn the_manifest_lists_every_row_of_routes_and_matches_both_link_forms() {
    let (_tmp, host) = host();
    let plugin = &host.plugins()[0];
    assert_eq!(plugin.id().as_str(), "usuyo-fetch/wechat-mp");
    assert_eq!(plugin.header.version.to_string(), env!("CARGO_PKG_VERSION"));
    let spec = &plugin.spec;
    // routes.md 第 13 节的每一行（`usuyo-fetch-hook/wechat-mp` 那行在另一个组织），顺序照清单
    let ids: Vec<&str> = spec.methods.iter().map(|m| m.id.as_str()).collect();
    assert_eq!(ids, ["article", "favorites-iphone", "favorites-mac"]);
    let article = spec.method("article").unwrap();
    assert_eq!(
        (article.access, article.login, article.account),
        (Access::PublicWeb, Login::None, Account::None)
    );
    assert!(article.built && article.sites.is_empty() && article.capabilities.fetch.is_some());
    assert!(article.on_by_default());
    for (id, account) in [
        ("favorites-iphone", Account::OwnPhone),
        ("favorites-mac", Account::OwnApp),
    ] {
        let m = spec.method(id).unwrap();
        assert!(!m.built, "{id}");
        assert_eq!((m.access, m.account), (Access::VisionUi, account), "{id}");
    }

    for hit in [
        format!("https://mp.weixin.qq.com/s/{ID}"),
        "http://mp.weixin.qq.com/s?__biz=MzI4NjAxNjY4Nw==&mid=2650242275&idx=1&sn=522b#wechat_redirect"
            .to_owned(),
    ] {
        let url = url::Url::parse(&hit).unwrap();
        assert_eq!(host.candidates(&url).len(), 1, "{hit}");
    }
    for miss in [
        "https://mp.weixin.qq.com/mp/profile_ext?action=home",
        "https://weixin.qq.com/s/tr38EieqyftubIx2FMWUWw",
    ] {
        let url = url::Url::parse(miss).unwrap();
        assert!(host.candidates(&url).is_empty(), "{miss}");
    }

    // 经宿主调一次：输出要过宿主的 schema 校验
    let url = url::Url::parse(&format!("https://mp.weixin.qq.com/s/{ID}")).unwrap();
    let canonical = host
        .canonicalize(host.candidates(&url)[0], &url, &CallOptions::default())
        .await
        .unwrap();
    let Canonicalized::Match {
        identity: Identity::Platform { content_id, .. },
        ..
    } = canonical
    else {
        panic!("{canonical:?}")
    };
    assert_eq!(content_id, ID);
}

#[tokio::test]
async fn a_link_that_is_not_an_article_is_turned_away_through_the_host() {
    let (_tmp, host) = host();
    let plugin = &host.plugins()[0];
    let url = url::Url::parse("https://mp.weixin.qq.com/s/short").unwrap();
    let request = FetchRequest {
        method: "article".into(),
        input: FetchInput::Url(url),
        want: None,
        input_file: None,
    };
    let err = host
        .fetch(plugin, request, &CallOptions::default())
        .await
        .unwrap_err();
    let PluginError::Call { failure, .. } = err else {
        panic!("{err:?}")
    };
    assert_eq!(failure.code, ErrorCode::UnsupportedInput);
}
