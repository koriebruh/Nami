mod common;

use axum::{
    Router,
    body::Body,
    http::{HeaderValue, Request, StatusCode, header},
};
use common::{call, seed_project, test_cipher, user_with_role};
use nami_engine::models::monitor::{self, CreateMonitorRequest};
use nami_engine::models::monitor_request::{open_headers, open_password};
use nami_engine::uptime::import::{
    IMPORT_LOCK_CLASS, ImportDefaults, ImportFormat, MAX_ENTRIES, MAX_IMPORT_BODY_BYTES,
    MAX_IMPORT_REQUEST_BYTES, ParseError, UNREADABLE_ENTRY, existing_key, parse,
};
use serde_json::{Value, json};
use tower::ServiceExt;
use uuid::Uuid;

fn bodies(format: ImportFormat, content: &str) -> Vec<(usize, Result<Value, String>)> {
    parse(format, content, &ImportDefaults::default())
        .unwrap()
        .into_iter()
        .map(|e| (e.line, e.body.map(Value::Object)))
        .collect()
}

/// What the import route will do with a parsed body: the create path's own
/// deserialization and validation.
fn validated(body: &Value) -> Result<monitor::MonitorConfig, String> {
    let request: CreateMonitorRequest =
        serde_json::from_value(body.clone()).map_err(|e| e.to_string())?;
    monitor::validate(request.into_config())
}

#[test]
fn url_lists_skip_blanks_and_comments_and_name_monitors_after_the_host() {
    let got = bodies(
        ImportFormat::Urls,
        "# prod\nhttps://example.com/health\n\n  http://api.example.com:8080/v1  \nnot a url\nftp://x.test/\n",
    );
    assert_eq!(
        got,
        vec![
            (
                2,
                Ok(
                    json!({"check_type": "http", "name": "example.com", "url": "https://example.com/health"})
                )
            ),
            (
                4,
                Ok(
                    json!({"check_type": "http", "name": "api.example.com", "url": "http://api.example.com:8080/v1"})
                )
            ),
            (5, Err("line is not a valid http(s) URL".to_string())),
            (6, Err("line is not a valid http(s) URL".to_string())),
        ]
    );
}

#[test]
fn more_than_500_entries_are_refused_before_anything_else() {
    let many: String = (0..=MAX_ENTRIES)
        .map(|i| format!("https://h{i}.test/\n"))
        .collect();
    assert_eq!(
        parse(ImportFormat::Urls, &many, &ImportDefaults::default()),
        Err(ParseError::TooManyEntries(501))
    );
    let exactly: String = (0..MAX_ENTRIES)
        .map(|i| format!("https://h{i}.test/\n# c\n\n"))
        .collect();
    assert_eq!(
        parse(ImportFormat::Urls, &exactly, &ImportDefaults::default())
            .unwrap()
            .len(),
        500
    );
}

#[test]
fn defaults_fill_what_the_source_leaves_out() {
    let group = uuid::Uuid::new_v4();
    let defaults = ImportDefaults {
        interval_seconds: Some(120),
        timeout_seconds: Some(5),
        confirmation_threshold: Some(3),
        recovery_threshold: Some(2),
        group_id: Some(group),
    };
    let entries = parse(ImportFormat::Urls, "https://a.test/", &defaults).unwrap();
    assert_eq!(
        Value::Object(entries[0].body.clone().unwrap()),
        json!({
            "check_type": "http", "name": "a.test", "url": "https://a.test/", "interval_seconds": 120,
            "timeout_seconds": 5, "confirmation_threshold": 3, "recovery_threshold": 2, "group_id": group })
    );
}

#[test]
fn defaults_never_override_the_source_but_the_group_always_applies() {
    let group = uuid::Uuid::new_v4();
    let defaults = ImportDefaults {
        interval_seconds: Some(999),
        group_id: Some(group),
        ..ImportDefaults::default()
    };
    let content = "Friendly Name,URL/IP,Type,Interval\nA,https://a.test/,HTTP(s),300\n";
    let entries = parse(ImportFormat::Uptimerobot, content, &defaults).unwrap();
    let body = entries[0].body.as_ref().unwrap();
    assert_eq!(body["interval_seconds"], 300);
    assert_eq!(body["group_id"], json!(group));
}

#[test]
fn uptime_kuma_backups_map_every_supported_type() {
    let content = std::fs::read_to_string("tests/fixtures/import/uptime_kuma.json").unwrap();
    let got = bodies(ImportFormat::UptimeKuma, &content);
    assert_eq!(
        got[0],
        (
            1,
            Ok(json!({
                "name": "Website", "check_type": "http", "url": "https://example.com/", "http_method": "GET",
                "interval_seconds": 30, "timeout_seconds": 16, "confirmation_threshold": 3,
                "verify_ssl": true, "follow_redirects": true, "paused": false,
                "expected_status_min": 200, "expected_status_max": 299,
                "request_headers": [{"name": "X-Api-Key", "value": "kuma-header-secret"}],
                "auth_username": "ops", "auth_password": "kuma-basic-secret" }))
        )
    );
    assert_eq!(
        got[1],
        (
            2,
            Ok(json!({
                "name": "Pricing keyword", "check_type": "keyword", "url": "https://example.com/pricing", "http_method": "GET",
                "keyword": "Sold out", "keyword_mode": "not_contains", "interval_seconds": 60, "confirmation_threshold": 1,
                "verify_ssl": false, "follow_redirects": false, "paused": true,
                "expected_status_min": 200, "expected_status_max": 299 }))
        )
    );
    assert_eq!(
        got[2],
        (
            3,
            Ok(
                json!({ "name": "Postgres", "check_type": "tcp", "host": "db.internal", "port": 5432,
        "interval_seconds": 30, "confirmation_threshold": 10, "paused": false })
            )
        )
    );
    assert_eq!(
        got[3],
        (
            4,
            Ok(
                json!({ "name": "Gateway", "check_type": "ping", "host": "10.0.0.1",
        "interval_seconds": 45, "confirmation_threshold": 2, "paused": false })
            )
        )
    );
    assert_eq!(
        got[4],
        (
            5,
            Ok(
                json!({ "name": "MX", "check_type": "dns", "host": "example.com", "dns_record_type": "MX",
        "dns_nameserver": "1.1.1.1:53", "interval_seconds": 300, "confirmation_threshold": 1, "paused": false })
            )
        )
    );
    assert_eq!(
        got[5],
        (
            6,
            Err("Uptime Kuma monitor type \"docker\" is not supported".to_string())
        )
    );
    assert_eq!(
        got[6],
        (7, Err("upside-down mode is not supported".to_string()))
    );
    assert_eq!(
        got[7],
        (
            8,
            Err("authMethod \"ntlm\" is not supported (only basic)".to_string())
        )
    );
}

