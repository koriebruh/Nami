// Create/edit validation of the Phase 2 monitor types.
// Requires `docker compose up -d postgres`.
mod common;

use axum::http::{HeaderValue, StatusCode};
use common::{call, seed_project, test_app, user_with_role};
use serde_json::{Value, json};
use uuid::Uuid;

struct Ctx {
    app: axum::Router,
    pool: sqlx::PgPool,
    project: Uuid,
    member: HeaderValue,
}

async fn ctx(pool: sqlx::PgPool) -> Ctx {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (_, _, member) = user_with_role(&app, &pool, project, Some("member")).await;
    Ctx {
        app,
        pool,
        project,
        member,
    }
}

impl Ctx {
    async fn create(&self, body: Value) -> (StatusCode, Value) {
        call(
            &self.app,
            "POST",
            &format!("/projects/{}/monitors", self.project),
            Some(&self.member),
            Some(body),
        )
        .await
    }

    async fn patch(&self, id: &str, body: Value) -> (StatusCode, Value) {
        let uri = format!("/projects/{}/monitors/{id}", self.project);
        call(&self.app, "PATCH", &uri, Some(&self.member), Some(body)).await
    }

    /// Every case must be a 400 whose message mentions its fragment, and
    /// none of them may write a monitor.
    async fn assert_rejected(&self, cases: Vec<(Value, &str)>) {
        for (body, fragment) in cases {
            let (status, json) = self.create(body.clone()).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{body} returned {json}");
            let message = json["error"].as_str().unwrap_or_default();
            assert!(
                message.contains(fragment),
                "{body}: {message:?} should mention {fragment:?}"
            );
        }
        let count: i64 = sqlx::query_scalar("select count(*) from monitors where project_id = $1")
            .bind(self.project)
            .fetch_one(&self.pool)
            .await
            .unwrap();
        assert_eq!(count, 0, "no invalid monitor was written");
    }
}

#[sqlx::test]
async fn ping_monitors_need_a_valid_host_and_keep_no_port(pool: sqlx::PgPool) {
    let c = ctx(pool).await;
    let (status, created) = c
        .create(json!({
            "name": "Router", "check_type": "ping", "host": " Gateway.LAN ", "port": 22
        }))
        .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    assert_eq!(created["check_type"], "ping");
    assert_eq!(created["host"], "gateway.lan", "trimmed and lower-cased");
    assert_eq!(created["port"], Value::Null, "ping has no port");
    for field in [
        "url",
        "keyword",
        "dns_record_type",
        "dns_nameserver",
        "expected_value",
    ] {
        assert_eq!(created[field], Value::Null, "{field} is not used by ping");
    }
    let (status, v6) = c
        .create(json!({ "name": "v6", "check_type": "ping", "host": "[2001:DB8::1]" }))
        .await;
    assert_eq!(status, StatusCode::CREATED, "{v6}");
    assert_eq!(v6["host"], "2001:db8::1");

    let c = ctx(c.pool.clone()).await;
    c.assert_rejected(vec![
        (
            json!({ "name": "x", "check_type": "ping" }),
            "host is required",
        ),
        (
            json!({ "name": "x", "check_type": "ping", "host": "   " }),
            "host is required",
        ),
        (
            json!({ "name": "x", "check_type": "ping", "host": "https://example.com" }),
            "host must not contain",
        ),
        (
            json!({ "name": "x", "check_type": "ping", "host": "exa mple.com" }),
            "host must not contain",
        ),
        (
            json!({ "name": "x", "check_type": "ping", "host": "example.com/path" }),
            "host must not contain",
        ),
        (
            json!({ "name": "x", "check_type": "ping", "host": "a..b" }),
            "host must be",
        ),
        (
            json!({
                "name": "x", "check_type": "ping", "host": format!("{}.com", "a".repeat(64))
            }),
            "host must be",
        ),
    ])
    .await;
}

