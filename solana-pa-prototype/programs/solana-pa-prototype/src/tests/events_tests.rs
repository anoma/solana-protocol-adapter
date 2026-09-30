use crate::events::event_instruction_data;
use crate::ActionExecutedEvent;
use anchor_lang::Discriminator;

/// The self-CPI instruction data is Anchor's 8-byte event tag, then the event's
/// own 8-byte discriminator, then the Borsh body — exactly what Anchor's
/// `emit_cpi!` builds, so any consumer written against Anchor's layout decodes it.
#[test]
fn event_instruction_data_is_tag_then_discriminator_then_borsh() {
    let event = ActionExecutedEvent {
        action_tree_root: [7u8; 32],
        action_tag_count: 3,
    };
    let data = event_instruction_data(&event).unwrap();

    // Sha256("anchor:event")[..8] as a little-endian u64: 0x1d9acb512ea545e4.
    assert_eq!(
        &data[..8],
        &[0xe4, 0x45, 0xa5, 0x2e, 0x51, 0xcb, 0x9a, 0x1d]
    );
    assert_eq!(&data[8..16], ActionExecutedEvent::DISCRIMINATOR);
    // Borsh body: 32 root bytes then the u32 count, little-endian.
    assert_eq!(&data[16..48], &[7u8; 32]);
    assert_eq!(&data[48..52], &3u32.to_le_bytes());
    assert_eq!(data.len(), 52);
}

/// A variable-length event body is Borsh-encoded in place after the tag and
/// discriminator, byte-for-byte what Anchor's own `Event::data` produces.
#[test]
fn event_instruction_data_is_tag_then_anchor_event_data_for_a_blob_event() {
    use anchor_lang::Event;
    let event = crate::ResourcePayloadEvent {
        tag: [9u8; 32],
        index: 2,
        blob: (0..=200u8).collect(),
    };
    let data = event_instruction_data(&event).unwrap();

    assert_eq!(
        data,
        [anchor_lang::event::EVENT_IX_TAG_LE, &event.data()].concat()
    );
    assert_eq!(data.len(), 8 + 8 + 32 + 4 + 4 + 201);
}
