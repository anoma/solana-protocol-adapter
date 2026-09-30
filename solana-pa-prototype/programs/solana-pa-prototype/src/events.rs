//! Settlement events as Anchor CPI events.
//!
//! Program logs are truncated by the runtime at 10 KB per transaction, so an
//! event carried in a log line can be lost while the settlement succeeds.
//! A CPI event is the instruction data of a self-invocation; it is part of the
//! transaction and cannot be truncated. The adapter emits from free functions
//! that have no `ctx` in scope, so this module performs the same invocation
//! Anchor's `emit_cpi!` macro expands to, with the authority and bump captured
//! once per instruction.

use anchor_lang::prelude::*;
use anchor_lang::solana_program::instruction::{AccountMeta, Instruction};
use anchor_lang::solana_program::program::invoke_signed;

/// Instruction data of a CPI event: Anchor's event tag, then the event's own
/// discriminator and Borsh body, serialized once into a buffer sized exactly
/// (the BPF bump allocator never frees a grown-out buffer).
pub fn event_instruction_data<E: anchor_lang::Event>(event: &E) -> Result<Vec<u8>> {
    let mut body_len = ByteCount(0);
    event.serialize(&mut body_len)?;
    let mut data = Vec::with_capacity(
        anchor_lang::event::EVENT_IX_TAG_LE.len() + E::DISCRIMINATOR.len() + body_len.0,
    );
    data.extend_from_slice(anchor_lang::event::EVENT_IX_TAG_LE);
    data.extend_from_slice(E::DISCRIMINATOR);
    event.serialize(&mut data)?;
    Ok(data)
}

/// A writer that only counts, to size a Borsh buffer before filling it.
struct ByteCount(usize);

impl std::io::Write for ByteCount {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0 += buf.len();
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// The event authority PDA and its bump, as `#[event_cpi]` derives them.
pub struct EventCpi<'info> {
    pub authority: AccountInfo<'info>,
    pub bump: u8,
}

impl EventCpi<'_> {
    /// Emit one event as a self-invocation signed by the event authority.
    pub fn emit<E: anchor_lang::Event>(&self, event: &E) -> Result<()> {
        let ix = Instruction {
            program_id: crate::ID,
            accounts: vec![AccountMeta::new_readonly(*self.authority.key, true)],
            data: event_instruction_data(event)?,
        };
        invoke_signed(
            &ix,
            std::slice::from_ref(&self.authority),
            &[&[b"__event_authority", &[self.bump]]],
        )
        .map_err(Error::from)
    }
}