#[test]
fn every_mapped_kuma_and_uptimerobot_body_passes_the_real_validation() {
    let kuma = std::fs::read_to_string("tests/fixtures/import/uptime_kuma.json").unwrap();
    for (line, body) in bodies(ImportFormat::UptimeKuma, &kuma) {
        if let Ok(body) = body {
            validated(&body).unwrap_or_else(|e| panic!("kuma entry {line}: {e}"));
        }
    }
    let csv = std::fs::read_to_string("tests/fixtures/import/uptimerobot.csv").unwrap();
    for (line, body) in bodies(ImportFormat::Uptimerobot, &csv) {
        if let Ok(body) = body {
            validated(&body).unwrap_or_else(|e| panic!("csv row {line}: {e}"));
        }
    }
}

#[test]
fn kuma_notification_secrets_never_leave_the_parser() {
    let content = std::fs::read_to_string("tests/fixtures/import/uptime_kuma.json").unwrap();
    let rendered = format!(
        "{:?}",
        parse(
            ImportFormat::UptimeKuma,
            &content,
            &ImportDefaults::default()
        )
    );
    assert!(!rendered.contains("KUMA-NOTIFY-SECRET") && !rendered.contains("hooks.slack.com"));
}

#[test]
fn malformed_kuma_documents_are_one_error_and_bad_entries_their_own() {
    assert_eq!(
        parse(
            ImportFormat::UptimeKuma,
            "{not json",
            &ImportDefaults::default()
        ),
        Err(ParseError::Malformed(
            "content is not valid JSON (line 1, column 2)".into()
        ))
    );
    assert_eq!(
        parse(
            ImportFormat::UptimeKuma,
            "{\"version\":\"1.23\"}",
            &ImportDefaults::default()
        ),
        Err(ParseError::Malformed(
            "content has no monitorList array (is it an Uptime Kuma backup?)".into()
        ))
    );
    // A secret in a malformed document is never quoted back.
    let err = parse(
        ImportFormat::UptimeKuma,
        "{\"monitorList\": \"hunter2\"}",
        &ImportDefaults::default(),
    )
    .unwrap_err();
    assert!(!err.to_string().contains("hunter2"));
    let got = bodies(
        ImportFormat::UptimeKuma,
        r#"{"monitorList":[42, {"type":"http"}, {"type":"http","name":"x","url":"https://x.test","headers":"[1]"}]}"#,
    );
    assert_eq!(got[0].1, Err("entry is not a JSON object".to_string()));
    assert_eq!(got[1].1, Err("entry has no name".to_string()));
    assert_eq!(
        got[2].1,
        Err("headers is not a JSON object of strings".to_string())
    );
    let many = format!("{{\"monitorList\":[{}]}}", vec!["{}"; 501].join(","));
    assert_eq!(
        parse(ImportFormat::UptimeKuma, &many, &ImportDefaults::default()),
        Err(ParseError::TooManyEntries(501))
    );
}

#[test]
fn uptimerobot_csv_maps_by_header_name() {
    let content = std::fs::read_to_string("tests/fixtures/import/uptimerobot.csv").unwrap();
    let got = bodies(ImportFormat::Uptimerobot, &content);
    assert_eq!(
        got[0],
        (
            2,
            Ok(
                json!({"name": "Website", "check_type": "http", "url": "https://example.com/", "interval_seconds": 300})
            )
        )
    );
    assert_eq!(
        got[1],
        (
            3,
            Ok(
                json!({"name": "Login page", "check_type": "keyword", "url": "https://example.com/login",
        "keyword": "Error", "keyword_mode": "not_contains", "interval_seconds": 60})
            )
        )
    );
    assert_eq!(
        got[2],
        (
            4,
            Ok(
                json!({"name": "Mail, primary", "check_type": "tcp", "host": "mail.example.com", "port": 25, "interval_seconds": 120})
            )
        )
    );
    assert_eq!(
        got[3],
        (
            5,
            Ok(
                json!({"name": "Router", "check_type": "ping", "host": "192.0.2.1", "interval_seconds": 30})
            )
        )
    );
    assert_eq!(
        got[4],
        (
            6,
            Err("UptimeRobot monitor type \"Heartbeat\" is not supported".to_string())
        )
    );
    // A formula-looking name is just a name; validation judges it like any other.
    assert_eq!(
        got[5].1.as_ref().unwrap()["name"],
        "=HYPERLINK(\"http://evil.test\")"
    );
}

#[test]
fn uptimerobot_headers_match_case_insensitively_and_unknown_columns_are_ignored() {
    let got = bodies(
        ImportFormat::Uptimerobot,
        "friendly name,url/ip,TYPE,Notes\nA,https://a.test/,1,ignored\n",
    );
    assert_eq!(
        got,
        vec![(
            2,
            Ok(json!({"name": "A", "check_type": "http", "url": "https://a.test/"}))
        )]
    );
    assert_eq!(
        parse(
            ImportFormat::Uptimerobot,
            "Name,Something\nx,y\n",
            &ImportDefaults::default()
        ),
        Err(ParseError::Malformed(
            "CSV header must name the columns Friendly Name, URL/IP and Type".into()
        ))
    );
    assert!(matches!(
        parse(ImportFormat::Uptimerobot, "", &ImportDefaults::default()),
        Err(ParseError::Malformed(_))
    ));
    // An unterminated quote never panics and never yields a monitor. (The csv crate is lenient: it
    // reads the rest of the input as one field instead of failing, so either outcome is accepted.)
    match parse(
        ImportFormat::Uptimerobot,
        "Friendly Name,URL/IP,Type\n\"a,https://a.test,1\n",
        &ImportDefaults::default(),
    ) {
        Err(ParseError::Malformed(_)) => {}
        Ok(entries) => assert!(entries.iter().all(|e| e.body.is_err()), "{entries:?}"),
        Err(other) => panic!("{other:?}"),
    }
    // A row without a target cell (short row, flexible reader) is a per-entry error, not a panic.
    assert_eq!(
        bodies(ImportFormat::Uptimerobot, "Friendly Name,URL/IP,Type\nA\n")[0].1,
        Err("row has no URL/IP".to_string())
    );
}

// ---- parser safety ----

#[test]
fn oversized_content_is_refused_for_every_format() {
    let big = "a".repeat(MAX_IMPORT_BODY_BYTES + 1);
    for format in [
        ImportFormat::Urls,
        ImportFormat::UptimeKuma,
        ImportFormat::Uptimerobot,
    ] {
        assert!(
            matches!(
                parse(format, &big, &ImportDefaults::default()),
                Err(ParseError::Malformed(_))
            ),
            "{format:?}"
        );
    }
    // Exactly at the cap is still read.
    let at_cap = format!("{}{}", "# ", "a".repeat(MAX_IMPORT_BODY_BYTES - 2));
    assert_eq!(
        parse(ImportFormat::Urls, &at_cap, &ImportDefaults::default()),
        Ok(vec![])
    );
}

