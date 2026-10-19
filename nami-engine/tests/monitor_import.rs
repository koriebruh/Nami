use nami_engine::models::monitor::{self, CreateMonitorRequest};
use nami_engine::uptime::import::{
    ImportDefaults, ImportFormat, MAX_ENTRIES, MAX_IMPORT_BODY_BYTES, ParseError, parse,
};
use serde_json::{Value, json};

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
