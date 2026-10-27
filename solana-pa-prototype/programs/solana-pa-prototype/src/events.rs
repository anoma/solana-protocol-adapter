//! Settlement events as Anchor CPI events.
//!
//! Program logs are truncated by the runtime at 10 KB per transaction, so an
//! event carried in a log line can be lost while the settlement succeeds.
//! A CPI event is the instruction data of a self-invocation; it is part of the
//! transaction and cannot be truncated. The adapter emits from free functions
//! that have no `ctx` in scope, so this module performs the same invocation
//! Anchor's `emit_cpi!` macro expands to, with the authority captured once per
//! instruction.

use anchor_lang::prelude::*;
use anchor_lang::solana_program::instruction::{AccountMeta, Instruction};
use anchor_lang::solana_program::program::invoke_signed;

/// Instruction data of a CPI event: Anchor's event tag, then the event's own
/// discriminator and Borsh body. The body is measured first and then written
/// into a buffer of exactly that size: the BPF bump allocator never frees a
/// grown-out buffer, and the counting pass costs fewer compute units than the
/// reallocations of a growing one.
pub fn event_instruction_data<E: anchor_lang::Event>(event: &E) -> Result<Vec<u8>> {
    let mut data = Vec::with_capacity(
        anchor_lang::event::EVENT_IX_TAG_LE.len()
            + E::DISCRIMINATOR.len()
            + borsh::object_length(event)?,
    );
    data.extend_from_slice(anchor_lang::event::EVENT_IX_TAG_LE);
    data.extend_from_slice(E::DISCRIMINATOR);
    event.serialize(&mut data)?;
    Ok(data)
}

/// The event authority PDA, as `#[event_cpi]` validates it.
pub struct EventCpi<'info> {
    pub authority: AccountInfo<'info>,
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
            &[&[b"__event_authority", &[crate::EVENT_AUTHORITY_AND_BUMP.1]]],
        )
        .map_err(Error::from)
    }
}

/// An event's discriminator: the first 8 bytes of
/// `sha256("event:" ++ signature)`, where the signature is the event's name
/// followed by its field types in order, as [`idl_event_signature`] spells
/// them. A change to the name or to any field's type changes the
/// discriminator, so a consumer never decodes one layout as another.
pub const fn event_discriminator(signature: &str) -> [u8; 8] {
    let hash = sha2_const_stable::Sha256::new()
        .update(b"event:")
        .update(signature.as_bytes())
        .finalize();
    let mut discriminator = [0u8; 8];
    let mut i = 0;
    while i < discriminator.len() {
        discriminator[i] = hash[i];
        i += 1;
    }
    discriminator
}

/// The signature of `event` in `idl`: its name, then the types of its fields
/// in order, comma-separated in parentheses. A primitive is spelled by its
/// IDL name; `option<T>`, `vec<T>` and `[T;N]` by their element types; a
/// defined alias by its target; a defined struct by its name and its fields'
/// types, as an event. Any other type panics.
#[cfg(feature = "idl-build")]
pub fn idl_event_signature(idl: &anchor_lang::idl::types::Idl, event: &str) -> String {
    use anchor_lang::idl::types::{
        IdlArrayLen, IdlDefinedFields, IdlType, IdlTypeDef, IdlTypeDefTy,
    };

    fn type_def<'a>(idl: &'a anchor_lang::idl::types::Idl, name: &str) -> &'a IdlTypeDef {
        idl.types
            .iter()
            .find(|t| t.name == name)
            .unwrap_or_else(|| panic!("the IDL defines no type {name}"))
    }

    fn struct_sig(idl: &anchor_lang::idl::types::Idl, event: &str, def: &IdlTypeDef) -> String {
        let types: Vec<String> = match &def.ty {
            IdlTypeDefTy::Struct { fields: None } => Vec::new(),
            IdlTypeDefTy::Struct {
                fields: Some(IdlDefinedFields::Named(fields)),
            } => fields
                .iter()
                .map(|f| ty(idl, event, &f.name, &f.ty))
                .collect(),
            IdlTypeDefTy::Struct {
                fields: Some(IdlDefinedFields::Tuple(types)),
            } => types
                .iter()
                .enumerate()
                .map(|(i, t)| ty(idl, event, &i.to_string(), t))
                .collect(),
            other => panic!(
                "event {event}: type {} is {other:?}, which the signature spelling does not cover",
                def.name
            ),
        };
        format!("{}({})", def.name, types.join(","))
    }

    fn ty(idl: &anchor_lang::idl::types::Idl, event: &str, field: &str, t: &IdlType) -> String {
        let uncovered = || -> ! {
            panic!(
                "event {event}: field {field} has type {t:?}, which the signature spelling does not cover"
            )
        };
        match t {
            IdlType::Bool => "bool".into(),
            IdlType::U8 => "u8".into(),
            IdlType::I8 => "i8".into(),
            IdlType::U16 => "u16".into(),
            IdlType::I16 => "i16".into(),
            IdlType::U32 => "u32".into(),
            IdlType::I32 => "i32".into(),
            IdlType::F32 => "f32".into(),
            IdlType::U64 => "u64".into(),
            IdlType::I64 => "i64".into(),
            IdlType::F64 => "f64".into(),
            IdlType::U128 => "u128".into(),
            IdlType::I128 => "i128".into(),
            IdlType::U256 => "u256".into(),
            IdlType::I256 => "i256".into(),
            IdlType::Bytes => "bytes".into(),
            IdlType::String => "string".into(),
            IdlType::Pubkey => "pubkey".into(),
            IdlType::Option(inner) => format!("option<{}>", ty(idl, event, field, inner)),
            IdlType::Vec(inner) => format!("vec<{}>", ty(idl, event, field, inner)),
            IdlType::Array(inner, IdlArrayLen::Value(len)) => {
                format!("[{};{len}]", ty(idl, event, field, inner))
            }
            IdlType::Defined { name, generics } if generics.is_empty() => {
                let def = type_def(idl, name);
                match &def.ty {
                    IdlTypeDefTy::Type { alias } => ty(idl, event, field, alias),
                    IdlTypeDefTy::Struct { .. } if def.generics.is_empty() => {
                        struct_sig(idl, event, def)
                    }
                    _ => uncovered(),
                }
            }
            _ => uncovered(),
        }
    }

    struct_sig(idl, event, type_def(idl, event))
}
