use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;

use anyhow::{bail, Context, Result};
use thiserror::Error;

use crate::format::obj::{Command, ObjectFile};
use crate::layout::LayoutPlan;
use crate::resolver::{ObjectSummary, SectionKind, Symbol};

mod map;
pub use map::write_map;
#[cfg(test)]
pub(crate) use map::build_map_text;
mod ctor_dtor;
mod opcode;
mod expr;

#[derive(Debug, Error)]
enum WriterError {
    #[error("relocation target address is odd: {offset:#x}")]
    RelocationTargetAddressIsOdd { offset: u32 },
}

#[derive(Debug, Clone, Copy)]
pub enum OutputFormat {
    X,
    R,
    Mcs,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RelocationCheck {
    Strict,
    Skip,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BssPolicy {
    Include,
    Omit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SymbolTablePolicy {
    Keep,
    Cut,
}

#[derive(Debug, Clone, Copy)]
pub struct OutputOptions {
    pub format: OutputFormat,
    pub relocation_check: RelocationCheck,
    pub bss_policy: BssPolicy,
    pub symbol_table: SymbolTablePolicy,
    pub base_address: u32,
    pub load_mode: u8,
    pub section_info: bool,
    pub g2lk_mode: bool,
}

/// Writes a linked output image to `output_path`.
///
/// # Errors
/// Returns an error when validation, image generation, header patching, or file write fails.
pub fn write_output(
    output_path: &str,
    options: OutputOptions,
    objects: &[ObjectFile],
    input_paths: &[String],
    summaries: &[ObjectSummary],
    layout: &LayoutPlan,
) -> Result<()> {
    validate_link_inputs(objects, input_paths, summaries, options.g2lk_mode)?;

    if matches!(options.format, OutputFormat::R | OutputFormat::Mcs)
        && matches!(options.relocation_check, RelocationCheck::Strict)
    {
        validate_r_convertibility(objects, summaries, layout, output_path)?;
    }

    let mut payload = if matches!(options.format, OutputFormat::R | OutputFormat::Mcs) {
        build_r_payload(
            objects,
            summaries,
            layout,
            matches!(options.bss_policy, BssPolicy::Omit),
        )?
    } else {
        build_x_image_with_options(
            objects,
            summaries,
            layout,
            matches!(options.symbol_table, SymbolTablePolicy::Keep),
        )
        .map_err(|err| {
            if err
                .downcast_ref::<WriterError>()
                .is_some_and(|e| matches!(e, WriterError::RelocationTargetAddressIsOdd { .. }))
            {
                anyhow::anyhow!(
                    "再配置対象が奇数アドレスにあります: {}",
                    to_human68k_path(Path::new(output_path))
                )
            } else {
                err
            }
        })?
    };

    if matches!(options.format, OutputFormat::X) && (options.base_address != 0 || options.load_mode != 0) {
        apply_x_header_options(&mut payload, options.base_address, options.load_mode)?;
    }
    if options.section_info {
        patch_section_size_info(
            &mut payload,
            matches!(options.format, OutputFormat::R | OutputFormat::Mcs),
            summaries,
            layout,
        )?;
    }

    if matches!(options.format, OutputFormat::Mcs) {
        let bss_extra = if matches!(options.bss_policy, BssPolicy::Omit) {
            0
        } else {
            layout
                .total_size_by_section
                .get(&SectionKind::Bss)
                .copied()
                .unwrap_or(0)
                .saturating_add(
                    layout
                        .total_size_by_section
                        .get(&SectionKind::Common)
                        .copied()
                        .unwrap_or(0),
                )
                .saturating_add(
                    layout
                        .total_size_by_section
                        .get(&SectionKind::Stack)
                        .copied()
                        .unwrap_or(0),
                )
        };
        patch_mcs_size(&mut payload, bss_extra).map_err(|_| {
            anyhow::anyhow!(
                "MACS形式ファイルではありません: {}",
                to_human68k_path(Path::new(output_path))
            )
        })?;
    }
    std::fs::write(output_path, payload).with_context(|| format!("failed to write {output_path}"))?;
    Ok(())
}

fn patch_section_size_info(
    payload: &mut [u8],
    r_format: bool,
    summaries: &[ObjectSummary],
    layout: &LayoutPlan,
) -> Result<()> {
    let text_size = section_total(layout, SectionKind::Text);
    let data_size = section_total(layout, SectionKind::Data);
    let bss_only = section_total(layout, SectionKind::Bss);
    let common_only = section_total(layout, SectionKind::Common);
    let addrs = build_global_symbol_addrs(summaries, layout, text_size, data_size, bss_only, common_only);
    let Some(sym) = addrs.get(b"___size_info".as_slice()) else {
        bail!("section info symbol is missing: ___size_info");
    };
    if sym.section != SectionKind::Data {
        bail!("section info symbol must be in data: ___size_info");
    }

    let roff_tbl_size = if r_format {
        0
    } else if payload.len() >= 28 && payload[0] == b'H' && payload[1] == b'U' {
        u32::from_be_bytes([payload[24], payload[25], payload[26], payload[27]])
    } else {
        0
    };
    let values = [
        text_size,
        data_size,
        section_total(layout, SectionKind::Bss),
        section_total(layout, SectionKind::Common),
        section_total(layout, SectionKind::Stack),
        section_total(layout, SectionKind::RData),
        section_total(layout, SectionKind::RBss),
        section_total(layout, SectionKind::RCommon),
        section_total(layout, SectionKind::RStack),
        section_total(layout, SectionKind::RLData),
        section_total(layout, SectionKind::RLBss),
        section_total(layout, SectionKind::RLCommon),
        section_total(layout, SectionKind::RLStack),
        roff_tbl_size,
    ];
    let write_pos = if r_format {
        sym.addr as usize
    } else {
        64usize.saturating_add(sym.addr as usize)
    };
    let need = write_pos.saturating_add(values.len() * 4);
    if need > payload.len() {
        bail!("section info region overflows output payload");
    }
    let mut p = write_pos;
    for v in values {
        payload[p..p + 4].copy_from_slice(&v.to_be_bytes());
        p += 4;
    }
    Ok(())
}

fn apply_x_header_options(payload: &mut [u8], base_address: u32, load_mode: u8) -> Result<()> {
    if payload.len() < 64 || payload[0] != b'H' || payload[1] != b'U' {
        bail!("invalid x-format payload while applying base address");
    }
    payload[3] = load_mode;
    if base_address == 0 {
        return Ok(());
    }
    let exec_off = u32::from_be_bytes([payload[8], payload[9], payload[10], payload[11]]);
    let exec_abs = base_address.wrapping_add(exec_off);
    payload[4..8].copy_from_slice(&base_address.to_be_bytes());
    payload[8..12].copy_from_slice(&exec_abs.to_be_bytes());
    Ok(())
}

fn section_total(layout: &LayoutPlan, section: SectionKind) -> u32 {
    layout.total_size_by_section.get(&section).copied().unwrap_or(0)
}

fn section_tag(section: SectionKind) -> &'static str {
    match section {
        SectionKind::Abs => "abs",
        SectionKind::Text => "text",
        SectionKind::Data => "data",
        SectionKind::Bss => "bss",
        SectionKind::Stack => "stack",
        SectionKind::RData => "rdata",
        SectionKind::RBss => "rbss",
        SectionKind::RStack => "rstack",
        SectionKind::RLData => "rldata",
        SectionKind::RLBss => "rlbss",
        SectionKind::RLStack => "rlstack",
        SectionKind::Common => "common",
        SectionKind::RCommon => "rcommon",
        SectionKind::RLCommon => "rlcommon",
        SectionKind::Xref => "xref",
        SectionKind::Unknown(_) => "unknown",
    }
}

#[cfg_attr(not(test), allow(dead_code))]
fn build_x_image(
    objects: &[ObjectFile],
    summaries: &[ObjectSummary],
    layout: &LayoutPlan,
) -> Result<Vec<u8>> {
    build_x_image_with_options(objects, summaries, layout, true)
}

fn build_x_image_with_options(
    objects: &[ObjectFile],
    summaries: &[ObjectSummary],
    layout: &LayoutPlan,
    include_symbols: bool,
) -> Result<Vec<u8>> {
    if objects.len() != summaries.len() || objects.len() != layout.placements.len() {
        bail!("internal mismatch: objects/summaries/layout length differs");
    }

    let mut linked = link_initialized_sections(
        objects,
        summaries,
        layout,
        &[SectionKind::Text, SectionKind::Data],
    )?;

    let text_size = linked
        .get(&SectionKind::Text)
        .map_or(0, |v| usize_to_u32_saturating(v.len()));
    let data_size = linked
        .get(&SectionKind::Data)
        .map_or(0, |v| usize_to_u32_saturating(v.len()));
    let bss_only = layout
        .total_size_by_section
        .get(&SectionKind::Bss)
        .copied()
        .unwrap_or(0);
    let common_only = layout
        .total_size_by_section
        .get(&SectionKind::Common)
        .copied()
        .unwrap_or(0);
    let stack_only = layout
        .total_size_by_section
        .get(&SectionKind::Stack)
        .copied()
        .unwrap_or(0);
    let bss_size = bss_only
        .saturating_add(common_only)
        .saturating_add(stack_only);

    let global_symbol_addrs =
        build_global_symbol_addrs(summaries, layout, text_size, data_size, bss_only, common_only);
    patch_opaque_commands(
        &mut linked,
        objects,
        summaries,
        layout,
        &global_symbol_addrs,
    );
    ctor_dtor::patch_ctor_dtor_tables(&mut linked, objects, layout, &global_symbol_addrs, text_size)?;

    let text = linked.get(&SectionKind::Text).cloned().unwrap_or_default();
    let data = linked.get(&SectionKind::Data).cloned().unwrap_or_default();

    let symbol_data = if include_symbols {
        build_symbol_table(
            summaries,
            layout,
            text_size,
            data_size,
            bss_only,
            common_only,
        )
    } else {
        Vec::new()
    };
    let symbol_size = usize_to_u32_saturating(symbol_data.len());
    let reloc_table = build_relocation_table(objects, summaries, layout, text_size, &global_symbol_addrs)?;
    let reloc_size = usize_to_u32_saturating(reloc_table.len());
    let (scd_line, scd_info, scd_name) = build_scd_passthrough(objects, summaries, layout)?;
    let scd_line_size = usize_to_u32_saturating(scd_line.len());
    let scd_info_size = usize_to_u32_saturating(scd_info.len());
    let scd_name_size = usize_to_u32_saturating(scd_name.len());

    let exec = resolve_exec_address(summaries, text_size, data_size, bss_size)?.unwrap_or(0);
    let header = build_x_header(XHeader {
        text_size,
        data_size,
        bss_size,
        reloc_size,
        symbol_size,
        scd_line_size,
        scd_info_size,
        scd_name_size,
        exec,
    });

    let mut image = header;
    image.extend_from_slice(&text);
    image.extend_from_slice(&data);
    image.extend_from_slice(&reloc_table);
    image.extend_from_slice(&symbol_data);
    image.extend_from_slice(&scd_line);
    image.extend_from_slice(&scd_info);
    image.extend_from_slice(&scd_name);
    Ok(image)
}

#[allow(clippy::similar_names)]
fn build_scd_passthrough(
    objects: &[ObjectFile],
    summaries: &[ObjectSummary],
    layout: &LayoutPlan,
) -> Result<(Vec<u8>, Vec<u8>, Vec<u8>)> {
    let xdefs = build_scd_xdef_map(summaries);
    let mut line = Vec::new();
    let mut info = Vec::new();
    let mut name = Vec::new();
    let mut sinfo_pos_entries = 0u32;

    for (idx, obj) in objects.iter().enumerate() {
        let tail = &obj.scd_tail;
        if tail.len() < 12 {
            continue;
        }
        let linfo_size = u32::from_be_bytes([tail[0], tail[1], tail[2], tail[3]]) as usize;
        let sinfo_plus_einfo_size = u32::from_be_bytes([tail[4], tail[5], tail[6], tail[7]]) as usize;
        let ninfo_size = u32::from_be_bytes([tail[8], tail[9], tail[10], tail[11]]) as usize;
        let total = 12usize
            .saturating_add(linfo_size)
            .saturating_add(sinfo_plus_einfo_size)
            .saturating_add(ninfo_size);
        if total > tail.len() {
            continue;
        }

        let mut p = 12usize;
        // HLK make_scdinfo compatible line fixup:
        // - location!=0 : + text_pos
        // - location==0 : + cumulative sinfo_pos(entries)
        let text_pos = layout.placements[idx]
            .by_section
            .get(&SectionKind::Text)
            .copied()
            .unwrap_or(0);
        let linfo = &tail[p..p + linfo_size];
        line.extend_from_slice(&rebase_scd_line_table(linfo, text_pos, sinfo_pos_entries));
        p += linfo_size;
        let sinfo_count = extract_sinfo_count(tail, linfo_size);
        let sinfo_plus_einfo = &tail[p..p + sinfo_plus_einfo_size];
        let ninfo = &tail[p + sinfo_plus_einfo_size..p + sinfo_plus_einfo_size + ninfo_size];
        info.extend_from_slice(&rebase_scd_info_table(
            sinfo_plus_einfo,
            ninfo,
            sinfo_count,
            sinfo_pos_entries,
            &layout.placements[idx].by_section,
            layout,
            &xdefs,
        )?);
        p += sinfo_plus_einfo_size;
        name.extend_from_slice(&tail[p..p + ninfo_size]);

        sinfo_pos_entries = sinfo_pos_entries.saturating_add(sinfo_count);
    }

    Ok((line, info, name))
}

fn rebase_scd_line_table(input: &[u8], text_pos: u32, sinfo_pos_entries: u32) -> Vec<u8> {
    if !input.len().is_multiple_of(6) {
        return input.to_vec();
    }
    let mut out = Vec::with_capacity(input.len());
    let mut i = 0usize;
    while i + 6 <= input.len() {
        let mut loc = u32::from_be_bytes([input[i], input[i + 1], input[i + 2], input[i + 3]]);
        if loc != 0 {
            loc = loc.saturating_add(text_pos);
        } else {
            loc = loc.saturating_add(sinfo_pos_entries);
        }
        out.extend_from_slice(&loc.to_be_bytes());
        out.push(input[i + 4]);
        out.push(input[i + 5]);
        i += 6;
    }
    out
}

fn extract_sinfo_count(tail: &[u8], linfo_size: usize) -> u32 {
    let pos = 20usize.saturating_add(linfo_size);
    if pos + 4 > tail.len() {
        return 0;
    }
    u32::from_be_bytes([tail[pos], tail[pos + 1], tail[pos + 2], tail[pos + 3]])
}

fn rebase_scd_info_table(
    input: &[u8],
    ninfo: &[u8],
    sinfo_count: u32,
    sinfo_pos_entries: u32,
    placement: &BTreeMap<SectionKind, u32>,
    layout: &LayoutPlan,
    xdefs: &HashMap<Vec<u8>, ScdXdef>,
) -> Result<Vec<u8>> {
    let mut out = input.to_vec();
    let sinfo_bytes = (sinfo_count as usize).saturating_mul(18).min(out.len());
    let mut p = 0usize;
    while p + 18 <= sinfo_bytes {
        let sect = u16::from_be_bytes([out[p + 12], out[p + 13]]);
        let delta = sinfo_section_delta(sect, placement, layout)?;
        if let Some(delta) = delta {
            if delta != 0 {
                let mut val = u32::from_be_bytes([out[p + 8], out[p + 9], out[p + 10], out[p + 11]]);
                val = val.wrapping_add(i64_low_u32(delta));
                out[p + 8..p + 12].copy_from_slice(&val.to_be_bytes());
            }
        }
        p += 18;
    }

    // Minimal einfo fixup:
    // if entry starts with d6==0, the next long is sinfo index and must be rebased.
    let mut q = sinfo_bytes;
    while q + 18 <= out.len() {
        let d6 = u32::from_be_bytes([out[q], out[q + 1], out[q + 2], out[q + 3]]);
        let sect = u16::from_be_bytes([out[q + 8], out[q + 9]]);
        if d6 == 0 {
            let mut ref_idx =
                u32::from_be_bytes([out[q + 4], out[q + 5], out[q + 6], out[q + 7]]);
            if ref_idx != 0 {
                ref_idx = ref_idx.saturating_add(sinfo_pos_entries);
                out[q + 4..q + 8].copy_from_slice(&ref_idx.to_be_bytes());
            }
        } else {
            if matches!(sect, 0x0004 | 0x0007 | 0x000a) {
                bail!("unsupported SCD einfo section for d6!=0: {sect:#06x}");
            }
            if matches!(sect, 0x00fc..=0x00fe | 0xfffc..=0xfffe) {
                let name = decode_scd_entry_name(&out[q..q + 18], ninfo)
                    .with_context(|| format!("invalid SCD einfo name at offset {q}"))?;
                let Some(xdef) = xdefs.get(&name) else {
                    bail!(
                        "unresolved SCD einfo common-reference for d6!=0: {}",
                        String::from_utf8_lossy(&name)
                    );
                };
                let (resolved_off, resolved_sect): (u32, u16) = match xdef.section {
                    SectionKind::Common => (xdef.value, 0x0003),
                    SectionKind::RCommon => (xdef.value, 0x0006),
                    SectionKind::RLCommon => (xdef.value, 0x0009),
                    _ => bail!(
                        "unsupported SCD einfo common-reference target section: {:?}",
                        xdef.section
                    ),
                };
                out[q + 4..q + 8].copy_from_slice(&resolved_off.to_be_bytes());
                out[q + 8..q + 10].copy_from_slice(&resolved_sect.to_be_bytes());
                q += 18;
                continue;
            }
            if let Some(delta) = einfo_section_delta(sect, placement, layout) {
                if delta != 0 {
                    let off =
                        u32::from_be_bytes([out[q + 4], out[q + 5], out[q + 6], out[q + 7]]);
                    let adj = off.wrapping_add(i64_low_u32(delta));
                    out[q + 4..q + 8].copy_from_slice(&adj.to_be_bytes());
                }
            }
        }
        q += 18;
    }
    Ok(out)
}

fn sinfo_section_delta(
    sect: u16,
    placement: &BTreeMap<SectionKind, u32>,
    layout: &LayoutPlan,
) -> Result<Option<i64>> {
    match sect {
        0x0001 | 0x0002 | 0x0003 | 0x0005 | 0x0006 | 0x0008 | 0x0009 => {
            Ok(einfo_section_delta(sect, placement, layout))
        }
        // xref/common/rcommon/rlcommon are carried as-is in make_scdinfo path.
        0x0000 | 0x00fc..=0x00fe | 0xfffc..=0xffff => Ok(None),
        _ => bail!("unsupported SCD sinfo section: {sect:#06x}"),
    }
}

#[derive(Debug, Clone, Copy)]
struct ScdXdef {
    section: SectionKind,
    value: u32,
}

fn decode_scd_entry_name(entry: &[u8], ninfo: &[u8]) -> Result<Vec<u8>> {
    if entry.len() < 8 {
        bail!("entry too short");
    }
    let head = u32::from_be_bytes([entry[0], entry[1], entry[2], entry[3]]);
    if head != 0 {
        let mut name = entry[0..8].to_vec();
        while name.last() == Some(&0) {
            name.pop();
        }
        return Ok(name);
    }
    let off = u32::from_be_bytes([entry[4], entry[5], entry[6], entry[7]]) as usize;
    if off >= ninfo.len() {
        bail!("ninfo offset out of range: {off}");
    }
    let rel_end = ninfo[off..]
        .iter()
        .position(|&b| b == 0)
        .ok_or_else(|| anyhow::anyhow!("unterminated ninfo string at offset {off}"))?;
    Ok(ninfo[off..off + rel_end].to_vec())
}

#[allow(clippy::similar_names)]
fn build_scd_xdef_map(summaries: &[ObjectSummary]) -> HashMap<Vec<u8>, ScdXdef> {
    let mut xdefs = HashMap::<Vec<u8>, ScdXdef>::new();
    let mut non_common = HashSet::<Vec<u8>>::new();

    for summary in summaries {
        for sym in &summary.symbols {
            if sym.name.first() == Some(&b'*') {
                continue;
            }
            if !matches!(
                sym.section,
                SectionKind::Common | SectionKind::RCommon | SectionKind::RLCommon
            ) {
                non_common.insert(sym.name.clone());
                xdefs.entry(sym.name.clone()).or_insert(ScdXdef {
                    section: sym.section,
                    value: sym.value,
                });
            }
        }
    }

    let mut common_size = HashMap::<Vec<u8>, (SectionKind, u32, usize, bool)>::new();
    let mut order = 0usize;
    for summary in summaries {
        for sym in &summary.symbols {
            if !matches!(
                sym.section,
                SectionKind::Common | SectionKind::RCommon | SectionKind::RLCommon
            ) {
                continue;
            }
            let size = align_even(sym.value);
            let e = common_size
                .entry(sym.name.clone())
                .or_insert((sym.section, size, order, false));
            order = order.saturating_add(1);
            if e.0 != sym.section {
                e.3 = true;
                continue;
            }
            if size > e.1 {
                e.1 = size;
            }
        }
    }

    let mut ordered = common_size.into_iter().collect::<Vec<_>>();
    ordered.sort_by_key(|(_, (_, _, ord, _))| *ord);
    let mut common_cur = 0u32;
    let mut rcommon_cur = 0u32;
    let mut rlcommon_cur = 0u32;
    for (name, (section, size, _, conflicted)) in ordered {
        if conflicted || non_common.contains(&name) || xdefs.contains_key(&name) {
            continue;
        }
        let off = match section {
            SectionKind::Common => {
                let v = common_cur;
                common_cur = common_cur.saturating_add(size);
                v
            }
            SectionKind::RCommon => {
                let v = rcommon_cur;
                rcommon_cur = rcommon_cur.saturating_add(size);
                v
            }
            SectionKind::RLCommon => {
                let v = rlcommon_cur;
                rlcommon_cur = rlcommon_cur.saturating_add(size);
                v
            }
            _ => continue,
        };
        xdefs.insert(
            name,
            ScdXdef {
                section,
                value: off,
            },
        );
    }

    xdefs
}

#[allow(clippy::similar_names)]
fn einfo_section_delta(
    sect: u16,
    placement: &BTreeMap<SectionKind, u32>,
    layout: &LayoutPlan,
) -> Option<i64> {
    let text_total = i64::from(
        layout
            .total_size_by_section
            .get(&SectionKind::Text)
            .copied()
            .unwrap_or(0),
    );
    let data_total = i64::from(
        layout
            .total_size_by_section
            .get(&SectionKind::Data)
            .copied()
            .unwrap_or(0),
    );
    let bss_total = i64::from(
        layout
            .total_size_by_section
            .get(&SectionKind::Bss)
            .copied()
            .unwrap_or(0),
    );
    let common_total = i64::from(
        layout
            .total_size_by_section
            .get(&SectionKind::Common)
            .copied()
            .unwrap_or(0),
    );
    let stack_total = i64::from(
        layout
            .total_size_by_section
            .get(&SectionKind::Stack)
            .copied()
            .unwrap_or(0),
    );
    let obj_size = text_total + data_total + bss_total + common_total + stack_total;

    let text_pos = i64::from(placement.get(&SectionKind::Text).copied().unwrap_or(0));
    let data_pos = text_total + i64::from(placement.get(&SectionKind::Data).copied().unwrap_or(0));
    let bss_pos = text_total
        + data_total
        + i64::from(placement.get(&SectionKind::Bss).copied().unwrap_or(0));
    let rdata_pos = i64::from(placement.get(&SectionKind::RData).copied().unwrap_or(0));
    let rbss_pos = i64::from(placement.get(&SectionKind::RBss).copied().unwrap_or(0));
    let rldata_pos = i64::from(placement.get(&SectionKind::RLData).copied().unwrap_or(0));
    let rlbss_pos = i64::from(placement.get(&SectionKind::RLBss).copied().unwrap_or(0));

    match sect {
        0x0001 => Some(text_pos),
        0x0002 => Some(data_pos - text_total),
        0x0003 => Some(bss_pos - obj_size),
        0x0005 => Some(rdata_pos),
        0x0006 => Some(rbss_pos),
        0x0008 => Some(rldata_pos),
        0x0009 => Some(rlbss_pos),
        _ => None,
    }
}

fn i64_low_u32(value: i64) -> u32 {
    let bytes = value.to_be_bytes();
    u32::from_be_bytes([bytes[4], bytes[5], bytes[6], bytes[7]])
}

fn build_symbol_table(
    summaries: &[ObjectSummary],
    layout: &LayoutPlan,
    text_size: u32,
    data_size: u32,
    bss_only: u32,
    common_only: u32,
) -> Vec<u8> {
    let mut out = Vec::new();
    for (idx, summary) in summaries.iter().enumerate() {
        for sym in &summary.symbols {
            if sym.name.first() == Some(&b'*') {
                continue;
            }
            let Some((ty, addr)) = encode_symbol(
                sym,
                &layout.placements[idx].by_section,
                text_size,
                data_size,
                bss_only,
                common_only,
            ) else {
                continue;
            };

            out.extend_from_slice(&ty.to_be_bytes());
            out.extend_from_slice(&addr.to_be_bytes());
            out.extend_from_slice(&sym.name);
            out.push(0);
            if out.len() % 2 != 0 {
                out.push(0);
            }
        }
    }
    out
}

fn encode_symbol(
    sym: &Symbol,
    placement: &BTreeMap<SectionKind, u32>,
    text_size: u32,
    data_size: u32,
    bss_only: u32,
    common_only: u32,
) -> Option<(u16, u32)> {
    match sym.section {
        SectionKind::Text => Some((
            0x0201,
            placement
                .get(&SectionKind::Text)
                .copied()
                .unwrap_or(0)
                .saturating_add(sym.value),
        )),
        SectionKind::Data => Some((
            0x0202,
            text_size
                .saturating_add(placement.get(&SectionKind::Data).copied().unwrap_or(0))
                .saturating_add(sym.value),
        )),
        SectionKind::Bss => Some((
            0x0203,
            text_size
                .saturating_add(data_size)
                .saturating_add(placement.get(&SectionKind::Bss).copied().unwrap_or(0))
                .saturating_add(sym.value),
        )),
        SectionKind::Stack => Some((
            0x0204,
            text_size
                .saturating_add(data_size)
                .saturating_add(bss_only)
                .saturating_add(common_only)
                .saturating_add(placement.get(&SectionKind::Stack).copied().unwrap_or(0))
                .saturating_add(sym.value),
        )),
        SectionKind::Common => Some((0x0003, sym.value)),
        SectionKind::Abs
        | SectionKind::RData
        | SectionKind::RBss
        | SectionKind::RStack
        | SectionKind::RLData
        | SectionKind::RLBss
        | SectionKind::RLStack => Some((0x0200, sym.value)),
        SectionKind::RCommon | SectionKind::RLCommon | SectionKind::Xref | SectionKind::Unknown(_) => None,
    }
}

fn build_r_payload(
    objects: &[ObjectFile],
    summaries: &[ObjectSummary],
    layout: &LayoutPlan,
    omit_bss: bool,
) -> Result<Vec<u8>> {
    let linked = link_initialized_sections(
        objects,
        summaries,
        layout,
        &[SectionKind::Text, SectionKind::Data, SectionKind::RData, SectionKind::RLData],
    )?;

    let mut payload = Vec::new();
    for section in [
        SectionKind::Text,
        SectionKind::Data,
        SectionKind::RData,
        SectionKind::RLData,
    ] {
        if let Some(bytes) = linked.get(&section) {
            payload.extend_from_slice(bytes);
        }
    }

    if !omit_bss {
        let bss = layout
            .total_size_by_section
            .get(&SectionKind::Bss)
            .copied()
            .unwrap_or(0);
        let common = layout
            .total_size_by_section
            .get(&SectionKind::Common)
            .copied()
            .unwrap_or(0);
        let stack = layout
            .total_size_by_section
            .get(&SectionKind::Stack)
            .copied()
            .unwrap_or(0);
        let total = bss.saturating_add(common).saturating_add(stack) as usize;
        payload.resize(payload.len() + total, 0);
    }

    Ok(payload)
}

fn link_initialized_sections(
    objects: &[ObjectFile],
    summaries: &[ObjectSummary],
    layout: &LayoutPlan,
    section_order: &[SectionKind],
) -> Result<BTreeMap<SectionKind, Vec<u8>>> {
    if objects.len() != summaries.len() || objects.len() != layout.placements.len() {
        bail!("internal mismatch: objects/summaries/layout length differs");
    }

    let mut linked = BTreeMap::<SectionKind, Vec<u8>>::new();
    for &section in section_order {
        let total = layout
            .total_size_by_section
            .get(&section)
            .copied()
            .unwrap_or(0) as usize;
        linked.insert(section, vec![0; total]);
    }

    for idx in 0..objects.len() {
        let obj_bytes = build_object_initialized_sections(&objects[idx], &summaries[idx]);
        for (section, bytes) in obj_bytes {
            let Some(start) = layout.placements[idx].by_section.get(&section).copied() else {
                continue;
            };
            if bytes.is_empty() {
                continue;
            }

            let target = linked
                .get_mut(&section)
                .with_context(|| format!("missing target section buffer: {section:?}"))?;
            let begin = start as usize;
            let end = begin + bytes.len();
            if end > target.len() {
                bail!("section overflow while placing object {idx} in {section:?}");
            }
            target[begin..end].copy_from_slice(&bytes);
        }
    }

    Ok(linked)
}

fn build_object_initialized_sections(
    object: &ObjectFile,
    summary: &ObjectSummary,
) -> BTreeMap<SectionKind, Vec<u8>> {
    let mut current = SectionKind::Text;
    let mut by_section = BTreeMap::<SectionKind, Vec<u8>>::new();

    for cmd in &object.commands {
        match cmd {
            Command::ChangeSection { section } => {
                current = SectionKind::from_u8(*section);
            }
            Command::RawData(data) => {
                if is_initialized_section(current) {
                    by_section.entry(current).or_default().extend_from_slice(data);
                }
            }
            Command::DefineSpace { size } => {
                if is_initialized_section(current) {
                    let entry = by_section.entry(current).or_default();
                    let new_len = entry.len() + *size as usize;
                    entry.resize(new_len, 0);
                }
            }
            Command::Opaque { code, .. } => {
                if is_initialized_section(current) {
                    let write_size = opaque_write_size(*code) as usize;
                    if write_size != 0 {
                        let entry = by_section.entry(current).or_default();
                        let new_len = entry.len() + write_size;
                        entry.resize(new_len, 0);
                    }
                }
            }
            _ => {}
        }
    }

    for section in [SectionKind::Text, SectionKind::Data, SectionKind::RData, SectionKind::RLData] {
        let expected = section_size(summary, section) as usize;
        let entry = by_section.entry(section).or_default();
        if entry.len() < expected {
            entry.resize(expected, 0);
        } else if entry.len() > expected {
            entry.truncate(expected);
        }
    }

    by_section
}

fn is_initialized_section(section: SectionKind) -> bool {
    matches!(
        section,
        SectionKind::Text | SectionKind::Data | SectionKind::RData | SectionKind::RLData
    )
}

fn section_size(summary: &ObjectSummary, section: SectionKind) -> u32 {
    let declared = summary
        .declared_section_sizes
        .get(&section)
        .copied()
        .unwrap_or(0);
    let observed = summary
        .observed_section_usage
        .get(&section)
        .copied()
        .unwrap_or(0);
    align_even(declared.max(observed))
}

fn align_even(v: u32) -> u32 {
    (v + 1) & !1
}

fn build_relocation_table(
    objects: &[ObjectFile],
    summaries: &[ObjectSummary],
    layout: &LayoutPlan,
    total_text_size: u32,
    global_symbol_addrs: &HashMap<Vec<u8>, GlobalSymbolAddr>,
) -> Result<Vec<u8>> {
    let mut offsets = Vec::<u32>::new();
    for (idx, (obj, summary)) in objects.iter().zip(summaries.iter()).enumerate() {
        collect_object_relocations(
            obj,
            summary,
            &layout.placements[idx].by_section,
            total_text_size,
            global_symbol_addrs,
            &mut offsets,
        );
    }
    if let Some(odd) = offsets.iter().copied().find(|off| off & 1 != 0) {
        bail!(WriterError::RelocationTargetAddressIsOdd { offset: odd });
    }
    offsets.sort_unstable();
    offsets.dedup();
    Ok(encode_relocation_offsets(&offsets))
}

fn validate_r_convertibility(
    objects: &[ObjectFile],
    summaries: &[ObjectSummary],
    layout: &LayoutPlan,
    output_path: &str,
) -> Result<()> {
    let text_size = layout
        .total_size_by_section
        .get(&SectionKind::Text)
        .copied()
        .unwrap_or(0);
    let data_size = layout
        .total_size_by_section
        .get(&SectionKind::Data)
        .copied()
        .unwrap_or(0);
    let bss_only = layout
        .total_size_by_section
        .get(&SectionKind::Bss)
        .copied()
        .unwrap_or(0);
    let common_only = layout
        .total_size_by_section
        .get(&SectionKind::Common)
        .copied()
        .unwrap_or(0);
    let global_symbol_addrs =
        build_global_symbol_addrs(summaries, layout, text_size, data_size, bss_only, common_only);
    let reloc = build_relocation_table(objects, summaries, layout, text_size, &global_symbol_addrs)?;
    if !reloc.is_empty() {
        bail!(
            "再配置テーブルが使われています: {}",
            to_human68k_path(Path::new(output_path))
        );
    }

    let data_size = layout
        .total_size_by_section
        .get(&SectionKind::Data)
        .copied()
        .unwrap_or(0);
    let bss_size = layout
        .total_size_by_section
        .get(&SectionKind::Bss)
        .copied()
        .unwrap_or(0)
        .saturating_add(
            layout
                .total_size_by_section
                .get(&SectionKind::Common)
                .copied()
                .unwrap_or(0),
        )
        .saturating_add(
            layout
                .total_size_by_section
                .get(&SectionKind::Stack)
                .copied()
                .unwrap_or(0),
        );
    let exec = resolve_exec_address(summaries, text_size, data_size, bss_size)?.unwrap_or(0);
    if exec != 0 {
        bail!(
            "実行開始アドレスがファイル先頭ではありません: {}",
            to_human68k_path(Path::new(output_path))
        );
    }
    Ok(())
}

fn to_human68k_path(path: &Path) -> String {
    format!("A:{}", path.to_string_lossy().replace('/', "\\"))
}

fn collect_object_relocations(
    object: &ObjectFile,
    summary: &ObjectSummary,
    placement: &BTreeMap<SectionKind, u32>,
    total_text_size: u32,
    global_symbol_addrs: &HashMap<Vec<u8>, GlobalSymbolAddr>,
    out: &mut Vec<u32>,
) {
    let mut current = SectionKind::Text;
    let mut cursor_by_section = BTreeMap::<SectionKind, u32>::new();

    for cmd in &object.commands {
        match cmd {
            Command::ChangeSection { section } => {
                current = SectionKind::from_u8(*section);
            }
            Command::RawData(bytes) => {
                bump_cursor(
                    &mut cursor_by_section,
                    current,
                    usize_to_u32_saturating(bytes.len()),
                );
            }
            Command::DefineSpace { size } => {
                bump_cursor(&mut cursor_by_section, current, *size);
            }
            Command::Opaque { code, payload } => {
                let write_size = opaque_write_size(*code);
                if write_size == 0 {
                    continue;
                }

                if matches!(current, SectionKind::Text | SectionKind::Data)
                    && should_relocate(*code, payload, summary, global_symbol_addrs)
                {
                    let local = cursor_by_section.get(&current).copied().unwrap_or(0);
                    let section_base = match current {
                        SectionKind::Data => total_text_size,
                        _ => 0,
                    };
                    let placed = placement.get(&current).copied().unwrap_or(0);
                    out.push(section_base.saturating_add(placed).saturating_add(local));
                }

                bump_cursor(&mut cursor_by_section, current, u32::from(write_size));
            }
            _ => {}
        }
    }
}

fn opaque_write_size(code: u16) -> u8 {
    let hi = code_hi(code);
    match hi {
        0x40 | 0x50 | opcode::OPH_WRT_STK_BYTE => 2,
        0x43 | 0x53 | 0x57 | 0x6b | opcode::OPH_WRT_STK_BYTE_RAW => 1,
        0x41 | 0x45 | 0x51 | 0x55 | 0x65 | 0x69 | opcode::OPH_WRT_STK_WORD_TEXT | opcode::OPH_WRT_STK_WORD_RELOC => 2,
        0x42 | 0x46 | 0x52 | 0x56 | 0x6a | opcode::OPH_WRT_STK_LONG | opcode::OPH_WRT_STK_LONG_ALT | opcode::OPH_WRT_STK_LONG_RELOC => 4,
        _ => 0,
    }
}

fn needs_relocation(code: u16) -> bool {
    matches!(code_hi(code), 0x42 | 0x46 | 0x52 | 0x56 | 0x6a)
}

#[derive(Clone, Copy, Debug)]
struct GlobalSymbolAddr {
    section: SectionKind,
    addr: u32,
}

#[derive(Clone, Copy, Debug)]
struct ExprEntry {
    stat: i16,
    value: i32,
}

fn walk_opaque_commands<F>(obj: &ObjectFile, mut on_opaque: F)
where
    F: FnMut(&Command, SectionKind, u32, &mut Vec<ExprEntry>),
{
    let mut current = SectionKind::Text;
    let mut cursor_by_section = BTreeMap::<SectionKind, u32>::new();
    let mut calc_stack = Vec::<ExprEntry>::new();
    for cmd in &obj.commands {
        match cmd {
            Command::ChangeSection { section } => {
                current = SectionKind::from_u8(*section);
            }
            Command::RawData(bytes) => {
                bump_cursor(
                    &mut cursor_by_section,
                    current,
                    usize_to_u32_saturating(bytes.len()),
                );
            }
            Command::DefineSpace { size } => {
                bump_cursor(&mut cursor_by_section, current, *size);
            }
            Command::Opaque { code, .. } => {
                let local = cursor_by_section.get(&current).copied().unwrap_or(0);
                on_opaque(cmd, current, local, &mut calc_stack);
                bump_cursor(
                    &mut cursor_by_section,
                    current,
                    u32::from(opaque_write_size(*code)),
                );
            }
            _ => {}
        }
    }
}

fn build_global_symbol_addrs(
    summaries: &[ObjectSummary],
    layout: &LayoutPlan,
    text_size: u32,
    data_size: u32,
    bss_only: u32,
    common_only: u32,
) -> HashMap<Vec<u8>, GlobalSymbolAddr> {
    let mut map = HashMap::new();
    for (idx, summary) in summaries.iter().enumerate() {
        let placement = &layout.placements[idx].by_section;
        for sym in &summary.symbols {
            let addr = match sym.section {
                SectionKind::Text => placement
                    .get(&SectionKind::Text)
                    .copied()
                    .unwrap_or(0)
                    .saturating_add(sym.value),
                SectionKind::Data => text_size
                    .saturating_add(placement.get(&SectionKind::Data).copied().unwrap_or(0))
                    .saturating_add(sym.value),
                SectionKind::Bss => text_size
                    .saturating_add(data_size)
                    .saturating_add(placement.get(&SectionKind::Bss).copied().unwrap_or(0))
                    .saturating_add(sym.value),
                SectionKind::Stack => text_size
                    .saturating_add(data_size)
                    .saturating_add(bss_only)
                    .saturating_add(common_only)
                    .saturating_add(placement.get(&SectionKind::Stack).copied().unwrap_or(0))
                    .saturating_add(sym.value),
                SectionKind::Common => text_size
                    .saturating_add(data_size)
                    .saturating_add(bss_only)
                    .saturating_add(sym.value),
                _ => sym.value,
            };
            map.insert(
                sym.name.clone(),
                GlobalSymbolAddr {
                    section: sym.section,
                    addr,
                },
            );
        }
    }
    map
}

fn patch_opaque_commands(
    linked: &mut BTreeMap<SectionKind, Vec<u8>>,
    objects: &[ObjectFile],
    summaries: &[ObjectSummary],
    layout: &LayoutPlan,
    global_symbol_addrs: &HashMap<Vec<u8>, GlobalSymbolAddr>,
) {
    for (idx, (obj, summary)) in objects.iter().zip(summaries.iter()).enumerate() {
        walk_opaque_commands(obj, |cmd, current, local, calc_stack| {
            let Command::Opaque { code, payload } = cmd else {
                return;
            };
            let [hi, lo] = code.to_be_bytes();
            if hi == 0x80 {
                if let Some(entry) =
                    evaluate_push_80_for_patch(lo, payload, summary, global_symbol_addrs)
                {
                    calc_stack.push(entry);
                }
            } else if hi == 0xa0 {
                let _ = expr::evaluate_a0(lo, calc_stack);
            }
            if let Some(bytes) =
                materialize_stack_write_opaque(*code, calc_stack).or_else(|| {
                    materialize_opaque(
                        *code,
                        payload,
                        summary,
                        global_symbol_addrs,
                        &layout.placements[idx].by_section,
                    )
                })
                .and_then(|v| if is_initialized_section(current) { Some(v) } else { None })
            {
                let Some(start) = layout.placements[idx].by_section.get(&current).copied() else {
                    return;
                };
                let Some(target) = linked.get_mut(&current) else {
                    return;
                };
                let begin = (start.saturating_add(local)) as usize;
                let end = begin.saturating_add(bytes.len());
                if end <= target.len() {
                    target[begin..end].copy_from_slice(&bytes);
                }
            }
        });
    }
}

fn materialize_stack_write_opaque(code: u16, calc_stack: &mut Vec<ExprEntry>) -> Option<Vec<u8>> {
    let hi = code_hi(code);
    let v = match hi {
        opcode::OPH_WRT_STK_BYTE
        | opcode::OPH_WRT_STK_WORD_TEXT
        | opcode::OPH_WRT_STK_LONG
        | opcode::OPH_WRT_STK_BYTE_RAW
        | opcode::OPH_WRT_STK_LONG_ALT
        | opcode::OPH_WRT_STK_WORD_RELOC
        | opcode::OPH_WRT_STK_LONG_RELOC => calc_stack.pop()?,
        _ => return None,
    };
    let out = match hi {
        opcode::OPH_WRT_STK_BYTE => vec![0x00, i32_low_u8(v.value)],
        opcode::OPH_WRT_STK_BYTE_RAW => vec![i32_low_u8(v.value)],
        opcode::OPH_WRT_STK_WORD_TEXT | opcode::OPH_WRT_STK_WORD_RELOC => {
            i32_low_u16(v.value).to_be_bytes().to_vec()
        }
        opcode::OPH_WRT_STK_LONG | opcode::OPH_WRT_STK_LONG_ALT | opcode::OPH_WRT_STK_LONG_RELOC => {
            i32_bits_to_u32(v.value).to_be_bytes().to_vec()
        }
        _ => return None,
    };
    Some(out)
}

fn evaluate_push_80_for_patch(
    lo: u8,
    payload: &[u8],
    summary: &ObjectSummary,
    global_symbol_addrs: &HashMap<Vec<u8>, GlobalSymbolAddr>,
) -> Option<ExprEntry> {
    if matches!(lo, 0xfc..=0xff) {
        let label_no = read_u16_be(payload)?;
        let xref = summary.xrefs.iter().find(|x| x.value == u32::from(label_no))?;
        let sym = global_symbol_addrs.get(&xref.name)?;
        let stat = match expr::section_stat(sym.section) {
            0 => 0,
            1 => 1,
            _ => 2,
        };
        return Some(ExprEntry {
            stat,
            value: u32_bits_to_i32(sym.addr),
        });
    }
    if lo <= 0x0a {
        let value = read_i32_be(payload)?;
        let stat = match lo {
            0x00 => 0,
            0x01..=0x04 => 1,
            _ => 2,
        };
        return Some(ExprEntry { stat, value });
    }
    None
}

fn materialize_opaque(
    code: u16,
    payload: &[u8],
    summary: &ObjectSummary,
    global_symbol_addrs: &HashMap<Vec<u8>, GlobalSymbolAddr>,
    placement: &BTreeMap<SectionKind, u32>,
) -> Option<Vec<u8>> {
    let hi = code_hi(code);
    let value = resolve_opaque_value(code, payload, summary, global_symbol_addrs, placement)?;
    match hi {
        0x40 | 0x50 => Some(vec![0x00, i32_low_u8(value)]),
        0x43 | 0x47 | 0x53 | 0x57 | 0x6b => Some(vec![i32_low_u8(value)]),
        0x41 | 0x45 | 0x51 | 0x55 | 0x65 | 0x69 => Some(i32_low_u16(value).to_be_bytes().to_vec()),
        0x42 | 0x46 | 0x52 | 0x56 | 0x6a => Some(i32_bits_to_u32(value).to_be_bytes().to_vec()),
        _ => None,
    }
}

fn resolve_opaque_value(
    code: u16,
    payload: &[u8],
    summary: &ObjectSummary,
    global_symbol_addrs: &HashMap<Vec<u8>, GlobalSymbolAddr>,
    placement: &BTreeMap<SectionKind, u32>,
) -> Option<i32> {
    let hi = code_hi(code);
    let lo = code_lo(code);

    if matches!(hi, 0x65 | 0x69 | 0x6a | 0x6b) {
        let adr = read_i32_be(payload)?;
        let label_no = read_u16_be(payload.get(4..)?)?;
        let xref = summary.xrefs.iter().find(|x| x.value == u32::from(label_no))?;
        let sym = global_symbol_addrs.get(&xref.name)?;
        let base = section_value_with_placement(lo, adr, placement)?;
        return Some(u32_bits_to_i32(sym.addr).wrapping_sub(base));
    }

    let mut base = if matches!(lo, 0xfc..=0xff) {
        let label_no = read_u16_be(payload)?;
        let xref = summary.xrefs.iter().find(|x| x.value == u32::from(label_no))?;
        global_symbol_addrs
            .get(&xref.name)
            .map(|v| u32_bits_to_i32(v.addr))?
    } else if (0x01..=0x0a).contains(&lo) {
        let v = read_i32_be(payload)?;
        section_value_with_placement(lo, v, placement)?
    } else if lo == 0x00 {
        read_i32_be(payload)?
    } else {
        return None;
    };

    if matches!(hi, 0x50 | 0x51 | 0x52 | 0x53 | 0x55 | 0x56 | 0x57) {
        let off_pos = if matches!(lo, 0xfc..=0xff) { 2 } else { 4 };
        let off = read_i32_be(payload.get(off_pos..)?)?;
        base = base.wrapping_add(off);
    }

    Some(base)
}

fn section_value_with_placement(lo: u8, value: i32, placement: &BTreeMap<SectionKind, u32>) -> Option<i32> {
    let sect = match lo {
        0x01 => SectionKind::Text,
        0x02 => SectionKind::Data,
        0x03 => SectionKind::Bss,
        0x04 => SectionKind::Stack,
        0x05 => SectionKind::RData,
        0x06 => SectionKind::RBss,
        0x07 => SectionKind::RStack,
        0x08 => SectionKind::RLData,
        0x09 => SectionKind::RLBss,
        0x0a => SectionKind::RLStack,
        _ => return None,
    };
    let base = u32_bits_to_i32(placement.get(&sect).copied().unwrap_or(0));
    Some(base.wrapping_add(value))
}

fn should_relocate(
    code: u16,
    payload: &[u8],
    summary: &ObjectSummary,
    global_symbol_addrs: &HashMap<Vec<u8>, GlobalSymbolAddr>,
) -> bool {
    if !needs_relocation(code) {
        return false;
    }
    let lo = code_lo(code);
    if is_reloc_section(lo) {
        return true;
    }
    if lo == 0xff {
        let Some(label_no) = read_u16_be(payload) else {
            return false;
        };
        let Some(xref) = summary.xrefs.iter().find(|x| x.value == u32::from(label_no)) else {
            return false;
        };
        let Some(sym) = global_symbol_addrs.get(&xref.name) else {
            return false;
        };
        return matches!(
            sym.section,
            SectionKind::Text | SectionKind::Data | SectionKind::Bss | SectionKind::Stack | SectionKind::Common
        );
    }
    false
}

fn is_reloc_section(sect: u8) -> bool {
    matches!(sect, 0x01..=0x0a)
}

fn bump_cursor(map: &mut BTreeMap<SectionKind, u32>, section: SectionKind, add: u32) {
    let entry = map.entry(section).or_insert(0);
    *entry = entry.saturating_add(add);
}

fn encode_relocation_offsets(offsets: &[u32]) -> Vec<u8> {
    let mut out = Vec::new();
    for &off in offsets {
        if off < 0x10000 && off != 1 {
            let short = u16::try_from(off).expect("off < 0x10000");
            out.extend_from_slice(&short.to_be_bytes());
        } else {
            out.extend_from_slice(&1u16.to_be_bytes());
            out.extend_from_slice(&off.to_be_bytes());
        }
    }
    out
}

fn patch_mcs_size(payload: &mut [u8], bss_size: u32) -> Result<()> {
    if payload.len() < 14 {
        bail!("not MACS format");
    }
    if &payload[0..4] != b"MACS" || &payload[4..8] != b"DATA" {
        bail!("not MACS format");
    }
    let total_size = u32::try_from(payload.len())
        .unwrap_or(u32::MAX)
        .saturating_add(bss_size);
    put_u32_be(payload, 10, total_size);
    Ok(())
}

fn code_hi(code: u16) -> u8 {
    code.to_be_bytes()[0]
}

fn code_lo(code: u16) -> u8 {
    code.to_be_bytes()[1]
}

fn i32_low_u8(value: i32) -> u8 {
    value.to_be_bytes()[3]
}

fn i32_low_u16(value: i32) -> u16 {
    let b = value.to_be_bytes();
    u16::from_be_bytes([b[2], b[3]])
}

fn i32_bits_to_u32(value: i32) -> u32 {
    u32::from_be_bytes(value.to_be_bytes())
}

fn u32_bits_to_i32(value: u32) -> i32 {
    i32::from_be_bytes(value.to_be_bytes())
}

fn usize_to_u32_saturating(value: usize) -> u32 {
    u32::try_from(value).unwrap_or(u32::MAX)
}

#[derive(Clone, Copy)]
struct XHeader {
    text_size: u32,
    data_size: u32,
    bss_size: u32,
    reloc_size: u32,
    symbol_size: u32,
    scd_line_size: u32,
    scd_info_size: u32,
    scd_name_size: u32,
    exec: u32,
}

fn build_x_header(xh: XHeader) -> Vec<u8> {
    let mut h = vec![0u8; 64];
    // 'HU'
    h[0] = b'H';
    h[1] = b'U';
    // load mode = 0, base = 0
    put_u32_be(&mut h, 8, xh.exec);
    put_u32_be(&mut h, 12, xh.text_size);
    put_u32_be(&mut h, 16, xh.data_size);
    put_u32_be(&mut h, 20, xh.bss_size);
    put_u32_be(&mut h, 24, xh.reloc_size);
    put_u32_be(&mut h, 28, xh.symbol_size);
    put_u32_be(&mut h, 32, xh.scd_line_size);
    put_u32_be(&mut h, 36, xh.scd_info_size);
    put_u32_be(&mut h, 40, xh.scd_name_size);
    h
}

fn resolve_exec_address(
    summaries: &[ObjectSummary],
    text_size: u32,
    data_size: u32,
    _bss_size: u32,
) -> Result<Option<u32>> {
    let starts = summaries
        .iter()
        .filter_map(|s| s.start_address)
        .collect::<Vec<_>>();
    if starts.len() > 1 {
        bail!("multiple start addresses are specified");
    }
    let Some(start) = starts.first().copied() else {
        return Ok(None);
    };
    let (sect, addr) = start;
    let base = match sect {
        0x02 => text_size,
        0x03 => text_size.saturating_add(data_size),
        _ => 0,
    };
    Ok(Some(base.saturating_add(addr)))
}

fn validate_link_inputs(
    objects: &[ObjectFile],
    input_paths: &[String],
    summaries: &[ObjectSummary],
    g2lk_mode: bool,
) -> Result<()> {
    validate_unsupported_expression_commands(objects, input_paths, summaries, g2lk_mode)
}

#[allow(clippy::similar_names)]
fn validate_unsupported_expression_commands(
    objects: &[ObjectFile],
    input_paths: &[String],
    summaries: &[ObjectSummary],
    g2lk_mode: bool,
) -> Result<()> {
    let mut global_symbols = HashMap::<Vec<u8>, Symbol>::new();
    for summary in summaries {
        for sym in &summary.symbols {
            global_symbols.insert(sym.name.clone(), sym.clone());
        }
    }

    let mut diagnostics = Vec::<String>::new();
    for (obj_idx, (obj, summary)) in objects.iter().zip(summaries.iter()).enumerate() {
        let obj_name = input_paths
            .get(obj_idx)
            .and_then(|p| std::path::Path::new(p).file_name())
            .and_then(|s| s.to_str())
            .map_or_else(|| format!("obj{obj_idx}.o"), std::borrow::ToOwned::to_owned);
        let mut has_ctor = false;
        let mut has_dtor = false;
        let mut has_doctor = false;
        let mut has_dodtor = false;
        let mut ctor_count = 0usize;
        let mut dtor_count = 0usize;
        let mut ctor_header_size = None::<u32>;
        let mut dtor_header_size = None::<u32>;
        for cmd in &obj.commands {
            if let Command::Header { section, size, .. } = cmd {
                match *section {
                    0x0c => ctor_header_size = Some(*size),
                    0x0d => dtor_header_size = Some(*size),
                    _ => {}
                }
            }
        }
        walk_opaque_commands(obj, |cmd, current, local, calc_stack| {
            let Command::Opaque { code, .. } = cmd else {
                return;
            };
            match *code {
                opcode::OP_CTOR_ENTRY => {
                    has_ctor = true;
                    ctor_count += 1;
                }
                opcode::OP_DTOR_ENTRY => {
                    has_dtor = true;
                    dtor_count += 1;
                }
                opcode::OP_DOCTOR => has_doctor = true,
                opcode::OP_DODTOR => has_dodtor = true,
                _ => {}
            }
            let messages = expr::classify_expression_errors(
                *code,
                cmd,
                summary,
                &global_symbols,
                current,
                calc_stack,
            );
            for msg in messages {
                diagnostics.push(format!(
                    "{msg} in {obj_name}\n at {local:08x} ({})",
                    section_name(current)
                ));
            }
        });
        if g2lk_mode {
            if has_ctor && !has_doctor {
                diagnostics.push(format!(".doctor なしで .ctor が使われています in {obj_name}"));
            }
            if has_dtor && !has_dodtor {
                diagnostics.push(format!(".dodtor なしで .dtor が使われています in {obj_name}"));
            }
        } else if has_ctor || has_dtor || has_doctor || has_dodtor {
            diagnostics.push(format!(
                "(do)ctor/dtor には -1 オプションの指定が必要です。 in {obj_name}"
            ));
        }
        if let Some(size) = ctor_header_size {
            let expected = usize_to_u32_saturating(ctor_count).saturating_mul(4);
            if size != expected {
                diagnostics.push(format!(
                    "ctor header size mismatch in {obj_name}: header={size} expected={expected}"
                ));
            }
        }
        if let Some(size) = dtor_header_size {
            let expected = usize_to_u32_saturating(dtor_count).saturating_mul(4);
            if size != expected {
                diagnostics.push(format!(
                    "dtor header size mismatch in {obj_name}: header={size} expected={expected}"
                ));
            }
        }
    }
    if diagnostics.is_empty() {
        return Ok(());
    }
    bail!("{}", diagnostics.join("\n"));
}


fn read_u16_be(bytes: &[u8]) -> Option<u16> {
    if bytes.len() < 2 {
        return None;
    }
    Some(u16::from_be_bytes([bytes[0], bytes[1]]))
}

fn read_i32_be(bytes: &[u8]) -> Option<i32> {
    if bytes.len() < 4 {
        return None;
    }
    Some(i32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

fn read_u32_be(bytes: &[u8]) -> Option<u32> {
    if bytes.len() < 4 {
        return None;
    }
    Some(u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

fn section_name(section: SectionKind) -> &'static str {
    match section {
        SectionKind::Text => "text",
        SectionKind::Data => "data",
        SectionKind::Bss => "bss",
        SectionKind::Stack => "stack",
        SectionKind::RData => "rdata",
        SectionKind::RBss => "rbss",
        SectionKind::RStack => "rstack",
        SectionKind::RLData => "rldata",
        SectionKind::RLBss => "rlbss",
        SectionKind::RLStack => "rlstack",
        _ => "abs",
    }
}

fn put_u32_be(buf: &mut [u8], at: usize, v: u32) {
    let b = v.to_be_bytes();
    buf[at..at + 4].copy_from_slice(&b);
}

#[cfg(test)]
mod tests;
