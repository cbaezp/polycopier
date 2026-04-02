use polycopier::api::SetupPayload;

#[test]
fn test_setup_payload_deserialization() {
    // 1. Valid payload matching exactly what the new SetupWizard.tsx sends.
    // Notice `target_wallets` is intentionally omitted compared to the old payload.
    let raw_payload = r#"{
        "funder_address": "0x01234FUNDER",
        "private_key": "0x9999PRIVATE"
    }"#;

    let payload_result = serde_json::from_str::<SetupPayload>(raw_payload);
    assert!(
        payload_result.is_ok(),
        "SetupPayload should seamlessly deserialize without target_wallets"
    );

    let payload = payload_result.unwrap();
    assert_eq!(payload.funder_address, "0x01234FUNDER");
    assert_eq!(payload.private_key, "0x9999PRIVATE");
}

#[test]
fn test_setup_payload_rejects_missing_funder() {
    // 2. Erroneous payload missing the now-primary funder address
    let bad_payload = r#"{
        "private_key": "0x9999PRIVATE"
    }"#;

    let result = serde_json::from_str::<SetupPayload>(bad_payload);
    assert!(
        result.is_err(),
        "SetupPayload MUST reject requests missing the funder_address"
    );
}

#[test]
fn test_setup_payload_tolerates_unexpected_legacy_fields() {
    // 3. Test that if a Ghost Cache client somehow sends `target_wallets`, it is safety ignored
    // (Serde ignores unknown fields by default struct logic)
    let legacy_payload = r#"{
        "funder_address": "0x01234FUNDER",
        "private_key": "0x9999PRIVATE",
        "target_wallets": "0xAAAA,0xBBBB"
    }"#;

    let result = serde_json::from_str::<SetupPayload>(legacy_payload);
    assert!(
        result.is_ok(),
        "SetupPayload should gracefully ignore legacy phantom targets if provided"
    );
}