/// Ping and dns hosts follow the rules a tcp host does (nothing that is not
/// a bare host name or IP address, at most 253 characters, no invisible or
/// text-direction characters) and are stricter about names.
#[sqlx::test]
async fn ping_and_dns_hosts_follow_the_hardened_host_rules(pool: sqlx::PgPool) {
    let c = ctx(pool).await;
    let mut cases = Vec::new();
    for check_type in ["ping", "dns"] {
        let host = |host: &str| json!({ "name": "x", "check_type": check_type, "host": host });
        // A zero-width space, a right-to-left override, a paragraph separator
        // (at the start, where trimming must not hide it) and a soft hyphen.
        for raw in [
            "db\u{200B}.internal",
            "db\u{202E}.internal",
            "\u{2029}db.internal",
            "db\u{00AD}internal",
        ] {
            cases.push((host(raw), "invisible or text-direction characters"));
        }
        for raw in [
            "user@db",
            "db?x=1",
            "db#x",
            "db\tinternal",
            "db\ninternal",
            "db\u{0}",
            "db\u{a0}internal",
            "tcp://db",
        ] {
            cases.push((host(raw), "host must not contain"));
        }
        // A colon only belongs in an IPv6 literal; the port has no field here.
        for raw in [
            "db:5432",
            "[db]",
            "[10.0.0.1]",
            "[::1",
            "::1]",
            "fe80::1%eth0",
        ] {
            cases.push((host(raw), "host must be"));
        }
        // Numbers the system resolver reads as addresses (inet_aton) are not
        // names: `2852039166` is the metadata address.
        for raw in [
            "2852039166",
            "0xa9fea9fe",
            "127.1",
            "010.0.0.1",
            "1.2.3",
            "db.123",
        ] {
            cases.push((host(raw), "host must be"));
        }
        for raw in ["ä.test", "-a.test", "a-.test", ".", "a..b", "db .internal"] {
            cases.push((host(raw), "host must"));
        }
        cases.push((host(&"a.".repeat(127)), "at most 253 characters"));
        cases.push((host(&"\u{e9}".repeat(254)), "at most 253 characters"));
    }
    c.assert_rejected(cases).await;
    // An address has no records to look up.
    c.assert_rejected(
        ["10.0.0.5", "::1", "2001:DB8:0:0::1", "[::1]"]
            .iter()
            .chain(&["169.254.169.254"])
            .map(|host| {
                (
                    json!({ "name": "x", "check_type": "dns", "host": host }),
                    "host must be a domain name to look up",
                )
            })
            .collect(),
    )
    .await;

    for check_type in ["ping", "dns"] {
        let name_of_253 = format!(
            "{}.{}.{}.{}",
            "a".repeat(63),
            "b".repeat(63),
            "c".repeat(63),
            "d".repeat(61)
        );
        assert_eq!(name_of_253.len(), 253);
        for (host, stored) in [
            ("db.internal", "db.internal"),
            ("  Db.Example.COM\t", "db.example.com"),
            ("example.com.", "example.com."),
            ("_dmarc.example.com", "_dmarc.example.com"),
            ("localhost", "localhost"),
            (name_of_253.as_str(), name_of_253.as_str()),
        ] {
            let (status, created) = c
                .create(json!({ "name": "ok", "check_type": check_type, "host": host }))
                .await;
            assert_eq!(
                status,
                StatusCode::CREATED,
                "{check_type} {host:?}: {created}"
            );
            assert_eq!(created["host"], stored, "{check_type} {host:?}");
        }
    }
    let addresses = ["10.0.0.5", "::1", "2001:DB8:0:0::1", "[::1]"];
    for (host, stored) in addresses.iter().zip(["10.0.0.5", "::1", "2001:db8::1", "::1"]) {
        let (status, created) = c
            .create(json!({ "name": "ok", "check_type": "ping", "host": host }))
            .await;
        assert_eq!(status, StatusCode::CREATED, "ping {host:?}: {created}");
        assert_eq!(created["host"], stored, "ping {host:?}");
    }
}

