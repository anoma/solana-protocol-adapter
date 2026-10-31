//! Every event's discriminator is the hash of its signature, read from the
//! committed IDL, so a change to an event's name or field types changes its
//! discriminator.

use anchor_lang::idl::types::Idl;
use protocol_adapter::events::{event_discriminator, idl_event_signature};

fn committed_idl() -> Idl {
    serde_json::from_str(include_str!("../../../idl/protocol_adapter.json"))
        .expect("idl/protocol_adapter.json is not an Anchor IDL")
}

#[test]
fn every_event_discriminator_is_its_idl_signatures() {
    let idl = committed_idl();
    assert!(
        !idl.events.is_empty(),
        "the committed IDL declares no events"
    );
    for event in &idl.events {
        let signature = idl_event_signature(&idl, &event.name);
        let expected = event_discriminator(&signature);
        println!(
            "{}: signature {signature}, IDL discriminator {:?}, sha256(\"event:\" ++ signature)[..8] {expected:?}",
            event.name, event.discriminator
        );
        assert_eq!(
            event.discriminator, expected,
            "event {}: the IDL's discriminator {:?} is not that of its signature {signature}, {expected:?}",
            event.name, event.discriminator
        );
    }
}

#[test]
fn a_signature_is_the_name_and_ordered_field_types() {
    let idl = committed_idl();
    assert_eq!(
        idl_event_signature(&idl, "ResourcePayload"),
        "ResourcePayload([u8;32],u256,bytes)"
    );
    assert_eq!(
        idl_event_signature(&idl, "ActionExecuted"),
        "ActionExecuted([u8;32],vec<[u8;32]>,vec<[u8;32]>,vec<[u8;32]>,vec<[u8;32]>)"
    );
}

#[test]
#[should_panic(expected = "which the signature spelling does not cover")]
fn an_enum_field_has_no_signature() {
    let idl: Idl = serde_json::from_str(
        r#"{
            "address": "11111111111111111111111111111111",
            "metadata": { "name": "enum_event", "version": "0.1.0", "spec": "0.1.0" },
            "instructions": [],
            "events": [{ "name": "Chosen", "discriminator": [0, 0, 0, 0, 0, 0, 0, 0] }],
            "types": [
                {
                    "name": "Chosen",
                    "type": { "kind": "struct", "fields": [{ "name": "choice", "type": { "defined": { "name": "Choice" } } }] }
                },
                {
                    "name": "Choice",
                    "type": { "kind": "enum", "variants": [{ "name": "Left" }, { "name": "Right" }] }
                }
            ]
        }"#,
    )
    .expect("the test IDL does not parse");
    let signature = idl_event_signature(&idl, "Chosen");
    println!("Chosen spelled as {signature}");
}
