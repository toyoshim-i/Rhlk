use std::collections::{BTreeMap, HashMap};

use anyhow::{bail, Result};

use crate::format::obj::{Command, ObjectFile};
use crate::layout::LayoutPlan;
use crate::resolver::SectionKind;

use super::GlobalSymbolAddr;
use super::opcode;

pub(super) fn patch_ctor_dtor_tables(
    linked: &mut BTreeMap<SectionKind, Vec<u8>>,
    objects: &[ObjectFile],
    layout: &LayoutPlan,
    global_symbol_addrs: &HashMap<Vec<u8>, GlobalSymbolAddr>,
    text_size: u32,
) -> Result<()> {
    const CTOR_LIST: &[u8] = b"___CTOR_LIST__";
    const DTOR_LIST: &[u8] = b"___DTOR_LIST__";

    let mut ctor_entries = Vec::<u32>::new();
    let mut dtor_entries = Vec::<u32>::new();
    for (idx, obj) in objects.iter().enumerate() {
        let text_base = layout.placements[idx]
            .by_section
            .get(&SectionKind::Text)
            .copied()
            .unwrap_or(0);
        for cmd in &obj.commands {
            let Command::Opaque { code, payload } = cmd else {
                continue;
            };
            let Some(v) = super::read_u32_be(payload) else {
                continue;
            };
            match *code {
                opcode::OP_CTOR_ENTRY => ctor_entries.push(text_base.saturating_add(v)),
                opcode::OP_DTOR_ENTRY => dtor_entries.push(text_base.saturating_add(v)),
                _ => {}
            }
        }
    }

    if !ctor_entries.is_empty() {
        let Some(base) = global_symbol_addrs.get(CTOR_LIST) else {
            bail!("ctor table symbol is missing: ___CTOR_LIST__");
        };
        if !matches!(base.section, SectionKind::Text | SectionKind::Data) {
            bail!("ctor table symbol must be in text/data: ___CTOR_LIST__");
        }
        let table = build_ctor_dtor_table(&ctor_entries);
        write_table_at_absolute(linked, text_size, base.addr, &table)?;
    }
    if !dtor_entries.is_empty() {
        let Some(base) = global_symbol_addrs.get(DTOR_LIST) else {
            bail!("dtor table symbol is missing: ___DTOR_LIST__");
        };
        if !matches!(base.section, SectionKind::Text | SectionKind::Data) {
            bail!("dtor table symbol must be in text/data: ___DTOR_LIST__");
        }
        let table = build_ctor_dtor_table(&dtor_entries);
        write_table_at_absolute(linked, text_size, base.addr, &table)?;
    }
    Ok(())
}

fn build_ctor_dtor_table(entries: &[u32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(8 + entries.len() * 4);
    out.extend_from_slice(&0xffff_ffffu32.to_be_bytes());
    for v in entries {
        out.extend_from_slice(&v.to_be_bytes());
    }
    out.extend_from_slice(&0u32.to_be_bytes());
    out
}

fn write_table_at_absolute(
    linked: &mut BTreeMap<SectionKind, Vec<u8>>,
    text_size: u32,
    addr: u32,
    table: &[u8],
) -> Result<()> {
    if addr < text_size {
        let target = linked
            .get_mut(&SectionKind::Text)
            .ok_or_else(|| anyhow::anyhow!("text section is missing while writing ctor/dtor table"))?;
        let begin = addr as usize;
        let end = begin.saturating_add(table.len());
        if end > target.len() {
            bail!("ctor/dtor table overflows text section");
        }
        target[begin..end].copy_from_slice(table);
        return Ok(());
    }

    let target = linked
        .get_mut(&SectionKind::Data)
        .ok_or_else(|| anyhow::anyhow!("data section is missing while writing ctor/dtor table"))?;
    let begin = addr.saturating_sub(text_size) as usize;
    let end = begin.saturating_add(table.len());
    if end > target.len() {
        bail!("ctor/dtor table overflows data section");
    }
    target[begin..end].copy_from_slice(table);
    Ok(())
}