#[test]
fn csv_over_500_rows_is_refused_with_the_real_count() {
    let mut csv = String::from("Friendly Name,URL/IP,Type\n");
    for i in 0..600 {
        csv.push_str(&format!("m{i},https://h{i}.test/,HTTP(s)\n"));
    }
    assert_eq!(
        parse(ImportFormat::Uptimerobot, &csv, &ImportDefaults::default()),
        Err(ParseError::TooManyEntries(600))
    );
    let mut ok = String::from("Friendly Name,URL/IP,Type\n");
    for i in 0..MAX_ENTRIES {
        ok.push_str(&format!("m{i},https://h{i}.test/,HTTP(s)\n"));
    }
    assert_eq!(
        parse(ImportFormat::Uptimerobot, &ok, &ImportDefaults::default())
            .unwrap()
            .len(),
        MAX_ENTRIES
    );
}

#[test]
fn deeply_nested_json_is_an_error_not_a_stack_overflow() {
    let depth = 200_000;
    let nested_list = format!(
        "{{\"monitorList\":{}{}}}",
        "[".repeat(depth),
        "]".repeat(depth)
    );
    let nested_entry = format!(
        "{{\"monitorList\":[{{\"name\":\"x\",\"type\":\"http\",\"x\":{}{}}}]}}",
        "[".repeat(depth),
        "]".repeat(depth)
    );
    let only_open = format!("{{\"monitorList\":[{}", "[".repeat(1_000_000));
    for doc in [nested_list, nested_entry, only_open] {
        assert!(doc.len() <= MAX_IMPORT_BODY_BYTES);
        match parse(ImportFormat::UptimeKuma, &doc, &ImportDefaults::default()) {
            Err(ParseError::Malformed(m)) => assert!(m.starts_with("content is not valid JSON")),
            other => panic!("{other:?}"),
        }
    }
    // Nesting inside the headers string is parsed separately and is bounded the same way.
    let headers = format!("{}{}", "[".repeat(100_000), "]".repeat(100_000));
    let doc = json!({"monitorList": [{"name": "x", "type": "http", "url": "https://x.test", "headers": headers}]});
    let got = bodies(ImportFormat::UptimeKuma, &doc.to_string());
    assert_eq!(
        got[0].1,
        Err("headers is not a JSON object of strings".to_string())
    );
}

#[test]
fn huge_json_within_the_cap_is_bounded_by_the_entry_count() {
    // ~1.5 MiB of one-element entries: refused on count, nothing mapped.
    let doc = format!("{{\"monitorList\":[{}]}}", vec!["0"; 700_000].join(","));
    assert!(doc.len() < MAX_IMPORT_BODY_BYTES);
    assert_eq!(
        parse(ImportFormat::UptimeKuma, &doc, &ImportDefaults::default()),
        Err(ParseError::TooManyEntries(700_000))
    );
    // 500 entries with ~3.5 KiB names each: parsed, and validation rejects the names.
    let name = "n".repeat(3_500);
    let entries: Vec<Value> = (0..MAX_ENTRIES)
        .map(|_| json!({"name": name, "type": "ping", "hostname": "h.test"}))
        .collect();
    let doc = json!({ "monitorList": entries }).to_string();
    assert!(doc.len() < MAX_IMPORT_BODY_BYTES);
    let got = parse(ImportFormat::UptimeKuma, &doc, &ImportDefaults::default()).unwrap();
    assert_eq!(got.len(), MAX_ENTRIES);
    let body = Value::Object(got[0].body.clone().unwrap());
    assert!(validated(&body).unwrap_err().contains("at most 200"));
}

#[test]
fn csv_formula_cells_stay_plain_text_and_are_never_interpreted() {
    let cells = [
        "=HYPERLINK(\"http://evil.test\",\"click\")",
        "+SUM(1+1)",
        "-2+3",
        "@SUM(A1)",
        "=cmd|' /C calc'!A0",
    ];
    for cell in cells {
        let mut writer = csv::Writer::from_writer(vec![]);
        writer
            .write_record(["Friendly Name", "URL/IP", "Type", "Keyword Value"])
            .unwrap();
        writer
            .write_record([cell, "https://a.test/", "Keyword", cell])
            .unwrap();
        let content = String::from_utf8(writer.into_inner().unwrap()).unwrap();
        let got = bodies(ImportFormat::Uptimerobot, &content);
        let body = got[0].1.as_ref().unwrap();
        // Verbatim: not stripped, prefixed, escaped or evaluated.
        assert_eq!(body["name"], cell);
        assert_eq!(body["keyword"], cell);
        // A formula in the target cell is no URL, so it never becomes a checkable monitor.
        let mut writer = csv::Writer::from_writer(vec![]);
        writer
            .write_record(["Friendly Name", "URL/IP", "Type"])
            .unwrap();
        writer.write_record(["n", cell, "HTTP(s)"]).unwrap();
        let content = String::from_utf8(writer.into_inner().unwrap()).unwrap();
        let body = bodies(ImportFormat::Uptimerobot, &content)[0]
            .1
            .clone()
            .unwrap();
        assert_eq!(body["url"], cell);
        assert!(validated(&body).is_err(), "{cell}");
    }
}

#[test]
fn invisible_and_bidi_characters_in_names_are_refused_by_validation() {
    for bad in [
        "evil\u{202e}gpj.exe",
        "zero\u{200b}width",
        "iso\u{2066}late",
        "line\u{2028}sep",
        "bom\u{feff}name",
    ] {
        let kuma = json!({"monitorList": [{"name": bad, "type": "ping", "hostname": "h.test"}]});
        let got = bodies(ImportFormat::UptimeKuma, &kuma.to_string());
        let body = got[0].1.as_ref().expect("parsers do not judge names");
        assert_eq!(body["name"], bad);
        assert!(
            validated(body).unwrap_err().contains("invisible"),
            "kuma {bad:?}"
        );

        let mut writer = csv::Writer::from_writer(vec![]);
        writer
            .write_record(["Friendly Name", "URL/IP", "Type"])
            .unwrap();
        writer.write_record([bad, "h.test", "Ping"]).unwrap();
        let csv = String::from_utf8(writer.into_inner().unwrap()).unwrap();
        let body = bodies(ImportFormat::Uptimerobot, &csv)[0]
            .1
            .clone()
            .unwrap();
        assert!(
            validated(&body).unwrap_err().contains("invisible"),
            "csv {bad:?}"
        );
    }
}