#[sqlx::test]
async fn dns_monitors_validate_record_type_nameserver_and_expected_value(pool: sqlx::PgPool) {
    let c = ctx(pool).await;
    let (status, created) = c
        .create(json!({
            "name": "DMARC", "check_type": "dns", "host": "_dmarc.example.com",
            "dns_record_type": "txt", "dns_nameserver": "1.1.1.1", "expected_value": " v=DMARC1 "
        }))
        .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    assert_eq!(created["dns_record_type"], "TXT");
    assert_eq!(
        created["dns_nameserver"], "1.1.1.1:53",
        "the default port is made explicit"
    );
    assert_eq!(created["expected_value"], "v=DMARC1");
    let (status, defaulted) = c
        .create(json!({ "name": "A", "check_type": "dns", "host": "example.com" }))
        .await;
    assert_eq!(status, StatusCode::CREATED, "{defaulted}");
    assert_eq!(
        defaulted["dns_record_type"], "A",
        "A is the default record type"
    );
    assert_eq!(
        defaulted["dns_nameserver"],
        Value::Null,
        "no nameserver = the system resolver"
    );
    assert_eq!(defaulted["expected_value"], Value::Null);
    let (status, v6) = c
        .create(json!({
            "name": "v6", "check_type": "dns", "host": "example.com",
            "dns_nameserver": "[2606:4700::1111]:5353"
        }))
        .await;
    assert_eq!(status, StatusCode::CREATED, "{v6}");
    assert_eq!(v6["dns_nameserver"], "[2606:4700::1111]:5353");
    let (status, blanks) = c
        .create(json!({
            "name": "blank", "check_type": "dns", "host": "example.com",
            "dns_record_type": " ", "dns_nameserver": "  ", "expected_value": "   "
        }))
        .await;
    assert_eq!(status, StatusCode::CREATED, "{blanks}");
    assert_eq!(blanks["dns_record_type"], "A");
    assert_eq!(blanks["dns_nameserver"], Value::Null, "blank = unset");
    assert_eq!(blanks["expected_value"], Value::Null, "blank = unset");

    let c = ctx(c.pool.clone()).await;
    let dns = |extra: Value| {
        let mut body = json!({ "name": "x", "check_type": "dns", "host": "example.com" });
        body.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        body
    };
    c.assert_rejected(vec![
        (
            json!({ "name": "x", "check_type": "dns" }),
            "host is required",
        ),
        (
            dns(json!({ "dns_record_type": "SOA" })),
            "dns_record_type must be one of A, AAAA, CNAME, MX, TXT, NS",
        ),
        (
            dns(json!({ "dns_record_type": "ANY" })),
            "dns_record_type must be one of",
        ),
        (
            dns(json!({ "dns_nameserver": "dns.google" })),
            "dns_nameserver must be an IP address",
        ),
        (
            dns(json!({ "dns_nameserver": "1.1.1.1:0" })),
            "dns_nameserver must be an IP address",
        ),
        (
            dns(json!({ "dns_nameserver": "1.1.1.1:99999" })),
            "dns_nameserver must be an IP address",
        ),
        (
            dns(json!({ "dns_nameserver": "https://1.1.1.1" })),
            "dns_nameserver must be an IP address",
        ),
        (
            dns(json!({ "dns_nameserver": "1.1.1.1\n8.8.8.8" })),
            "dns_nameserver must be an IP address",
        ),
        (
            dns(json!({ "dns_nameserver": "2852039166" })),
            "dns_nameserver must be an IP address",
        ),
        (dns(json!({ "expected_value": "a\nb" })), "expected_value"),
        (
            dns(json!({ "expected_value": "a\u{0}b" })),
            "expected_value",
        ),
        (
            dns(json!({ "expected_value": "a".repeat(1025) })),
            "expected_value must be at most 1024 bytes",
        ),
        (
            dns(json!({ "expected_value": "\u{e9}".repeat(513) })),
            "expected_value must be at most 1024 bytes",
        ),
    ])
    .await;

    // The cap is the same bytes as a keyword's, at the boundary.
    for (label, expected) in [
        ("1024 bytes", "a".repeat(1024)),
        ("512 two-byte characters", "\u{e9}".repeat(512)),
        (
            "a value with spaces and symbols",
            "v=spf1 include:_spf.example.com ~all".to_string(),
        ),
    ] {
        let (status, created) = c.create(dns(json!({ "expected_value": expected }))).await;
        assert_eq!(status, StatusCode::CREATED, "{label}: {created}");
        assert_eq!(created["expected_value"], expected, "{label}");
    }
}

#[sqlx::test]
async fn switching_away_from_dns_clears_its_fields(pool: sqlx::PgPool) {
    let c = ctx(pool).await;
    let (_, created) = c
        .create(json!({
            "name": "MX", "check_type": "dns", "host": "example.com", "dns_record_type": "MX",
            "dns_nameserver": "9.9.9.9", "expected_value": "mail"
        }))
        .await;
    let id = created["id"].as_str().unwrap();

    let (status, http) = c
        .patch(
            id,
            json!({ "check_type": "http", "url": "https://example.com" }),
        )
        .await;

    assert_eq!(status, StatusCode::OK, "{http}");
    for field in [
        "host",
        "dns_record_type",
        "dns_nameserver",
        "expected_value",
    ] {
        assert_eq!(http[field], Value::Null, "{field} must be cleared");
    }
    let (status, cleared) = c
        .patch(id, json!({ "check_type": "dns", "host": "example.com" }))
        .await;
    assert_eq!(status, StatusCode::OK, "{cleared}");
    assert_eq!(cleared["dns_record_type"], "A");
}

