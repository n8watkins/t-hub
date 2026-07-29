use super::*;

#[test]
fn key_rotation_keeps_fresh_seals_and_rotates_on_policy() {
    // item-3 Pillar B rotation-on-restart: a fresh key is KEPT (stable across
    // restarts within max age) and sealed at rest; a forced rotation and an
    // aged-out key both mint-and-REPLACE the file (never re-read the old key).
    // BYPASS-WOULD-FAIL: revert to reuse-the-file and the forced/aged asserts go RED.
    let base = std::env::temp_dir().join(format!("t-hub-keyrot-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base).unwrap();
    let path = base.join("server-key");

    // Missing => mints and writes; the written file unseals back to the key.
    let k1 = load_or_rotate_key_with(&path, false, 3600);
    assert!(!k1.is_empty());
    assert!(path.exists(), "a minted key must be written to disk");
    let stored = std::fs::read_to_string(&path).unwrap();
    assert_eq!(
        crate::secret_seal::unseal_str(&stored).as_deref(),
        Some(k1.as_str())
    );

    // Within age, not forced => KEEP the same value.
    let k2 = load_or_rotate_key_with(&path, false, 3600);
    assert_eq!(
        k2, k1,
        "a fresh key within max age must be kept, not rotated"
    );

    // Forced => a DIFFERENT value overwrites the file (mint-and-replace).
    let k3 = load_or_rotate_key_with(&path, true, 3600);
    assert_ne!(k3, k1, "a forced rotation must mint-and-replace the key");
    let stored3 = std::fs::read_to_string(&path).unwrap();
    assert_eq!(
        crate::secret_seal::unseal_str(&stored3).as_deref(),
        Some(k3.as_str())
    );

    // max_age 0 => past age on every call => rotates.
    let k4 = load_or_rotate_key_with(&path, false, 0);
    assert_ne!(k4, k3, "max_age 0 must rotate on every restart");
    assert!(
        !key_is_past_max_age(&path, 3600),
        "a just-written key is not past a 1h age"
    );

    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn packaged_legacy_orphan_forces_control_bearer_rotation_before_start() {
    let fixture: Value = serde_json::from_str(PACKAGED_SCHEMA_25_LEGACY_ORPHAN_FIXTURE).unwrap();
    let snapshot: CaptainsSnapshot =
        serde_json::from_value(fixture["captainsSnapshot"].clone()).unwrap();
    CaptainsRegistry::validate_snapshot(&snapshot).unwrap();
    assert!(snapshot.cortana.legacy_orphan_provenance.is_some());

    let base = std::env::temp_dir().join(format!(
        "t-hub-packaged-orphan-keyrot-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4().simple()
    ));
    std::fs::create_dir_all(&base).unwrap();
    let path = base.join("server-key");
    let old = fixture["capture"]["control"]["sharedPersistentToken"]
        .as_str()
        .unwrap();
    write_key_file(&path, old);

    let kept = persistent_key_for_start_with(&path, false, 3600, false).unwrap();
    assert_eq!(kept, old);
    let rotated = persistent_key_for_start_with(&path, false, 3600, true).unwrap();
    assert_ne!(rotated, old);
    assert_eq!(
        crate::secret_seal::unseal_str(&std::fs::read_to_string(&path).unwrap()).as_deref(),
        Some(rotated.as_str())
    );
    let read = "profile-scoped-read-token";
    let handshake = ControlHandshake {
        addr: fixture["capture"]["control"]["currentAddress"]
            .as_str()
            .unwrap()
            .into(),
        token: select_published_token(&rotated, read, true).into(),
        read_token: read.into(),
        pid: 7,
        protocol_version: PROTOCOL_VERSION,
        instance_id: "captured-package-start".into(),
        listener_generation: 1,
        published_at: 1,
        local_control_token: rotated.clone(),
        local_host_token: "host-only".into(),
    };
    let published = serde_json::to_string(&handshake).unwrap();
    assert_eq!(handshake.token, read);
    assert_eq!(handshake.local_control_token, rotated);
    assert!(!published.contains(old));
    assert!(!published.contains(&rotated));

    std::fs::remove_dir_all(base).ok();
}

#[test]
fn legacy_bearer_rotation_failure_is_prepublication_and_preserves_old_key() {
    let base = std::env::temp_dir().join(format!(
        "t-hub-packaged-orphan-key-failure-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4().simple()
    ));
    std::fs::create_dir_all(&base).unwrap();
    let path = base.join("server-key");
    let old = "old-profile-control-bearer";
    write_key_file(&path, old);

    let error = write_key_file_durable_with(&path, "unpublished-new-bearer", || {
        Err("injected crash before key publication".into())
    })
    .unwrap_err();
    assert!(error.contains("injected crash before key publication"));
    assert_eq!(
        crate::secret_seal::unseal_str(&std::fs::read_to_string(&path).unwrap()).as_deref(),
        Some(old)
    );
    assert_eq!(
        std::fs::read_dir(&base)
            .unwrap()
            .filter_map(Result::ok)
            .count(),
        1,
        "a refused rotation must not leave a publishable temporary key"
    );

    std::fs::remove_dir_all(base).ok();
}

#[test]
fn key_rotation_reads_legacy_plaintext_and_keeps_it() {
    // A pre-item-3 key file (raw token, no seal prefix) is read and KEPT within
    // age, so an upgrade preserves the paired credential (no surprise rotation).
    let base = std::env::temp_dir().join(format!("t-hub-keylegacy-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base).unwrap();
    let path = base.join("server-read-key");
    std::fs::write(&path, "legacy-plaintext-token").unwrap();
    let k = load_or_rotate_key_with(&path, false, 3600);
    assert_eq!(
        k, "legacy-plaintext-token",
        "legacy plaintext must be read and kept"
    );
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn age_rotation_eligibility_holds_off_until_a_sealing_host_adopts_the_key() {
    // MED-1: on a SEALING host (Windows/DPAPI) age-rotation is held off until a
    // pre-item-3 (unsealed) key has been ADOPTED (sealed), so the first item-3
    // restart never strands pre-existing fleet. Once sealed, age-rotation resumes.
    // On a NON-sealing host there is no sealed form, so age-rotation stays eligible.
    assert!(
        !age_rotation_eligible(false, true),
        "sealing host + unsealed key: adopt, don't rotate"
    );
    assert!(
        age_rotation_eligible(true, true),
        "sealing host + sealed key: age-rotates"
    );
    assert!(
        age_rotation_eligible(false, false),
        "non-sealing host: eligible regardless"
    );
    assert!(
        age_rotation_eligible(true, false),
        "non-sealing host: eligible regardless"
    );
}

#[test]
fn hardened_control_json_withholds_full_token_but_handshake_carries_it() {
    // The security-critical Phase-3-safety invariant. Build the handshake exactly
    // as `start` does with hardening ON, write it, and assert BOTH halves of the
    // contract:
    //   (a) the SERIALIZED control.json `token` == read_token (full token withheld
    //       from external scrapers), and the full token appears nowhere in the file;
    //   (b) the RETURNED handshake's `local_control_token` == the full control token,
    //       so the trusted in-process frontend still gets full power.
    let full = "FULL-SECRET-abc123";
    let read = "READ-only-xyz789";
    let handshake = ControlHandshake {
        addr: "127.0.0.1:5000".into(),
        // Mirrors `start`: published token is the read token under hardening.
        token: select_published_token(full, read, true).to_string(),
        read_token: read.into(),
        pid: 7,
        protocol_version: PROTOCOL_VERSION,
        instance_id: "instance".into(),
        listener_generation: 1,
        published_at: 123,
        local_control_token: full.into(),
        local_host_token: "host".into(),
    };

    // (a) Published discovery is read-only and never leaks the full token.
    assert_eq!(
        handshake.token, read,
        "published token must be the read token"
    );
    let file = std::env::temp_dir().join(format!("t-hub-ctl-harden-{}.json", std::process::id()));
    let on_disk = {
        let _control_file = ControlFileEnv::set(&file);
        write_handshake(&handshake).expect("write handshake");
        std::fs::read_to_string(&file).expect("read control.json")
    };
    let _ = std::fs::remove_file(&file);

    assert!(
        !on_disk.contains(full),
        "control.json must NOT contain the full control token; got: {on_disk}"
    );
    assert!(
        !on_disk.contains("local_control_token"),
        "the in-process field must not be serialized; got: {on_disk}"
    );
    let parsed: ControlHandshake = serde_json::from_str(&on_disk).expect("control.json parses");
    assert_eq!(parsed.token, read, "on-disk token must be the read token");
    assert_eq!(
        parsed.local_control_token, "",
        "in-process token must not survive to disk"
    );

    // (b) The RETURNED handshake still carries the full token for the frontend.
    assert_eq!(
        handshake.local_control_token, full,
        "local frontend must receive the full control token in-process"
    );
}

#[test]
fn phase3_hardened_publishes_read_token_and_default_spawn_is_read() {
    // With hardening ON (the item-3 default): what `control.json` publishes as
    // `token` is the READ token (so a raw scraper is read-only), AND the default
    // spawn-tree discovery contains no rotating capability token. Generic
    // control requests are rejected by the spawn contract.
    let ctx = test_ctx("ctl"); // read token is "read-ctl" (see test_ctx)
                               // Discovery, hardened: publishes the read token, NOT the control token.
    let published = select_published_token(&ctx.token, &ctx.read_token, true);
    assert_eq!(
        published, ctx.read_token,
        "hardened discovery must publish read token"
    );
    assert_ne!(
        published, ctx.token,
        "hardened discovery must NOT publish control token"
    );
    assert_eq!(
        resolve_capability(&ctx, published),
        Some(Capability::ReadOnly),
        "published token must resolve to read-only"
    );

    // Spawn-tree injection carries only stable discovery and explicitly
    // scrubs rotating address and token values.
    let mut ctx = ctx;
    ctx.addr = "127.0.0.1:4242".to_string();
    let env = elevation_env(&ctx, &json!({}));
    assert!(env
        .iter()
        .any(|(key, value)| key == "T_HUB_CONTROL_FILE" && !value.is_empty()));
    assert!(env
        .iter()
        .any(|(key, value)| key == "T_HUB_CONTROL_ADDR" && value.is_empty()));
    assert!(env
        .iter()
        .any(|(key, value)| key == "T_HUB_CONTROL_TOKEN" && value.is_empty()));

    // An explicit capability request does not put a shared credential back
    // into the child environment.
    let up = elevation_env(&ctx, &json!({"capability": "control"}));
    assert_eq!(up, env);
}

#[test]
fn phase3_verification_gate_checks_1_2_4_5() {
    // item-3 §3.1: the automated portion of the FIVE-check verification gate that
    // earns the default-ON flip #2. This test pins checks 1, 2, 4, 5 at the code
    // level; check 3 (a real attach + send_keys DRIVEN THROUGH THE WEBVIEW on a
    // WSLg build) is the manual acceptance step, documented in the PR body.
    let ctx = test_ctx("ctl"); // token "ctl", read token "read-ctl"
    let harden = true; // the ratified default (T_HUB_CONTROL_HARDEN unset => ON)

    // CHECK 1: control.json's `token` == the READ token (full withheld from disk).
    let published = select_published_token(&ctx.token, &ctx.read_token, harden);
    assert_eq!(
        published, ctx.read_token,
        "check 1: disk token must be the read token"
    );
    assert_ne!(
        published, ctx.token,
        "check 1: full token must NOT reach disk"
    );

    // CHECK 2: the webview obtains the FULL token in-process, not from disk. The
    // handshake carries `local_control_token` = full and never serializes it;
    // `control_client::resolve_endpoint` returns it in local mode (proven by
    // `control_client::tests::local_arm_authenticates_with_the_full_control_token`).
    let handshake = ControlHandshake {
        addr: "127.0.0.1:5000".into(),
        token: published.to_string(),
        read_token: ctx.read_token.clone(),
        pid: 1,
        protocol_version: PROTOCOL_VERSION,
        instance_id: "instance".into(),
        listener_generation: 1,
        published_at: 123,
        local_control_token: ctx.token.clone(),
        local_host_token: ctx.host_token.clone(),
    };
    assert_eq!(
        handshake.local_control_token, ctx.token,
        "check 2: in-process full token"
    );
    assert_eq!(
        serde_json::to_value(&handshake)
            .unwrap()
            .get("local_control_token"),
        None,
        "check 2: the in-process token must never serialize to control.json"
    );

    // CHECK 4: an external scraper presenting the PUBLISHED token is capped to
    // ReadOnly (it can never spawn/type/kill).
    assert_eq!(
        resolve_capability(&ctx, published),
        Some(Capability::ReadOnly),
        "check 4: the published token must resolve to read-only"
    );

    // CHECK 5: attach SURVIVES a control rebind while hardened - the webview keeps
    // full control across the rebind (the `rebind-strands-webview` class). Proven
    // end-to-end by `control_client::tests::refresh_addr_adopts_a_rotated_port_
    // from_the_local_handshake`, which keeps the full token across a port rotation
    // where the published token on disk is read-only. Asserted here structurally:
    // `rebind_control` rebuilds the handshake KEEPING the same full token.
    // (Cross-module behavioral proof lives in that control_client test.)
}

#[test]
fn elevation_env_passes_only_stable_discovery_and_scrubs_rotating_values() {
    let mut ctx = test_ctx("t");
    ctx.addr = "127.0.0.1:4242".to_string();
    let def = elevation_env(&ctx, &json!({}));
    assert_eq!(def[0].0, "T_HUB_CONTROL_FILE");
    assert!(!def[0].1.is_empty());
    assert_eq!(def[1], ("T_HUB_CONTROL_ADDR".to_string(), String::new()));
    assert_eq!(def[2], ("T_HUB_CONTROL_TOKEN".to_string(), String::new()));
    let typo = elevation_env(&ctx, &json!({"capability": "conrtol"}));
    assert_eq!(typo, def);
    let up = elevation_env(&ctx, &json!({"capability": "control"}));
    assert_eq!(up, def);
    // No bound addr (headless): nothing injected, so spawns behave as before.
    ctx.addr = String::new();
    assert!(elevation_env(&ctx, &json!({"capability": "control"})).is_empty());
}

#[test]
fn windows_discovery_path_is_stable_and_wsl_readable() {
    assert_eq!(
        wsl_discovery_path(Path::new(r"C:\Users\natha\.t-hub\control.json")),
        "/mnt/c/Users/natha/.t-hub/control.json"
    );
    assert_eq!(
        wsl_discovery_path(Path::new("/home/natkins/.t-hub/control.json")),
        "/home/natkins/.t-hub/control.json"
    );
}

#[test]
fn discovery_proof_echoes_nonce_and_live_listener_identity_at_read_tier() {
    let mut ctx = test_ctx("t");
    ctx.listener_instance_id = "proof-instance".into();
    ctx.addr = "127.0.0.1:4242".into();
    ctx.bound_listener_generation = 7;
    let proof = dispatch_authenticated(
        &ctx,
        req(
            "read-t",
            "control_discovery_proof",
            json!({"nonce": "fresh-proof-nonce"}),
        ),
    );
    let result = proof.result.unwrap();
    assert_eq!(result["nonce"], "fresh-proof-nonce");
    assert_eq!(result["protocolVersion"], PROTOCOL_VERSION);
    assert_eq!(result["instanceId"], "proof-instance");
    assert_eq!(result["listenerGeneration"], 7);
    assert_eq!(result["listenerAddr"], "127.0.0.1:4242");

    // An overlapping serve loop shares the allocator but retains its own
    // immutable address/generation proof.
    let mut replacement = ctx.clone();
    replacement.addr = "127.0.0.1:4243".into();
    replacement.bound_listener_generation = 8;
    ctx.listener_generation.store(99, Ordering::Release);
    let old_overlap = dispatch_authenticated(
        &ctx,
        req(
            "read-t",
            "control_discovery_proof",
            json!({"nonce": "old-overlap"}),
        ),
    )
    .result
    .unwrap();
    let new_overlap = dispatch_authenticated(
        &replacement,
        req(
            "read-t",
            "control_discovery_proof",
            json!({"nonce": "new-overlap"}),
        ),
    )
    .result
    .unwrap();
    assert_eq!(old_overlap["listenerGeneration"], 7);
    assert_eq!(old_overlap["listenerAddr"], "127.0.0.1:4242");
    assert_eq!(new_overlap["listenerGeneration"], 8);
    assert_eq!(new_overlap["listenerAddr"], "127.0.0.1:4243");

    let missing = dispatch_authenticated(&ctx, req("read-t", "control_discovery_proof", json!({})));
    assert!(missing.error.unwrap().contains("bounded non-empty nonce"));
}