#[test]
fn urls_in_every_format_go_through_the_monitor_url_validation() {
    // Valid ones pass.
    let ok = bodies(ImportFormat::Urls, "https://example.com/health?x=1")[0]
        .1
        .clone()
        .unwrap();
    assert!(validated(&ok).is_ok());
    // Bidi characters survive URL parsing (percent-encoding is not applied to the text we keep)
    // and are refused by validation, not silently stored.
    let bidi = bodies(ImportFormat::Urls, "https://example.com/a\u{202e}b")[0]
        .1
        .clone()
        .unwrap();
    assert_eq!(bidi["url"], "https://example.com/a\u{202e}b");
    assert!(validated(&bidi).unwrap_err().contains("url"));
    // Non-http schemes never become entries' bodies.
    for line in [
        "javascript:alert(1)",
        "file:///etc/passwd",
        "gopher://x.test/",
    ] {
        assert!(bodies(ImportFormat::Urls, line)[0].1.is_err(), "{line}");
    }
    // Kuma and CSV URLs are validated like typed ones.
    let kuma = json!({"monitorList": [{"name": "x", "type": "http", "url": "file:///etc/passwd"}]});
    let body = bodies(ImportFormat::UptimeKuma, &kuma.to_string())[0]
        .1
        .clone()
        .unwrap();
    assert!(validated(&body).is_err());
    let body = bodies(
        ImportFormat::Uptimerobot,
        "Friendly Name,URL/IP,Type\nx,ftp://a.test/,HTTP(s)\n",
    )[0]
    .1
    .clone()
    .unwrap();
    assert!(validated(&body).is_err());
}

#[test]
fn kuma_status_range_and_dns_type_errors_do_not_echo_long_input() {
    let long = "z".repeat(500);
    let doc = json!({"monitorList": [
        {"name": "a", "type": "http", "url": "https://a.test", "accepted_statuscodes": [long]},
        {"name": "b", "type": "dns", "hostname": "a.test", "dns_resolve_type": long},
        {"name": "c", "type": long},
    ]});
    for (_, body) in bodies(ImportFormat::UptimeKuma, &doc.to_string()) {
        assert!(body.unwrap_err().len() < 120);
    }
}

// ---- review follow-ups ----

#[test]
fn kuma_flags_accept_bools_numbers_and_strings() {
    let doc = json!({"monitorList": [
        {"name": "a", "type": "http", "url": "https://a.test", "upsideDown": 1},
        {"name": "b", "type": "http", "url": "https://b.test", "upsideDown": "true"},
        {"name": "c", "type": "keyword", "url": "https://c.test", "keyword": "k", "invertKeyword": 1},
        {"name": "d", "type": "keyword", "url": "https://d.test", "keyword": "k", "invertKeyword": "true"},
        {"name": "e", "type": "http", "url": "https://e.test", "ignoreTls": 1},
        {"name": "f", "type": "http", "url": "https://f.test", "ignoreTls": "0", "active": "false"},
        {"name": "g", "type": "http", "url": "https://g.test", "upsideDown": 0, "active": 0},
    ]});
    let got = bodies(ImportFormat::UptimeKuma, &doc.to_string());
    assert_eq!(got[0].1, Err("upside-down mode is not supported".into()));
    assert_eq!(got[1].1, Err("upside-down mode is not supported".into()));
    assert_eq!(got[2].1.as_ref().unwrap()["keyword_mode"], "not_contains");
    assert_eq!(got[3].1.as_ref().unwrap()["keyword_mode"], "not_contains");
    assert_eq!(got[4].1.as_ref().unwrap()["verify_ssl"], false);
    assert_eq!(got[5].1.as_ref().unwrap()["verify_ssl"], true);
    assert_eq!(got[5].1.as_ref().unwrap()["paused"], true);
    assert_eq!(got[6].1.as_ref().unwrap()["paused"], true);
}

#[test]
fn urls_strip_a_leading_bom() {
    let got = bodies(ImportFormat::Urls, "\u{feff}https://a.test/\n");
    assert_eq!(got[0].1.as_ref().unwrap()["url"], "https://a.test/");
}

#[test]
fn error_text_drops_invisible_characters_from_quoted_input() {
    let doc = json!({"monitorList": [{"name": "a", "type": "do\u{202e}cker"}]});
    let err = bodies(ImportFormat::UptimeKuma, &doc.to_string())[0]
        .1
        .clone()
        .unwrap_err();
    assert_eq!(err, "Uptime Kuma monitor type \"docker\" is not supported");
}

#[test]
fn debug_output_masks_header_values_and_the_password() {
    let content = std::fs::read_to_string("tests/fixtures/import/uptime_kuma.json").unwrap();
    let entries = parse(
        ImportFormat::UptimeKuma,
        &content,
        &ImportDefaults::default(),
    )
    .unwrap();
    let rendered = format!("{entries:?}");
    assert!(!rendered.contains("kuma-header-secret") && !rendered.contains("kuma-basic-secret"));
    assert!(rendered.contains("X-Api-Key") && rendered.contains("<set>"));
    // The real values are still in the body.
    assert_eq!(
        entries[0].body.as_ref().unwrap()["auth_password"],
        "kuma-basic-secret"
    );
}

#[test]
fn no_error_path_echoes_a_password_or_header_value() {
    let docs = [
        json!({"name": "a", "type": "http", "url": "https://a.test", "authMethod": "ntlm",
               "basic_auth_user": "u", "basic_auth_pass": "PW-SECRET",
               "headers": "{\"X\": \"HDR-SECRET\"}"}),
        json!({"name": "b", "type": "http", "url": "https://b.test", "upsideDown": true,
               "basic_auth_pass": "PW-SECRET", "headers": "{\"X\": \"HDR-SECRET\"}"}),
        json!({"name": "c", "type": "http", "url": "https://c.test", "accepted_statuscodes": ["x"],
               "basic_auth_pass": "PW-SECRET", "headers": "{\"X\": \"HDR-SECRET\"}"}),
        json!({"name": "d", "type": "http", "url": "https://d.test",
               "basic_auth_pass": "PW-SECRET", "headers": "{\"X\": 1, \"Y\": \"HDR-SECRET\"}"}),
        json!({"name": "e", "type": "http", "url": "https://e.test",
               "basic_auth_pass": "PW-SECRET", "headers": "HDR-SECRET"}),
        json!({"name": "f", "type": "PW-SECRET", "basic_auth_pass": "PW-SECRET"}),
    ];
    for doc in docs {
        let list = json!({"monitorList": [doc]}).to_string();
        let got = bodies(ImportFormat::UptimeKuma, &list);
        let err = got[0].1.clone().unwrap_err();
        assert!(!err.contains("HDR-SECRET"), "{err}");
        if !err.contains("monitor type") {
            assert!(!err.contains("PW-SECRET"), "{err}");
        }
    }
    // Whole-document errors never quote content.
    let bad = r#"{"monitorList": [1,], "x": "PW-SECRET"}"#;
    let err = parse(ImportFormat::UptimeKuma, bad, &ImportDefaults::default()).unwrap_err();
    assert!(!err.to_string().contains("PW-SECRET"));
}