/// Every type keeps only the fields it uses, whichever type it had before.
#[sqlx::test]
async fn every_type_switch_leaves_only_the_fields_of_the_new_type(pool: sqlx::PgPool) {
    let c = ctx(pool).await;
    let (_, created) = c
        .create(json!({
            "name": "Box", "check_type": "tcp", "host": "db.internal", "port": 5432
        }))
        .await;
    let id = created["id"].as_str().unwrap();

    // tcp -> ping: the port goes, the host stays.
    let (status, ping) = c.patch(id, json!({ "check_type": "ping" })).await;
    assert_eq!(status, StatusCode::OK, "{ping}");
    assert_eq!(
        (&ping["host"], &ping["port"]),
        (&json!("db.internal"), &Value::Null)
    );

    // ping -> dns: record type defaults, host stays.
    let (status, dns) = c
        .patch(
            id,
            json!({ "check_type": "dns", "dns_nameserver": "127.0.0.1:5353", "expected_value": "x" }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{dns}");
    assert_eq!(dns["host"], "db.internal");
    assert_eq!(dns["dns_record_type"], "A");

    // dns -> ping: the dns fields go.
    let (status, ping) = c.patch(id, json!({ "check_type": "ping" })).await;
    assert_eq!(status, StatusCode::OK, "{ping}");
    assert_eq!(ping["host"], "db.internal");
    for field in [
        "dns_record_type",
        "dns_nameserver",
        "expected_value",
        "port",
    ] {
        assert_eq!(ping[field], Value::Null, "{field}");
    }

    // ping -> tcp needs the port again, and a rejected switch changes nothing.
    let (status, rejected) = c.patch(id, json!({ "check_type": "tcp" })).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{rejected}");
    assert!(rejected["error"].as_str().unwrap().contains("port"));
    let (status, tcp) = c
        .patch(id, json!({ "check_type": "tcp", "port": 22 }))
        .await;
    assert_eq!(status, StatusCode::OK, "{tcp}");
    assert_eq!(tcp["port"], 22);

    // tcp -> keyword -> ping: no url, keyword or port is left behind.
    let (status, keyword) = c
        .patch(
            id,
            json!({ "check_type": "keyword", "url": "https://example.com", "keyword": "ok" }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{keyword}");
    assert_eq!(
        (&keyword["host"], &keyword["port"]),
        (&Value::Null, &Value::Null)
    );
    let (status, ping) = c
        .patch(id, json!({ "check_type": "ping", "host": "gateway.lan" }))
        .await;
    assert_eq!(status, StatusCode::OK, "{ping}");
    for field in ["url", "keyword", "port"] {
        assert_eq!(ping[field], Value::Null, "{field}");
    }
    assert_eq!(ping["host"], "gateway.lan");
}

#[sqlx::test]
async fn dns_fields_follow_patch_rules_and_null_clears_them(pool: sqlx::PgPool) {
    let c = ctx(pool).await;
    let (_, created) = c
        .create(json!({
            "name": "MX", "check_type": "dns", "host": "example.com", "dns_record_type": "MX",
            "dns_nameserver": "9.9.9.9", "expected_value": "mail"
        }))
        .await;
    let id = created["id"].as_str().unwrap();

    let (_, renamed) = c.patch(id, json!({ "name": "renamed" })).await;
    assert_eq!(renamed["dns_record_type"], "MX", "absent keeps");
    assert_eq!(renamed["dns_nameserver"], "9.9.9.9:53");
    assert_eq!(renamed["expected_value"], "mail");

    let (_, no_nameserver) = c.patch(id, json!({ "dns_nameserver": null })).await;
    assert_eq!(no_nameserver["dns_nameserver"], Value::Null, "null clears");
    assert_eq!(no_nameserver["expected_value"], "mail", "the others stay");

    let (_, no_expected) = c.patch(id, json!({ "expected_value": null })).await;
    assert_eq!(no_expected["expected_value"], Value::Null);
    assert_eq!(no_expected["dns_record_type"], "MX");

    let (_, retyped) = c
        .patch(
            id,
            json!({ "dns_record_type": "ns", "dns_nameserver": "[::1]:5353" }),
        )
        .await;
    assert_eq!(retyped["dns_record_type"], "NS");
    assert_eq!(retyped["dns_nameserver"], "[::1]:5353");

    // A bad value is a 400 and changes nothing.
    for body in [
        json!({ "dns_record_type": "SOA" }),
        json!({ "dns_nameserver": "dns.google" }),
        json!({ "expected_value": "a\nb" }),
    ] {
        let (status, rejected) = c.patch(id, body.clone()).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}: {rejected}");
    }
    let (status, still) = call(
        &c.app,
        "GET",
        &format!("/projects/{}/monitors/{id}", c.project),
        Some(&c.member),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(still["dns_record_type"], "NS");
    assert_eq!(still["dns_nameserver"], "[::1]:5353");
}

/// Editing what a check does reschedules it to run now; renaming does not.
#[sqlx::test]
async fn editing_the_dns_target_reschedules_the_check_and_renaming_does_not(pool: sqlx::PgPool) {
    let c = ctx(pool).await;
    let (_, created) = c
        .create(json!({
            "name": "MX", "check_type": "dns", "host": "example.com", "dns_record_type": "MX"
        }))
        .await;
    let id = created["id"].as_str().unwrap();
    let id_uuid: Uuid = id.parse().unwrap();
    let park = || async {
        sqlx::query("update monitors set next_check_at = now() + interval '1 day' where id = $1")
            .bind(id_uuid)
            .execute(&c.pool)
            .await
            .unwrap();
    };
    let due_now = || async {
        sqlx::query_scalar::<_, bool>("select next_check_at <= now() from monitors where id = $1")
            .bind(id_uuid)
            .fetch_one(&c.pool)
            .await
            .unwrap()
    };

    park().await;
    c.patch(id, json!({ "name": "renamed", "paused": false }))
        .await;
    assert!(!due_now().await, "a rename is not a check change");

    for change in [
        json!({ "dns_record_type": "TXT" }),
        json!({ "dns_nameserver": "9.9.9.9" }),
        json!({ "expected_value": "mail" }),
        json!({ "host": "example.org" }),
    ] {
        park().await;
        let (status, body) = c.patch(id, change.clone()).await;
        assert_eq!(status, StatusCode::OK, "{change}: {body}");
        assert!(due_now().await, "{change} must reschedule the check");
    }
}

/// Request configuration belongs to http and keyword monitors: the other
/// types neither keep it nor reject it.
#[sqlx::test]
async fn ping_and_dns_monitors_carry_no_request_configuration(pool: sqlx::PgPool) {
    let c = ctx(pool).await;
    for body in [
        json!({ "name": "p", "check_type": "ping", "host": "gateway.lan" }),
        json!({ "name": "d", "check_type": "dns", "host": "example.com" }),
    ] {
        let mut body = body;
        body.as_object_mut().unwrap().extend(
            json!({
                "http_method": "POST", "request_body": "{}", "auth_username": "u",
                "auth_password": "p", "request_headers": [{ "name": "X-A", "value": "v" }],
                "follow_redirects": false, "verify_ssl": false
            })
            .as_object()
            .unwrap()
            .clone(),
        );
        let (status, created) = c.create(body).await;
        assert_eq!(status, StatusCode::CREATED, "{created}");
        assert_eq!(created["http_method"], "GET");
        assert_eq!(created["request_body"], Value::Null);
        assert_eq!(created["auth_username"], Value::Null);
        assert_eq!(created["has_auth_password"], false);
        assert_eq!(created["request_headers"], json!([]));
        assert_eq!(created["follow_redirects"], true);
        assert_eq!(created["verify_ssl"], true);
        let stored: (Option<Vec<u8>>, Option<Vec<u8>>, Vec<String>) = sqlx::query_as(
            "select request_headers_enc, auth_password_enc, request_header_names
             from monitors where id = $1",
        )
        .bind(created["id"].as_str().unwrap().parse::<Uuid>().unwrap())
        .fetch_one(&c.pool)
        .await
        .unwrap();
        assert_eq!(stored, (None, None, Vec::new()), "no secret is stored");
    }
}

#[sqlx::test]
async fn an_unknown_check_type_names_the_supported_ones(pool: sqlx::PgPool) {
    let c = ctx(pool).await;
    c.assert_rejected(vec![
        (
            json!({ "name": "x", "check_type": "icmp", "host": "gateway.lan" }),
            "check_type must be one of http, keyword, tcp, ping, dns",
        ),
        (
            json!({ "name": "x", "check_type": "PING", "host": "gateway.lan" }),
            "check_type must be one of",
        ),
    ])
    .await;
}

/// The check constraints are the last line behind the API.
#[sqlx::test]
async fn the_database_accepts_ping_and_dns_and_only_known_record_types(pool: sqlx::PgPool) {
    let project = seed_project(&pool).await;
    let insert = |check_type: &'static str, record_type: Option<&'static str>| {
        let pool = pool.clone();
        async move {
            sqlx::query(
                "insert into monitors (project_id, name, check_type, host, dns_record_type)
                 values ($1, 'm', $2, 'example.com', $3)",
            )
            .bind(project)
            .bind(check_type)
            .bind(record_type)
            .execute(&pool)
            .await
        }
    };
    assert!(insert("ping", None).await.is_ok());
    assert!(insert("dns", Some("A")).await.is_ok());
    for record_type in ["A", "AAAA", "CNAME", "MX", "TXT", "NS"] {
        assert!(
            insert("dns", Some(record_type)).await.is_ok(),
            "{record_type}"
        );
    }
    for record_type in ["SOA", "ANY", "a", "PTR", ""] {
        assert!(
            insert("dns", Some(record_type)).await.is_err(),
            "{record_type:?} must be refused by the check constraint"
        );
    }
    assert!(
        insert("icmp", None).await.is_err(),
        "unknown types stay refused"
    );
    assert!(
        insert("http", None).await.is_ok(),
        "the old types still work"
    );
    assert!(insert("keyword", None).await.is_ok());
    assert!(insert("tcp", None).await.is_ok());
}

#[sqlx::test]
async fn expiry_monitors_get_daily_checks_and_type_specific_thresholds(pool: sqlx::PgPool) {
    let c = ctx(pool).await;
    let (status, ssl) = c
        .create(json!({ "name": "TLS", "check_type": "ssl_expiry", "host": "example.com" }))
        .await;
    assert_eq!(status, StatusCode::CREATED, "{ssl}");
    assert_eq!(ssl["port"], 443);
    assert_eq!(ssl["expiry_threshold_days"], 14);
    assert_eq!(ssl["interval_seconds"], 86400);
    let (status, domain) = c
        .create(json!({ "name": "Domain", "check_type": "domain_expiry", "host": "Example.COM." }))
        .await;
    assert_eq!(status, StatusCode::CREATED, "{domain}");
    assert_eq!(
        domain["host"], "example.com",
        "lower-cased, trailing dot dropped"
    );
    assert_eq!(domain["expiry_threshold_days"], 30);
    assert_eq!(domain["interval_seconds"], 86400);
    assert_eq!(domain["port"], Value::Null);
    let (status, http) = c
        .create(json!({ "name": "Web", "check_type": "http", "url": "https://example.com" }))
        .await;
    assert_eq!(status, StatusCode::CREATED, "{http}");
    assert_eq!(
        http["interval_seconds"], 60,
        "other types keep the 60 s default"
    );
    assert_eq!(http["expiry_threshold_days"], Value::Null);

    let c = ctx(c.pool.clone()).await;
    c.assert_rejected(vec![
        (
            json!({ "name": "x", "check_type": "ssl_expiry", "host": "example.com", "interval_seconds": 600 }),
            "interval_seconds must be at least 3600",
        ),
        (
            json!({ "name": "x", "check_type": "ssl_expiry", "host": "example.com", "expiry_threshold_days": 0 }),
            "expiry_threshold_days must be between 1 and 365",
        ),
        (
            json!({ "name": "x", "check_type": "domain_expiry", "host": "example.com", "expiry_threshold_days": 366 }),
            "expiry_threshold_days must be between 1 and 365",
        ),
        (json!({ "name": "x", "check_type": "ssl_expiry", "host": "example.com", "port": 70000 }), "port"),
        (json!({ "name": "x", "check_type": "domain_expiry", "host": "93.184.216.34" }), "domain name"),
        (json!({ "name": "x", "check_type": "domain_expiry", "host": "localhost" }), "domain name"),
        (json!({ "name": "x", "check_type": "domain_expiry" }), "host is required"),
    ])
    .await;
}

#[sqlx::test]
async fn the_database_itself_keeps_expiry_monitors_hourly_at_most(pool: sqlx::PgPool) {
    let project = seed_project(&pool).await;
    let insert = |check_type: &'static str, interval: i32, threshold: Option<i32>| {
        let pool = pool.clone();
        async move {
            sqlx::query(
                "insert into monitors (project_id, name, check_type, interval_seconds, expiry_threshold_days)
                 values ($1, 'm', $2, $3, $4)",
            )
            .bind(project)
            .bind(check_type)
            .bind(interval)
            .bind(threshold)
            .execute(&pool)
            .await
        }
    };
    assert!(insert("ssl_expiry", 3600, Some(14)).await.is_ok());
    assert!(insert("domain_expiry", 86400, None).await.is_ok());
    assert!(insert("ssl_expiry", 3599, None).await.is_err());
    assert!(insert("domain_expiry", 60, None).await.is_err());
    assert!(
        insert("http", 60, None).await.is_ok(),
        "other types keep short intervals"
    );
    assert!(insert("ssl_expiry", 3600, Some(0)).await.is_err());
    assert!(insert("ssl_expiry", 3600, Some(366)).await.is_err());
}

#[sqlx::test]
async fn editing_an_expiry_monitor_revalidates_the_interval_and_threshold(pool: sqlx::PgPool) {
    let c = ctx(pool).await;
    let (_, ssl) = c
        .create(json!({ "name": "TLS", "check_type": "ssl_expiry", "host": "example.com" }))
        .await;
    let id = ssl["id"].as_str().unwrap();
    let (status, body) = c.patch(id, json!({ "interval_seconds": 600 })).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    let (status, body) = c.patch(id, json!({ "expiry_threshold_days": 400 })).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    let (status, body) = c.patch(id, json!({ "expiry_threshold_days": 7 })).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["expiry_threshold_days"], 7);
    // Changing the type to one that does not use the threshold clears it.
    let (status, body) = c.patch(id, json!({ "check_type": "ping" })).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["expiry_threshold_days"], Value::Null);
    assert_eq!(body["port"], Value::Null);
}

#[sqlx::test]
async fn switching_between_expiry_types_resets_the_threshold_to_the_new_default(
    pool: sqlx::PgPool,
) {
    let c = ctx(pool).await;
    let (_, ssl) = c
        .create(json!({ "name": "TLS", "check_type": "ssl_expiry", "host": "example.com", "expiry_threshold_days": 7 }))
        .await;
    let id = ssl["id"].as_str().unwrap();
    assert_eq!(ssl["expiry_threshold_days"], 7);

    // Renaming keeps it.
    let (_, body) = c.patch(id, json!({ "name": "TLS 2" })).await;
    assert_eq!(body["expiry_threshold_days"], 7);
    // A type change without a value takes the new type's default.
    let (status, body) = c.patch(id, json!({ "check_type": "domain_expiry" })).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["expiry_threshold_days"], 30);
    // ...and an explicit value wins.
    let (_, body) = c
        .patch(
            id,
            json!({ "check_type": "ssl_expiry", "expiry_threshold_days": 21 }),
        )
        .await;
    assert_eq!(body["expiry_threshold_days"], 21);
    // Sending the same type again is no change.
    let (_, body) = c.patch(id, json!({ "check_type": "ssl_expiry" })).await;
    assert_eq!(body["expiry_threshold_days"], 21);
    // null resets to the type's default.
    let (status, body) = c.patch(id, json!({ "expiry_threshold_days": null })).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["expiry_threshold_days"], 14);
    // Into an expiry type from another one.
    let (_, ping) = c
        .create(json!({ "name": "P", "check_type": "ping", "host": "example.com" }))
        .await;
    let (status, body) = c
        .patch(
            ping["id"].as_str().unwrap(),
            json!({ "check_type": "domain_expiry", "interval_seconds": 86400 }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["expiry_threshold_days"], 30);
}