#[test]
fn numbers_above_the_i32_range_parse_but_cannot_become_create_requests() {
    // Recorded behaviour for NAMI-UP-26: the parsers keep the wide integer
    // (no silent clamp); deserializing it as CreateMonitorRequest (i32 fields)
    // fails, and that error text must not be shown raw to the client.
    let kuma = json!({"monitorList": [
        {"name": "p", "type": "port", "hostname": "h.test", "port": 4_294_967_297u64, "interval": 99_999_999_999u64}
    ]});
    let got = bodies(ImportFormat::UptimeKuma, &kuma.to_string());
    let body = got[0].1.as_ref().unwrap();
    assert_eq!(body["port"], 4_294_967_297u64);
    assert_eq!(body["interval_seconds"], 99_999_999_999u64);
    assert!(serde_json::from_value::<CreateMonitorRequest>(body.clone()).is_err());

    let csv = "Friendly Name,URL/IP,Type,Port,Interval\nm,h.test,Port,4294967297,99999999999\n";
    let got = bodies(ImportFormat::Uptimerobot, csv);
    let body = got[0].1.as_ref().unwrap();
    assert_eq!(body["port"], 4_294_967_297u64);
    assert_eq!(body["interval_seconds"], 99_999_999_999u64);
    assert!(serde_json::from_value::<CreateMonitorRequest>(body.clone()).is_err());
}

// ---------------------------------------------------------------------------
// POST /projects/{id}/monitors/import (NAMI-UP-26)
// ---------------------------------------------------------------------------

const KUMA_FIXTURE: &str = "tests/fixtures/import/uptime_kuma.json";

fn kuma_fixture() -> String {
    std::fs::read_to_string(KUMA_FIXTURE).unwrap()
}

async fn import(
    app: &Router,
    project: Uuid,
    cookie: &HeaderValue,
    body: Value,
) -> (StatusCode, Value) {
    call(
        app,
        "POST",
        &format!("/projects/{project}/monitors/import"),
        Some(cookie),
        Some(body),
    )
    .await
}