#[sqlx::test]
async fn expiry_monitors_alert_on_the_first_failed_check_by_default(pool: sqlx::PgPool) {
    let c = ctx(pool).await;
    for check_type in ["ssl_expiry", "domain_expiry"] {
        let (_, created) = c
            .create(json!({ "name": check_type, "check_type": check_type, "host": "example.com" }))
            .await;
        assert_eq!(created["confirmation_threshold"], 1, "{check_type}");
        let (_, explicit) = c
            .create(json!({ "name": "e", "check_type": check_type, "host": "example.com", "confirmation_threshold": 3 }))
            .await;
        assert_eq!(explicit["confirmation_threshold"], 3, "{check_type}");
    }
    let (_, http) = c
        .create(json!({ "name": "h", "check_type": "http", "url": "https://example.com" }))
        .await;
    assert_eq!(http["confirmation_threshold"], 2, "other types keep 2");
}

#[sqlx::test]
async fn mail_monitors_default_their_port_from_protocol_and_tls(pool: sqlx::PgPool) {
    let c = ctx(pool).await;
    let cases = [
        ("smtp", false, 25),
        ("smtp", true, 465),
        ("pop3", false, 110),
        ("pop3", true, 995),
        ("imap", false, 143),
        ("imap", true, 993),
    ];
    for (check_type, use_tls, port) in cases {
        let (status, created) = c
            .create(json!({ "name": check_type, "check_type": check_type, "host": "mail.example.com", "use_tls": use_tls }))
            .await;
        assert_eq!(status, StatusCode::CREATED, "{created}");
        assert_eq!(created["port"], port, "{check_type} use_tls={use_tls}");
        assert_eq!(created["use_tls"], use_tls);
    }
    let (status, custom) = c
        .create(json!({ "name": "submission", "check_type": "smtp", "host": "mail.example.com", "port": 587 }))
        .await;
    assert_eq!(status, StatusCode::CREATED, "{custom}");
    assert_eq!(custom["port"], 587);
    let (_, http) = c
        .create(json!({ "name": "web", "check_type": "http", "url": "https://example.com", "use_tls": true }))
        .await;
    assert_eq!(
        http["use_tls"], false,
        "use_tls only applies to mail monitors"
    );

    let c = ctx(c.pool.clone()).await;
    c.assert_rejected(vec![
        (
            json!({ "name": "x", "check_type": "imap" }),
            "host is required",
        ),
        (
            json!({ "name": "x", "check_type": "pop3", "host": "mail.example.com", "port": 0 }),
            "port",
        ),
        (
            json!({ "name": "x", "check_type": "smtp", "host": "smtp://mail.example.com" }),
            "host must not contain",
        ),
    ])
    .await;
}