/// `import`, returning the raw response text too (for "never echoed" checks).
async fn import_text(
    app: &Router,
    project: Uuid,
    cookie: &HeaderValue,
    body: Value,
) -> (StatusCode, String) {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/projects/{project}/monitors/import"))
                .header(header::COOKIE, cookie)
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

async fn monitor_count(pool: &sqlx::PgPool, project: Uuid) -> i64 {
    sqlx::query_scalar("select count(*) from monitors where project_id = $1")
        .bind(project)
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn seed_group(pool: &sqlx::PgPool, project: Uuid) -> Uuid {
    sqlx::query_scalar(
        "insert into monitor_groups (project_id, name, position) values ($1, 'g', 0) returning id",
    )
    .bind(project)
    .fetch_one(pool)
    .await
    .unwrap()
}

fn kuma(entries: Value) -> Value {
    json!({ "format": "uptime_kuma", "content": json!({ "monitorList": entries }).to_string() })
}

#[sqlx::test]
async fn a_url_list_creates_monitors_and_reports_every_line(pool: sqlx::PgPool) {
    let app = common::test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (_, _, member) = user_with_role(&app, &pool, project, Some("member")).await;
    let (status, body) = import(
        &app,
        project,
        &member,
        json!({
            "format": "urls",
            "content": "https://a.test/\n# skip me\nhttps://b.test/x?token=abc\nnot-a-url\nhttps://a.test/\n",
            "defaults": { "interval_seconds": 120 }
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["dry_run"], false);
    assert_eq!(
        (
            body["created"].as_u64(),
            body["skipped"].as_u64(),
            body["errors"].as_u64()
        ),
        (Some(2), Some(1), Some(1))
    );
    let results = body["results"].as_array().unwrap();
    assert_eq!(results[0]["line"], 1);
    assert_eq!(results[0]["status"], "created");
    assert_eq!(results[0]["name"], "a.test");
    assert_eq!(results[0]["target"], "https://a.test/");
    assert!(results[0]["monitor_id"].is_string());
    assert_eq!(
        results[1]["target"], "https://b.test/x?token=***",
        "targets are redacted"
    );
    assert_eq!(
        results[2],
        json!({ "line": 4, "status": "error", "error": "line is not a valid http(s) URL" })
    );
    assert_eq!(results[3]["line"], 5);
    assert_eq!(results[3]["status"], "skipped");
    assert_eq!(results[3]["error"], "already monitored");
    assert!(results[3].get("monitor_id").is_none());
    assert_eq!(monitor_count(&pool, project).await, 2);
    let interval: i32 =
        sqlx::query_scalar("select interval_seconds from monitors where project_id = $1 limit 1")
            .bind(project)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(interval, 120);
    // Created exactly like a single create: due at once, through the worker.
    let id: Uuid = results[0]["monitor_id"].as_str().unwrap().parse().unwrap();
    let due: bool = sqlx::query_scalar("select next_check_at <= now() from monitors where id = $1")
        .bind(id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert!(due);
}

#[sqlx::test]
async fn importing_the_same_list_twice_creates_nothing_the_second_time(pool: sqlx::PgPool) {
    let app = common::test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (_, _, member) = user_with_role(&app, &pool, project, Some("member")).await;
    let list = json!({ "format": "urls", "content": "https://a.test/\nhttps://b.test/" });
    let (status, first) = import(&app, project, &member, list.clone()).await;
    assert_eq!(status, StatusCode::OK, "{first}");
    assert_eq!(first["created"], 2);
    let (status, second) = import(&app, project, &member, list).await;
    assert_eq!(status, StatusCode::OK, "{second}");
    assert_eq!(
        (second["created"].as_u64(), second["skipped"].as_u64()),
        (Some(0), Some(2))
    );
    for row in second["results"].as_array().unwrap() {
        assert_eq!(row["status"], "skipped");
        assert_eq!(row["error"], "already monitored");
    }
    assert_eq!(monitor_count(&pool, project).await, 2);
    let (status, third) = import(
        &app,
        project,
        &member,
        json!({ "format": "urls", "content": "https://a.test/\nhttps://b.test/", "skip_existing": false }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{third}");
    assert_eq!(third["created"], 2);
    assert_eq!(monitor_count(&pool, project).await, 4);
}

#[sqlx::test]
async fn host_based_monitors_skip_by_lowercased_host_and_port(pool: sqlx::PgPool) {
    let app = common::test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (_, _, member) = user_with_role(&app, &pool, project, Some("member")).await;
    let entries = json!([
        { "name": "db", "type": "port", "hostname": "DB.example.com", "port": 5432 },
        { "name": "db again", "type": "port", "hostname": "db.example.com", "port": 5432 },
        { "name": "db other port", "type": "port", "hostname": "db.example.com", "port": 6432 },
        { "name": "gw", "type": "ping", "hostname": "gw.example.com" },
    ]);
    let (status, body) = import(&app, project, &member, kuma(entries)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let statuses: Vec<&str> = body["results"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["status"].as_str().unwrap())
        .collect();
    assert_eq!(statuses, ["created", "skipped", "created", "created"]);
    assert_eq!(body["results"][0]["target"], "DB.example.com:5432");
    assert_eq!(body["results"][3]["target"], "gw.example.com");
}

#[test]
fn the_skip_key_is_the_url_or_the_lowercased_host_and_port() {
    let config = |body: Value| validated(&body).unwrap();
    assert_eq!(
        existing_key(&config(
            json!({"name": "a", "check_type": "http", "url": " https://A.test/x "})
        )),
        "https://A.test/x"
    );
    assert_eq!(
        existing_key(&config(
            json!({"name": "a", "check_type": "tcp", "host": "DB.test", "port": 5432})
        )),
        "db.test:5432"
    );
    assert_eq!(
        existing_key(&config(
            json!({"name": "a", "check_type": "ping", "host": "GW.test"})
        )),
        "gw.test"
    );
}

#[sqlx::test]
async fn a_dry_run_validates_everything_and_creates_nothing(pool: sqlx::PgPool) {
    let app = common::test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (_, _, member) = user_with_role(&app, &pool, project, Some("member")).await;
    let (status, body) = import(
        &app,
        project,
        &member,
        json!({ "format": "uptime_kuma", "content": kuma_fixture(), "dry_run": true }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["dry_run"], true);
    let results = body["results"].as_array().unwrap();
    assert_eq!(results.len(), 8);
    for row in &results[..5] {
        assert_eq!(row["status"], "created", "{row}");
        assert!(row.get("monitor_id").is_none(), "{row}");
        assert!(row["name"].is_string());
    }
    for row in &results[5..] {
        assert_eq!(row["status"], "error", "{row}");
    }
    assert_eq!(
        (body["created"].as_u64(), body["errors"].as_u64()),
        (Some(5), Some(3))
    );
    assert_eq!(monitor_count(&pool, project).await, 0);
    // A repeat inside the input is skipped in the preview too.
    let (_, body) = import(
        &app,
        project,
        &member,
        json!({ "format": "urls", "content": "https://a.test/\nhttps://a.test/", "dry_run": true }),
    )
    .await;
    assert_eq!(body["results"][1]["status"], "skipped");
    assert_eq!(monitor_count(&pool, project).await, 0);
}

#[sqlx::test]
async fn kuma_secrets_are_sealed_never_echoed_and_notifications_never_stored(pool: sqlx::PgPool) {
    let app = common::test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (_, _, member) = user_with_role(&app, &pool, project, Some("member")).await;
    let (status, text) = import_text(
        &app,
        project,
        &member,
        json!({ "format": "uptime_kuma", "content": kuma_fixture() }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{text}");
    for secret in [
        "kuma-header-secret",
        "kuma-basic-secret",
        "KUMA-NOTIFY-SECRET",
        "hooks.slack.com",
    ] {
        assert!(!text.contains(secret), "{secret} echoed: {text}");
    }
    let body: Value = serde_json::from_str(&text).unwrap();
    let id: Uuid = body["results"][0]["monitor_id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    let (names, headers_enc, password_enc): (Vec<String>, Vec<u8>, Vec<u8>) = sqlx::query_as(
        "select request_header_names, request_headers_enc, auth_password_enc from monitors where id = $1",
    )
    .bind(id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(names, ["X-Api-Key"]);
    let cipher = test_cipher();
    let headers = open_headers(&cipher, id, &headers_enc).unwrap();
    assert_eq!(headers[0].value.expose(), "kuma-header-secret");
    assert_eq!(
        open_password(&cipher, id, &password_enc).unwrap().expose(),
        "kuma-basic-secret"
    );
    for sealed in [&headers_enc, &password_enc] {
        let raw = String::from_utf8_lossy(sealed);
        assert!(!raw.contains("kuma-header-secret") && !raw.contains("kuma-basic-secret"));
    }
    let dump: Vec<String> =
        sqlx::query_scalar("select to_jsonb(m)::text from monitors m where project_id = $1")
            .bind(project)
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(dump.len(), 5);
    for row in dump {
        assert!(
            !row.contains("KUMA-NOTIFY-SECRET") && !row.contains("hooks.slack.com"),
            "{row}"
        );
        assert!(
            !row.contains("kuma-header-secret") && !row.contains("kuma-basic-secret"),
            "{row}"
        );
    }
}

#[sqlx::test]
async fn kuma_extra_status_ranges_are_reported_as_a_warning(pool: sqlx::PgPool) {
    let app = common::test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (_, _, member) = user_with_role(&app, &pool, project, Some("member")).await;
    let (_, body) = import(
        &app,
        project,
        &member,
        json!({ "format": "uptime_kuma", "content": kuma_fixture(), "dry_run": true }),
    )
    .await;
    let results = body["results"].as_array().unwrap();
    assert!(results[0].get("warning").is_none(), "one range, no warning");
    assert_eq!(
        results[1]["warning"],
        "only the first of 2 accepted_statuscodes ranges was imported; adjust the expected status range if the others matter"
    );
}

/// The error a single `POST /projects/{id}/monitors` gives for `body`.
async fn single_create_error(
    app: &Router,
    project: Uuid,
    cookie: &HeaderValue,
    body: &Value,
) -> String {
    let (status, reply) = call(
        app,
        "POST",
        &format!("/projects/{project}/monitors"),
        Some(cookie),
        Some(body.clone()),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{reply}");
    reply["error"].as_str().unwrap().to_string()
}

#[sqlx::test]
async fn every_entry_is_validated_like_a_single_create(pool: sqlx::PgPool) {
    let app = common::test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (_, _, member) = user_with_role(&app, &pool, project, Some("member")).await;
    let entries = json!([
        { "name": "inject", "type": "http", "url": "https://a.test/", "headers": "{\"X-Bad\\r\\nInjected\": \"v\"}" },
        { "name": "host", "type": "http", "url": "https://b.test/", "headers": "{\"Host\": \"evil\"}" },
        { "name": "js", "type": "http", "url": "javascript:alert(1)" },
        { "name": "\u{202E}gnp.exe", "type": "http", "url": "https://c.test/" },
        { "name": "fast", "type": "http", "url": "https://d.test/", "interval": 10 },
        { "name": "trace", "type": "http", "url": "https://e.test/", "method": "TRACE" },
    ]);
    let parsed = parse(
        ImportFormat::UptimeKuma,
        &json!({ "monitorList": entries.clone() }).to_string(),
        &ImportDefaults::default(),
    )
    .unwrap();
    let (status, body) = import(&app, project, &member, kuma(entries)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let results = body["results"].as_array().unwrap();
    for i in [0, 1, 2, 3, 5] {
        let single = Value::Object(parsed[i].body.clone().unwrap());
        let expected = single_create_error(&app, project, &member, &single).await;
        assert_eq!(results[i]["status"], "error", "{}", results[i]);
        assert_eq!(results[i]["error"], expected.as_str(), "entry {}", i + 1);
        assert!(
            results[i].get("name").is_none(),
            "unvalidated names are never shown"
        );
    }
    assert_eq!(results[4]["status"], "created");
    let interval: i32 =
        sqlx::query_scalar("select interval_seconds from monitors where project_id = $1")
            .bind(project)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(interval, 30);
    assert_eq!(monitor_count(&pool, project).await, 1);
}

#[sqlx::test]
async fn bad_entries_are_entry_errors_never_a_500(pool: sqlx::PgPool) {
    let app = common::test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (_, _, member) = user_with_role(&app, &pool, project, Some("member")).await;
    let many_headers: serde_json::Map<String, Value> = (0..21)
        .map(|i| (format!("X-H{i}"), Value::from("v")))
        .collect();
    let cases: Vec<(Value, &str)> = vec![
        (
            json!({ "name": "blank", "type": "http", "url": "https://a.test/", "headers": "{\"X-A\": \"  \"}" }),
            "header X-A: enter a value",
        ),
        (
            json!({ "name": "dup", "type": "http", "url": "https://a.test/", "headers": "{\"X-A\": \"1\", \"x-a\": \"2\"}" }),
            "is listed twice",
        ),
        (
            json!({ "name": "host", "type": "http", "url": "https://a.test/", "headers": "{\"Host\": \"h\"}" }),
            "header Host cannot be set",
        ),
        (
            json!({ "name": "conn", "type": "http", "url": "https://a.test/", "headers": "{\"Connection\": \"close\"}" }),
            "header Connection cannot be set",
        ),
        (
            json!({ "name": "clen", "type": "http", "url": "https://a.test/", "headers": "{\"Content-Length\": \"1\"}" }),
            "header Content-Length cannot be set",
        ),
        (
            json!({ "name": "te", "type": "http", "url": "https://a.test/", "headers": "{\"TE\": \"trailers\"}" }),
            "header TE cannot be set",
        ),
        (
            json!({ "name": "expect", "type": "http", "url": "https://a.test/", "headers": "{\"Expect\": \"100-continue\"}" }),
            "header Expect cannot be set",
        ),
        (
            json!({ "name": "many", "type": "http", "url": "https://a.test/", "headers": Value::Object(many_headers).to_string() }),
            "at most 20 request headers are allowed",
        ),
        (
            json!({ "name": "big interval", "type": "http", "url": "https://a.test/", "interval": 99_999_999_999u64 }),
            UNREADABLE_ENTRY,
        ),
        (
            json!({ "name": "big port", "type": "port", "hostname": "h.test", "port": 4_294_967_297u64 }),
            UNREADABLE_ENTRY,
        ),
        (
            json!({ "name": "get body", "type": "http", "url": "https://a.test/", "method": "GET", "body": "{}" }),
            "request_body is only allowed with POST, PUT or PATCH",
        ),
        (
            json!({ "name": "head kw", "type": "keyword", "url": "https://a.test/", "method": "HEAD", "keyword": "ok" }),
            "keyword monitors cannot use HEAD",
        ),
        (
            json!({ "name": "slow", "type": "http", "url": "https://a.test/", "timeout": 60 }),
            "timeout_seconds must be less than interval_seconds",
        ),
        (
            json!({ "name": "dns ip", "type": "dns", "hostname": "1.1.1.1" }),
            "host must be a domain name to look up",
        ),
    ];
    let entries: Vec<Value> = cases.iter().map(|(entry, _)| entry.clone()).collect();
    let (status, body) = import(&app, project, &member, kuma(Value::Array(entries))).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let results = body["results"].as_array().unwrap();
    for (i, (_, expected)) in cases.iter().enumerate() {
        assert_eq!(results[i]["status"], "error", "case {i}: {}", results[i]);
        let error = results[i]["error"].as_str().unwrap();
        assert!(error.contains(expected), "case {i}: {error}");
    }
    assert_eq!(monitor_count(&pool, project).await, 0);

    // The default timeout reaching the default interval is an entry error too.
    let (_, body) = import(
        &app,
        project,
        &member,
        json!({ "format": "urls", "content": "https://a.test/", "defaults": { "timeout_seconds": 60 } }),
    )
    .await;
    assert_eq!(
        body["results"][0]["error"],
        "timeout_seconds must be less than interval_seconds"
    );

    // A Kuma password without a username is dropped by the parser, so no
    // password is stored for it (validation would refuse it otherwise).
    let (_, body) = import(
        &app,
        project,
        &member,
        kuma(json!([{ "name": "pw", "type": "http", "url": "https://p.test/", "basic_auth_pass": "lonely" }])),
    )
    .await;
    assert_eq!(body["results"][0]["status"], "created", "{body}");
    let stored: Option<Vec<u8>> =
        sqlx::query_scalar("select auth_password_enc from monitors where project_id = $1")
            .bind(project)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(stored.is_none());
}

#[sqlx::test]
async fn entry_errors_never_echo_a_password_or_header_value(pool: sqlx::PgPool) {
    let app = common::test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (_, _, member) = user_with_role(&app, &pool, project, Some("member")).await;
    let entries = json!([
        { "name": "cr", "type": "http", "url": "https://a.test/", "headers": "{\"X-A\": \"HDR-SECRET\\r\\nX: y\"}" },
        { "name": "ctl", "type": "http", "url": "https://a.test/", "headers": "{\"X-A\": \"HDR-SECRET\\u0001\"}" },
        { "name": "dup", "type": "http", "url": "https://a.test/", "headers": "{\"X-A\": \"HDR-SECRET\", \"x-a\": \"HDR-SECRET\"}" },
        { "name": "pwctl", "type": "http", "url": "https://a.test/", "basic_auth_user": "u", "basic_auth_pass": "PW-SECRET\u{1}" },
        { "name": "both", "type": "http", "url": "https://a.test/", "basic_auth_user": "u", "basic_auth_pass": "PW-SECRET",
          "headers": "{\"Authorization\": \"HDR-SECRET\"}" },
        { "name": "range", "type": "http", "url": "https://a.test/", "basic_auth_user": "u", "basic_auth_pass": "PW-SECRET",
          "headers": "{\"X-A\": \"HDR-SECRET\"}", "interval": 99_999_999_999u64 },
    ]);
    let (status, text) = import_text(&app, project, &member, kuma(entries)).await;
    assert_eq!(status, StatusCode::OK, "{text}");
    assert!(!text.contains("HDR-SECRET"), "{text}");
    assert!(!text.contains("PW-SECRET"), "{text}");
    let body: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(body["errors"], 6, "{text}");
    assert_eq!(monitor_count(&pool, project).await, 0);
}

#[sqlx::test]
async fn import_limits_are_enforced_before_anything_is_created(pool: sqlx::PgPool) {
    let app = common::test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (_, _, member) = user_with_role(&app, &pool, project, Some("member")).await;
    let many: String = (0..=MAX_ENTRIES)
        .map(|i| format!("https://h{i}.test/\n"))
        .collect();
    let (status, body) = import(
        &app,
        project,
        &member,
        json!({ "format": "urls", "content": many }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(
        body,
        json!({ "error": "at most 500 entries per import, got 501" })
    );
    // Content over 2 MiB inside a body the route accepts: 413 from the handler.
    let content = "#".repeat(MAX_IMPORT_BODY_BYTES + 1);
    let (status, body) = import(
        &app,
        project,
        &member,
        json!({ "format": "urls", "content": content }),
    )
    .await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE, "{body}");
    assert_eq!(body, json!({ "error": "content is larger than 2 MiB" }));
    // A body over the route limit: 413 before the handler.
    let content = "#".repeat(MAX_IMPORT_REQUEST_BYTES + 1);
    let (status, _) = import(
        &app,
        project,
        &member,
        json!({ "format": "urls", "content": content }),
    )
    .await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
    // Exactly 2 MiB of content is accepted (escaping included).
    let content = format!(
        "https://a.test/\n{}",
        "\"".repeat(MAX_IMPORT_BODY_BYTES - 16)
    );
    assert_eq!(content.len(), MAX_IMPORT_BODY_BYTES);
    let (status, body) = import(
        &app,
        project,
        &member,
        json!({ "format": "urls", "content": content, "dry_run": true }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body) = import(
        &app,
        project,
        &member,
        json!({ "format": "uptime_kuma", "content": "{\"monitorList\": [" }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(
        body["error"]
            .as_str()
            .unwrap()
            .starts_with("content is not valid JSON"),
        "{body}"
    );
    let (status, _) = import(
        &app,
        project,
        &member,
        json!({ "format": "pingdom", "content": "x" }),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(monitor_count(&pool, project).await, 0);
}

#[sqlx::test]
async fn a_second_import_while_one_runs_is_409(pool: sqlx::PgPool) {
    let app = common::test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (_, _, member) = user_with_role(&app, &pool, project, Some("member")).await;
    let mut held = pool.begin().await.unwrap();
    let taken: bool =
        sqlx::query_scalar("select pg_try_advisory_xact_lock($1, hashtext($2::text))")
            .bind(IMPORT_LOCK_CLASS)
            .bind(project)
            .fetch_one(&mut *held)
            .await
            .unwrap();
    assert!(taken);
    let list = json!({ "format": "urls", "content": "https://a.test/" });
    let (status, body) = import(&app, project, &member, list.clone()).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(
        body,
        json!({ "error": "another import is running in this project; try again when it finishes" })
    );
    assert_eq!(monitor_count(&pool, project).await, 0);
    // Another project is not blocked.
    let other = seed_project(&pool).await;
    let (_, _, other_member) = user_with_role(&app, &pool, other, Some("member")).await;
    let (status, _) = import(&app, other, &other_member, list.clone()).await;
    assert_eq!(status, StatusCode::OK);
    // Dry runs take no lock.
    let (status, body) = import(
        &app,
        project,
        &member,
        json!({ "format": "urls", "content": "https://a.test/", "dry_run": true }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    held.rollback().await.unwrap();
    let (status, body) = import(&app, project, &member, list.clone()).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["created"], 1);
    // The lock was released when that import finished.
    let (status, body) = import(&app, project, &member, list).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["skipped"], 1);
}

#[sqlx::test]
async fn import_rbac_and_foreign_groups(pool: sqlx::PgPool) {
    let app = common::test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (_, _, viewer) = user_with_role(&app, &pool, project, Some("viewer")).await;
    let (_, _, outsider) = user_with_role(&app, &pool, project, None).await;
    let (_, _, member) = user_with_role(&app, &pool, project, Some("member")).await;
    let (_, _, admin) = user_with_role(&app, &pool, project, Some("admin")).await;
    let list = json!({ "format": "urls", "content": "https://a.test/\nhttps://b.test/" });
    for cookie in [&viewer, &outsider] {
        let (status, _) = import(&app, project, cookie, list.clone()).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        let (status, _) = import(
            &app,
            project,
            cookie,
            json!({ "format": "urls", "content": "https://a.test/", "dry_run": true }),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
    }
    let (status, _) = call(
        &app,
        "POST",
        &format!("/projects/{project}/monitors/import"),
        None,
        Some(list.clone()),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(monitor_count(&pool, project).await, 0);

    let other = seed_project(&pool).await;
    let foreign = seed_group(&pool, other).await;
    let (status, _) = import(
        &app,
        project,
        &member,
        json!({ "format": "urls", "content": "https://a.test/", "defaults": { "group_id": foreign } }),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(monitor_count(&pool, project).await, 0);

    let own = seed_group(&pool, project).await;
    let (status, body) = import(
        &app,
        project,
        &admin,
        json!({ "format": "urls", "content": "https://a.test/\nhttps://b.test/", "defaults": { "group_id": own } }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let groups: Vec<Option<Uuid>> =
        sqlx::query_scalar("select group_id from monitors where project_id = $1")
            .bind(project)
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(groups, [Some(own), Some(own)]);
}

#[sqlx::test]
async fn one_failing_entry_never_rolls_back_the_others(pool: sqlx::PgPool) {
    let app = common::test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (_, _, member) = user_with_role(&app, &pool, project, Some("member")).await;
    let long = format!("https://b.test/{}", "x".repeat(2048));
    let (status, body) = import(
        &app,
        project,
        &member,
        json!({ "format": "urls", "content": format!("https://a.test/\n{long}\nhttps://c.test/") }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let statuses: Vec<&str> = body["results"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["status"].as_str().unwrap())
        .collect();
    assert_eq!(statuses, ["created", "error", "created"]);
    assert_eq!(
        body["results"][1]["error"],
        "url must be at most 2048 bytes"
    );
    assert_eq!(monitor_count(&pool, project).await, 2);
}