#[sqlx::test]
async fn udp_monitors_need_a_port_and_a_payload(pool: sqlx::PgPool) {
    let c = ctx(pool).await;
    let (status, created) = c
        .create(json!({
            "name": "Game server", "check_type": "udp", "host": "game.example.com", "port": 27015,
            "udp_payload": "\u{ff}\u{ff}status\r\n", "keyword": "players"
        }))
        .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    assert_eq!(
        created["udp_payload"], "\u{ff}\u{ff}status\r\n",
        "payloads are sent verbatim, line breaks included"
    );
    assert_eq!(created["keyword"], "players");
    let id = created["id"].as_str().unwrap();
    let (status, tcp) = c.patch(id, json!({ "check_type": "tcp" })).await;
    assert_eq!(status, StatusCode::OK, "{tcp}");
    assert_eq!(tcp["udp_payload"], Value::Null);
    assert_eq!(tcp["keyword"], Value::Null);

    let c = ctx(c.pool.clone()).await;
    let udp = |extra: Value| {
        let mut body = json!({ "name": "x", "check_type": "udp", "host": "game.example.com", "port": 27015, "udp_payload": "hi" });
        body.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        body
    };
    c.assert_rejected(vec![
        (udp(json!({ "port": null })), "port"),
        (
            udp(json!({ "udp_payload": null })),
            "udp_payload is required",
        ),
        (udp(json!({ "udp_payload": "" })), "udp_payload is required"),
        (udp(json!({ "udp_payload": "a\u{0}b" })), "NUL"),
        (
            udp(json!({ "udp_payload": "a".repeat(1025) })),
            "at most 1024 bytes",
        ),
        (
            udp(json!({ "keyword": "a".repeat(1025) })),
            "keyword must be at most 1024 bytes",
        ),
        (
            udp(json!({ "keyword": "a\nb" })),
            "keyword must not contain control characters",
        ),
    ])
    .await;
}
