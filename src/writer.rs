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
pub(crate) use map::{MapSizes, build_map_text};
mod ctor_dtor;
mod opcode;
mod expr;

const CTOR_LIST_SYM: &[u8] = b"___CTOR_LIST__";
const DTOR_LIST_SYM: &[u8] = b"___DTOR_LIST__";

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
        validate_r_convertibility(objects, summaries, layout, output_path, options.g2lk_mode)?;
    }

    let mut payload = if matches!(options.format, OutputFormat::R | OutputFormat::Mcs) {
        build_r_payload(
            objects,
            summaries,
            layout,
            matches!(options.bss_policy, BssPolicy::Omit),
            options.g2lk_mode,
        )?
    } else {
        build_x_image_with_options(
            objects,
            summaries,
            layout,
            matches!(options.symbol_table, SymbolTablePolicy::Keep),
            options.g2lk_mode,
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
            bss_common_stack_total(layout)
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

fn bss_common_stack_total(layout: &LayoutPlan) -> u32 {
    section_total(layout, SectionKind::Bss)
        .saturating_add(section_total(layout, SectionKind::Common))
        .saturating_add(section_total(layout, SectionKind::Stack))
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
    build_x_image_with_options(objects, summaries, layout, true, false)
}

fn build_x_image_with_options(
    objects: &[ObjectFile],
    summaries: &[ObjectSummary],
    layout: &LayoutPlan,
    include_symbols: bool,
    g2lk_mode: bool,
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
    let (data_size, g2lk_synth) = extend_data_for_g2lk(&mut linked, objects, g2lk_mode, text_size, data_size);
    let bss_only = section_total(layout, SectionKind::Bss);
    let common_only = section_total(layout, SectionKind::Common);
    let stack_only = section_total(layout, SectionKind::Stack);
    let bss_size = bss_only
        .saturating_add(common_only)
        .saturating_add(stack_only);

    let global_symbol_addrs = build_global_symbol_addrs_with_g2lk(
        summaries,
        layout,
        text_size,
        data_size,
        bss_only,
        common_only,
        g2lk_synth,
    );
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
            g2lk_synth,
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
    let mut line_table = Vec::new();
    let mut info_table = Vec::new();
    let mut name_table = Vec::new();
    let mut sinfo_pos_entries = 0u32;

    for (idx, obj) in objects.iter().enumerate() {
        let tail = &obj.scd_tail;
        let Some(view) = parse_scd_tail_view(tail) else {
            continue;
        };
        // HLK make_scdinfo compatible line fixup:
        // - location!=0 : + text_pos
        // - location==0 : + cumulative sinfo_pos(entries)
        let text_pos = layout.placements[idx]
            .by_section
            .get(&SectionKind::Text)
            .copied()
            .unwrap_or(0);
        line_table.extend_from_slice(&rebase_scd_line_table(view.linfo, text_pos, sinfo_pos_entries));
        info_table.extend_from_slice(&rebase_scd_info_table(
            view.sinfo_plus_einfo,
            view.ninfo,
            view.sinfo_count,
            sinfo_pos_entries,
            &layout.placements[idx].by_section,
            layout,
            &xdefs,
        )?);
        name_table.extend_from_slice(view.ninfo);

        sinfo_pos_entries = sinfo_pos_entries.saturating_add(view.sinfo_count);
    }

    Ok((line_table, info_table, name_table))
}

struct ScdTailView<'a> {
    linfo: &'a [u8],
    sinfo_plus_einfo: &'a [u8],
    ninfo: &'a [u8],
    sinfo_count: u32,
}

fn parse_scd_tail_view(tail: &[u8]) -> Option<ScdTailView<'_>> {
    const SCD_HEADER_SIZE: usize = 12;
    if tail.len() < SCD_HEADER_SIZE {
        return None;
    }

    let linfo_size = u32::from_be_bytes([tail[0], tail[1], tail[2], tail[3]]) as usize;
    let sinfo_plus_einfo_size = u32::from_be_bytes([tail[4], tail[5], tail[6], tail[7]]) as usize;
    let ninfo_size = u32::from_be_bytes([tail[8], tail[9], tail[10], tail[11]]) as usize;
    let total = SCD_HEADER_SIZE
        .saturating_add(linfo_size)
        .saturating_add(sinfo_plus_einfo_size)
        .saturating_add(ninfo_size);
    if total > tail.len() {
        return None;
    }

    let linfo_start = SCD_HEADER_SIZE;
    let sinfo_start = linfo_start + linfo_size;
    let ninfo_start = sinfo_start + sinfo_plus_einfo_size;
    let linfo = &tail[linfo_start..sinfo_start];
    let sinfo_plus_einfo = &tail[sinfo_start..ninfo_start];
    let ninfo = &tail[ninfo_start..ninfo_start + ninfo_size];
    let sinfo_count = extract_sinfo_count(tail, linfo_size);
    Some(ScdTailView {
        linfo,
        sinfo_plus_einfo,
        ninfo,
        sinfo_count,
    })
}

fn rebase_scd_line_table(input: &[u8], text_pos: u32, sinfo_pos_entries: u32) -> Vec<u8> {
    const LINE_ENTRY_SIZE: usize = 6;
    if !input.len().is_multiple_of(LINE_ENTRY_SIZE) {
        return input.to_vec();
    }
    let mut out = Vec::with_capacity(input.len());
    let mut entry_offset = 0usize;
    while entry_offset + LINE_ENTRY_SIZE <= input.len() {
        let mut loc = u32::from_be_bytes([
            input[entry_offset],
            input[entry_offset + 1],
            input[entry_offset + 2],
            input[entry_offset + 3],
        ]);
        if loc != 0 {
            loc = loc.saturating_add(text_pos);
        } else {
            loc = loc.saturating_add(sinfo_pos_entries);
        }
        out.extend_from_slice(&loc.to_be_bytes());
        out.push(input[entry_offset + 4]);
        out.push(input[entry_offset + 5]);
        entry_offset += LINE_ENTRY_SIZE;
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
    const SCD_INFO_ENTRY_SIZE: usize = 18;
    let mut out = input.to_vec();
    let sinfo_bytes = (sinfo_count as usize)
        .saturating_mul(SCD_INFO_ENTRY_SIZE)
        .min(out.len());
    rebase_sinfo_entries(&mut out, sinfo_bytes, placement, layout)?;
    // Minimal einfo fixup:
    // if entry starts with d6==0, the next long is sinfo index and must be rebased.
    rebase_einfo_entries(
        &mut out,
        sinfo_bytes,
        sinfo_pos_entries,
        ninfo,
        placement,
        layout,
        xdefs,
    )?;
    Ok(out)
}

fn rebase_sinfo_entries(
    out: &mut [u8],
    sinfo_bytes: usize,
    placement: &BTreeMap<SectionKind, u32>,
    layout: &LayoutPlan,
) -> Result<()> {
    const SCD_INFO_ENTRY_SIZE: usize = 18;
    let mut sinfo_offset = 0usize;
    while sinfo_offset + SCD_INFO_ENTRY_SIZE <= sinfo_bytes {
        let sect = u16::from_be_bytes([out[sinfo_offset + 12], out[sinfo_offset + 13]]);
        if let Some(delta) = sinfo_section_delta(sect, placement, layout)? {
            if delta != 0 {
                adjust_u32_at(out, sinfo_offset + 8, delta);
            }
        }
        sinfo_offset += SCD_INFO_ENTRY_SIZE;
    }
    Ok(())
}

fn rebase_einfo_entries(
    out: &mut [u8],
    sinfo_bytes: usize,
    sinfo_pos_entries: u32,
    ninfo: &[u8],
    placement: &BTreeMap<SectionKind, u32>,
    layout: &LayoutPlan,
    xdefs: &HashMap<Vec<u8>, ScdXdef>,
) -> Result<()> {
    const SCD_INFO_ENTRY_SIZE: usize = 18;
    let mut einfo_offset = sinfo_bytes;
    while einfo_offset + SCD_INFO_ENTRY_SIZE <= out.len() {
        rebase_einfo_entry(out, einfo_offset, sinfo_pos_entries, ninfo, placement, layout, xdefs)?;
        einfo_offset += SCD_INFO_ENTRY_SIZE;
    }
    Ok(())
}

fn rebase_einfo_entry(
    out: &mut [u8],
    einfo_offset: usize,
    sinfo_pos_entries: u32,
    ninfo: &[u8],
    placement: &BTreeMap<SectionKind, u32>,
    layout: &LayoutPlan,
    xdefs: &HashMap<Vec<u8>, ScdXdef>,
) -> Result<()> {
    const SCD_INFO_ENTRY_SIZE: usize = 18;
    let d6 = u32::from_be_bytes([
        out[einfo_offset],
        out[einfo_offset + 1],
        out[einfo_offset + 2],
        out[einfo_offset + 3],
    ]);
    let sect = u16::from_be_bytes([out[einfo_offset + 8], out[einfo_offset + 9]]);
    if d6 == 0 {
        let ref_idx = u32::from_be_bytes([
            out[einfo_offset + 4],
            out[einfo_offset + 5],
            out[einfo_offset + 6],
            out[einfo_offset + 7],
        ]);
        if ref_idx != 0 {
            let rebased = ref_idx.saturating_add(sinfo_pos_entries);
            out[einfo_offset + 4..einfo_offset + 8].copy_from_slice(&rebased.to_be_bytes());
        }
        return Ok(());
    }

    if matches!(sect, 0x0004 | 0x0007 | 0x000a) {
        bail!("unsupported SCD einfo section for d6!=0: {sect:#06x}");
    }
    if matches!(sect, 0x00fc..=0x00fe | 0xfffc..=0xfffe) {
        let name = decode_scd_entry_name(&out[einfo_offset..einfo_offset + SCD_INFO_ENTRY_SIZE], ninfo)
            .with_context(|| format!("invalid SCD einfo name at offset {einfo_offset}"))?;
        let (resolved_off, resolved_sect) = resolve_scd_common_reference(&name, xdefs)?;
        out[einfo_offset + 4..einfo_offset + 8].copy_from_slice(&resolved_off.to_be_bytes());
        out[einfo_offset + 8..einfo_offset + 10].copy_from_slice(&resolved_sect.to_be_bytes());
        return Ok(());
    }
    if let Some(delta) = einfo_section_delta(sect, placement, layout) {
        if delta != 0 {
            adjust_u32_at(out, einfo_offset + 4, delta);
        }
    }
    Ok(())
}

fn resolve_scd_common_reference(
    name: &[u8],
    xdefs: &HashMap<Vec<u8>, ScdXdef>,
) -> Result<(u32, u16)> {
    let Some(xdef) = xdefs.get(name) else {
        bail!(
            "unresolved SCD einfo common-reference for d6!=0: {}",
            String::from_utf8_lossy(name)
        );
    };
    match xdef.section {
        SectionKind::Common => Ok((xdef.value, 0x0003)),
        SectionKind::RCommon => Ok((xdef.value, 0x0006)),
        SectionKind::RLCommon => Ok((xdef.value, 0x0009)),
        _ => bail!(
            "unsupported SCD einfo common-reference target section: {:?}",
            xdef.section
        ),
    }
}

fn adjust_u32_at(bytes: &mut [u8], offset: usize, delta: i64) {
    let base = u32::from_be_bytes([
        bytes[offset],
        bytes[offset + 1],
        bytes[offset + 2],
        bytes[offset + 3],
    ]);
    let adjusted = base.wrapping_add(i64_low_u32(delta));
    bytes[offset..offset + 4].copy_from_slice(&adjusted.to_be_bytes());
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

#[derive(Debug, Clone, Copy)]
struct CommonSymbolStats {
    section: SectionKind,
    max_size: u32,
    first_seen_order: usize,
    has_conflicting_section: bool,
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

    // Collect common/rcommon/rlcommon candidates and keep HLK-like first appearance order.
    let mut common_candidates = HashMap::<Vec<u8>, CommonSymbolStats>::new();
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
            let stats = common_candidates
                .entry(sym.name.clone())
                .or_insert(CommonSymbolStats {
                    section: sym.section,
                    max_size: size,
                    first_seen_order: order,
                    has_conflicting_section: false,
                });
            order = order.saturating_add(1);
            if stats.section != sym.section {
                stats.has_conflicting_section = true;
                continue;
            }
            if size > stats.max_size {
                stats.max_size = size;
            }
        }
    }

    let mut ordered_candidates = common_candidates.into_iter().collect::<Vec<_>>();
    ordered_candidates.sort_by_key(|(_, stats)| stats.first_seen_order);
    let mut common_cursor = 0u32;
    let mut rcommon_cursor = 0u32;
    let mut rlcommon_cursor = 0u32;
    for (name, stats) in ordered_candidates {
        if stats.has_conflicting_section || non_common.contains(&name) || xdefs.contains_key(&name) {
            continue;
        }
        let offset = match stats.section {
            SectionKind::Common => {
                let current = common_cursor;
                common_cursor = common_cursor.saturating_add(stats.max_size);
                current
            }
            SectionKind::RCommon => {
                let current = rcommon_cursor;
                rcommon_cursor = rcommon_cursor.saturating_add(stats.max_size);
                current
            }
            SectionKind::RLCommon => {
                let current = rlcommon_cursor;
                rlcommon_cursor = rlcommon_cursor.saturating_add(stats.max_size);
                current
            }
            _ => continue,
        };
        xdefs.insert(
            name,
            ScdXdef {
                section: stats.section,
                value: offset,
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
    let totals = SectionTotals::from_layout(layout);
    let pos = SectionPlacement::new(placement);

    match sect {
        0x0001 => Some(pos.section(SectionKind::Text)),
        0x0002 => Some(pos.section(SectionKind::Data)),
        0x0003 => Some(totals.text + totals.data + pos.section(SectionKind::Bss) - totals.object_size()),
        0x0005 => Some(pos.section(SectionKind::RData)),
        0x0006 => Some(pos.section(SectionKind::RBss)),
        0x0008 => Some(pos.section(SectionKind::RLData)),
        0x0009 => Some(pos.section(SectionKind::RLBss)),
        _ => None,
    }
}

#[derive(Clone, Copy)]
struct SectionTotals {
    text: i64,
    data: i64,
    bss: i64,
    common: i64,
    stack: i64,
}

impl SectionTotals {
    fn from_layout(layout: &LayoutPlan) -> Self {
        Self {
            text: i64::from(section_total(layout, SectionKind::Text)),
            data: i64::from(section_total(layout, SectionKind::Data)),
            bss: i64::from(section_total(layout, SectionKind::Bss)),
            common: i64::from(section_total(layout, SectionKind::Common)),
            stack: i64::from(section_total(layout, SectionKind::Stack)),
        }
    }

    fn object_size(self) -> i64 {
        self.text + self.data + self.bss + self.common + self.stack
    }
}

struct SectionPlacement<'a> {
    placement: &'a BTreeMap<SectionKind, u32>,
}

impl SectionPlacement<'_> {
    fn new(placement: &BTreeMap<SectionKind, u32>) -> SectionPlacement<'_> {
        SectionPlacement { placement }
    }

    fn section(&self, section: SectionKind) -> i64 {
        i64::from(self.placement.get(&section).copied().unwrap_or(0))
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
    g2lk_synth: Option<G2lkSyntheticSymbols>,
) -> Vec<u8> {
    let mut out = Vec::new();
    if let Some(synth) = g2lk_synth {
        append_symbol_entry(&mut out, 0x0202, synth.ctor_addr, CTOR_LIST_SYM);
        append_symbol_entry(&mut out, 0x0202, synth.dtor_addr, DTOR_LIST_SYM);
    }
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

fn append_symbol_entry(out: &mut Vec<u8>, ty: u16, addr: u32, name: &[u8]) {
    out.extend_from_slice(&ty.to_be_bytes());
    out.extend_from_slice(&addr.to_be_bytes());
    out.extend_from_slice(name);
    out.push(0);
    if !out.len().is_multiple_of(2) {
        out.push(0);
    }
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
    g2lk_mode: bool,
) -> Result<Vec<u8>> {
    let mut linked = link_initialized_sections(
        objects,
        summaries,
        layout,
        &[SectionKind::Text, SectionKind::Data, SectionKind::RData, SectionKind::RLData],
    )?;

    let text_size = linked
        .get(&SectionKind::Text)
        .map_or(0, |v| usize_to_u32_saturating(v.len()));
    let data_size = linked
        .get(&SectionKind::Data)
        .map_or(0, |v| usize_to_u32_saturating(v.len()));
    let (data_size, g2lk_synth) = extend_data_for_g2lk(&mut linked, objects, g2lk_mode, text_size, data_size);
    let bss_only = section_total(layout, SectionKind::Bss);
    let common_only = section_total(layout, SectionKind::Common);
    let global_symbol_addrs = build_global_symbol_addrs_with_g2lk(
        summaries,
        layout,
        text_size,
        data_size,
        bss_only,
        common_only,
        g2lk_synth,
    );
    patch_opaque_commands(
        &mut linked,
        objects,
        summaries,
        layout,
        &global_symbol_addrs,
    );
    ctor_dtor::patch_ctor_dtor_tables(&mut linked, objects, layout, &global_symbol_addrs, text_size)?;

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
        let total = usize::try_from(bss_common_stack_total(layout)).unwrap_or(usize::MAX);
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
        let total = usize::try_from(section_total(layout, section)).unwrap_or(usize::MAX);
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
    let mut by_section = BTreeMap::<SectionKind, Vec<u8>>::new();

    walk_commands(object, |cmd, current, local, _calc_stack| {
        if !is_initialized_section(current) {
            return;
        }
        match cmd {
            Command::RawData(data) => {
                let begin = u32_to_usize_saturating(local);
                let end = begin.saturating_add(data.len());
                let entry = by_section.entry(current).or_default();
                if entry.len() < end {
                    entry.resize(end, 0);
                }
                entry[begin..end].copy_from_slice(data);
            }
            Command::DefineSpace { size } => {
                let end = u32_to_usize_saturating(local.saturating_add(*size));
                let entry = by_section.entry(current).or_default();
                if entry.len() < end {
                    entry.resize(end, 0);
                }
            }
            Command::Opaque { code, .. } => {
                let write_size = usize::from(opaque_write_size(*code));
                if write_size == 0 {
                    return;
                }
                let begin = u32_to_usize_saturating(local);
                let end = begin.saturating_add(write_size);
                let entry = by_section.entry(current).or_default();
                if entry.len() < end {
                    entry.resize(end, 0);
                }
            }
            Command::Header { .. }
            | Command::ChangeSection { .. }
            | Command::DefineSymbol { .. }
            | Command::Request { .. }
            | Command::StartAddress { .. }
            | Command::SourceFile { .. }
            | Command::End => {}
        }
    });

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
    g2lk_mode: bool,
) -> Result<()> {
    let text_size = section_total(layout, SectionKind::Text);
    let data_size = section_total(layout, SectionKind::Data);
    let bss_only = section_total(layout, SectionKind::Bss);
    let common_only = section_total(layout, SectionKind::Common);
    let g2lk_synth = compute_g2lk_synthetic_symbols(objects, g2lk_mode, text_size, data_size);
    let global_symbol_addrs = build_global_symbol_addrs_with_g2lk(
        summaries,
        layout,
        text_size,
        data_size,
        bss_only,
        common_only,
        g2lk_synth,
    );
    let reloc = build_relocation_table(objects, summaries, layout, text_size, &global_symbol_addrs)?;
    if !reloc.is_empty() {
        bail!(
            "再配置テーブルが使われています: {}",
            to_human68k_path(Path::new(output_path))
        );
    }

    let bss_size = bss_common_stack_total(layout);
    let exec = resolve_exec_address(summaries, text_size, data_size, bss_size)?.unwrap_or(0);
    if exec != 0 {
        bail!(
            "実行開始アドレスがファイル先頭ではありません: {}",
            to_human68k_path(Path::new(output_path))
        );
    }
    Ok(())
}

#[derive(Clone, Copy, Debug)]
struct G2lkSyntheticSymbols {
    ctor_addr: u32,
    dtor_addr: u32,
    data_growth: u32,
}

fn compute_g2lk_synthetic_symbols(
    objects: &[ObjectFile],
    g2lk_mode: bool,
    text_size: u32,
    data_size: u32,
) -> Option<G2lkSyntheticSymbols> {
    if !g2lk_mode {
        return None;
    }
    let mut doctor = false;
    let mut has_dodtor = false;
    let mut ctor_count = 0u32;
    let mut dtor_count = 0u32;
    for obj in objects {
        walk_commands(obj, |cmd, _current, _local, _calc_stack| {
            let Command::Opaque { code, .. } = cmd else {
                return;
            };
            match *code {
                opcode::OP_DOCTOR => doctor = true,
                opcode::OP_DODTOR => has_dodtor = true,
                opcode::OP_CTOR_ENTRY => ctor_count = ctor_count.saturating_add(1),
                opcode::OP_DTOR_ENTRY => dtor_count = dtor_count.saturating_add(1),
                _ => {}
            }
        });
    }
    let ctor_size = if doctor {
        8u32.saturating_add(ctor_count.saturating_mul(4))
    } else {
        0
    };
    let dtor_size = if has_dodtor {
        8u32.saturating_add(dtor_count.saturating_mul(4))
    } else {
        0
    };
    let ctor_addr = text_size.saturating_add(data_size);
    let dtor_addr = ctor_addr.saturating_add(ctor_size);
    Some(G2lkSyntheticSymbols {
        ctor_addr,
        dtor_addr,
        data_growth: ctor_size.saturating_add(dtor_size),
    })
}

fn inject_g2lk_symbols(
    global_symbol_addrs: &mut HashMap<Vec<u8>, GlobalSymbolAddr>,
    g2lk_synth: Option<G2lkSyntheticSymbols>,
) {
    let Some(synth) = g2lk_synth else {
        return;
    };
    global_symbol_addrs.insert(
        CTOR_LIST_SYM.to_vec(),
        GlobalSymbolAddr {
            section: SectionKind::Data,
            addr: synth.ctor_addr,
        },
    );
    global_symbol_addrs.insert(
        DTOR_LIST_SYM.to_vec(),
        GlobalSymbolAddr {
            section: SectionKind::Data,
            addr: synth.dtor_addr,
        },
    );
}

fn build_global_symbol_addrs_with_g2lk(
    summaries: &[ObjectSummary],
    layout: &LayoutPlan,
    text_size: u32,
    data_size: u32,
    bss_only: u32,
    common_only: u32,
    g2lk_synth: Option<G2lkSyntheticSymbols>,
) -> HashMap<Vec<u8>, GlobalSymbolAddr> {
    let mut global_symbol_addrs =
        build_global_symbol_addrs(summaries, layout, text_size, data_size, bss_only, common_only);
    inject_g2lk_symbols(&mut global_symbol_addrs, g2lk_synth);
    global_symbol_addrs
}

fn extend_data_for_g2lk(
    linked: &mut BTreeMap<SectionKind, Vec<u8>>,
    objects: &[ObjectFile],
    g2lk_mode: bool,
    text_size: u32,
    data_size: u32,
) -> (u32, Option<G2lkSyntheticSymbols>) {
    let g2lk_synth = compute_g2lk_synthetic_symbols(objects, g2lk_mode, text_size, data_size);
    let mut updated_data_size = data_size;
    if let Some(synth) = g2lk_synth {
        if synth.data_growth != 0 {
            let data = linked.entry(SectionKind::Data).or_default();
            let new_size = data_size.saturating_add(synth.data_growth);
            data.resize(usize::try_from(new_size).unwrap_or(usize::MAX), 0);
            updated_data_size = new_size;
        }
    }
    (updated_data_size, g2lk_synth)
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
    walk_commands(object, |cmd, current, local, _calc_stack| {
        let Command::Opaque { code, payload } = cmd else {
            return;
        };
        if opaque_write_size(*code) == 0 {
            return;
        }
        if !matches!(current, SectionKind::Text | SectionKind::Data) {
            return;
        }
        if !should_relocate(*code, payload, summary, global_symbol_addrs) {
            return;
        }
        let section_base = match current {
            SectionKind::Data => total_text_size,
            _ => 0,
        };
        let placed = placement.get(&current).copied().unwrap_or(0);
        out.push(section_base.saturating_add(placed).saturating_add(local));
    });
}

fn opaque_write_size(code: u16) -> u8 {
    let hi = code_hi(code);
    match hi {
        opcode::OPH_ABS_WORD
        | opcode::OPH_ADD_WORD
        | opcode::OPH_WRT_STK_BYTE
        | opcode::OPH_ABS_WORD_ALT
        | opcode::OPH_XREF_WORD
        | opcode::OPH_ADD_WORD_ALT
        | opcode::OPH_ADD_XREF_WORD
        | opcode::OPH_DISP_WORD
        | opcode::OPH_DISP_WORD_ALIAS
        | opcode::OPH_WRT_STK_WORD_TEXT
        | opcode::OPH_WRT_STK_WORD_RELOC => 2,
        opcode::OPH_ABS_BYTE
        | opcode::OPH_ADD_BYTE
        | opcode::OPH_ADD_XREF_BYTE
        | opcode::OPH_DISP_BYTE
        | opcode::OPH_WRT_STK_BYTE_RAW => 1,
        opcode::OPH_ABS_LONG
        | opcode::OPH_XREF_LONG
        | opcode::OPH_ADD_LONG
        | opcode::OPH_ADD_XREF_LONG
        | opcode::OPH_DISP_LONG
        | opcode::OPH_WRT_STK_LONG
        | opcode::OPH_WRT_STK_LONG_ALT
        | opcode::OPH_WRT_STK_LONG_RELOC => 4,
        _ => 0,
    }
}

fn needs_relocation(code: u16) -> bool {
    matches!(
        code_hi(code),
        opcode::OPH_ABS_LONG
            | opcode::OPH_XREF_LONG
            | opcode::OPH_ADD_LONG
            | opcode::OPH_ADD_XREF_LONG
            | opcode::OPH_DISP_LONG
    )
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

fn walk_commands<F>(obj: &ObjectFile, mut on_command: F)
where
    F: FnMut(&Command, SectionKind, u32, &mut Vec<ExprEntry>),
{
    let mut current = SectionKind::Text;
    let mut cursor_by_section = BTreeMap::<SectionKind, u32>::new();
    let mut calc_stack = Vec::<ExprEntry>::new();
    for cmd in &obj.commands {
        let local = cursor_by_section.get(&current).copied().unwrap_or(0);
        on_command(cmd, current, local, &mut calc_stack);
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
        walk_commands(obj, |cmd, current, local, calc_stack| {
            let Command::Opaque { code, payload } = cmd else {
                return;
            };
            let [hi, lo] = code.to_be_bytes();
            if hi == opcode::OPH_PUSH_VALUE_BASE {
                if let Some(entry) =
                    evaluate_push_80_for_patch(lo, payload, summary, global_symbol_addrs)
                {
                    calc_stack.push(entry);
                }
            } else if hi == opcode::OPH_EXPR_BASE {
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
    if is_common_or_xref_section(lo) {
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
    if reloc_section_kind(lo).is_some() || is_abs_section(lo) {
        let value = read_i32_be(payload)?;
        let stat = match lo {
            s if is_abs_section(s) => 0,
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
        opcode::OPH_ABS_WORD | opcode::OPH_ADD_WORD => Some(vec![0x00, i32_low_u8(value)]),
        opcode::OPH_ABS_BYTE
        | opcode::OPH_XREF_BYTE
        | opcode::OPH_ADD_BYTE
        | opcode::OPH_ADD_XREF_BYTE
        | opcode::OPH_DISP_BYTE => Some(vec![i32_low_u8(value)]),
        opcode::OPH_ABS_WORD_ALT
        | opcode::OPH_XREF_WORD
        | opcode::OPH_ADD_WORD_ALT
        | opcode::OPH_ADD_XREF_WORD
        | opcode::OPH_DISP_WORD
        | opcode::OPH_DISP_WORD_ALIAS => Some(i32_low_u16(value).to_be_bytes().to_vec()),
        opcode::OPH_ABS_LONG
        | opcode::OPH_XREF_LONG
        | opcode::OPH_ADD_LONG
        | opcode::OPH_ADD_XREF_LONG
        | opcode::OPH_DISP_LONG => Some(i32_bits_to_u32(value).to_be_bytes().to_vec()),
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

    if matches!(
        hi,
        opcode::OPH_DISP_WORD
            | opcode::OPH_DISP_WORD_ALIAS
            | opcode::OPH_DISP_LONG
            | opcode::OPH_DISP_BYTE
    ) {
        let adr = read_i32_be(payload)?;
        let label_no = read_u16_be(payload.get(4..)?)?;
        let xref = summary.xrefs.iter().find(|x| x.value == u32::from(label_no))?;
        let sym = global_symbol_addrs.get(&xref.name)?;
        let base = section_value_with_placement(lo, adr, placement)?;
        return Some(u32_bits_to_i32(sym.addr).wrapping_sub(base));
    }

    let mut base = if is_common_or_xref_section(lo) {
        let label_no = read_u16_be(payload)?;
        let xref = summary.xrefs.iter().find(|x| x.value == u32::from(label_no))?;
        global_symbol_addrs
            .get(&xref.name)
            .map(|v| u32_bits_to_i32(v.addr))?
    } else if reloc_section_kind(lo).is_some() {
        let v = read_i32_be(payload)?;
        section_value_with_placement(lo, v, placement)?
    } else if is_abs_section(lo) {
        read_i32_be(payload)?
    } else {
        return None;
    };

    if matches!(
        hi,
        opcode::OPH_ADD_WORD
            | opcode::OPH_ADD_WORD_ALT
            | opcode::OPH_ADD_LONG
            | opcode::OPH_ADD_BYTE
            | opcode::OPH_ADD_XREF_WORD
            | opcode::OPH_ADD_XREF_LONG
            | opcode::OPH_ADD_XREF_BYTE
    ) {
        let off_pos = if is_common_or_xref_section(lo) { 2 } else { 4 };
        let off = read_i32_be(payload.get(off_pos..)?)?;
        base = base.wrapping_add(off);
    }

    Some(base)
}

fn section_value_with_placement(lo: u8, value: i32, placement: &BTreeMap<SectionKind, u32>) -> Option<i32> {
    let sect = reloc_section_kind(lo)?;
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
    if is_xref_section(lo) {
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
    reloc_section_kind(sect).is_some()
}

pub(super) fn reloc_section_kind(section: u8) -> Option<SectionKind> {
    let kind = SectionKind::from_u8(section);
    match kind {
        SectionKind::Text
        | SectionKind::Data
        | SectionKind::Bss
        | SectionKind::Stack
        | SectionKind::RData
        | SectionKind::RBss
        | SectionKind::RStack
        | SectionKind::RLData
        | SectionKind::RLBss
        | SectionKind::RLStack => Some(kind),
        _ => None,
    }
}

pub(super) fn is_common_or_xref_section(section: u8) -> bool {
    matches!(
        SectionKind::from_u8(section),
        SectionKind::RLCommon | SectionKind::RCommon | SectionKind::Common | SectionKind::Xref
    )
}

pub(super) fn is_xref_section(section: u8) -> bool {
    matches!(SectionKind::from_u8(section), SectionKind::Xref)
}

pub(super) fn is_abs_section(section: u8) -> bool {
    matches!(SectionKind::from_u8(section), SectionKind::Abs)
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

fn u32_to_usize_saturating(value: u32) -> usize {
    usize::try_from(value).unwrap_or(usize::MAX)
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

#[derive(Default)]
struct CtorDtorUsage {
    seen_mask: u8,
    ctor_count: usize,
    dtor_count: usize,
    ctor_header_size: Option<u32>,
    dtor_header_size: Option<u32>,
}

impl CtorDtorUsage {
    const SEEN_CTOR: u8 = 1 << 0;
    const SEEN_DTOR: u8 = 1 << 1;
    const SEEN_DOCTOR: u8 = 1 << 2;
    const SEEN_DODTOR: u8 = 1 << 3;

    fn has_seen(&self, flag: u8) -> bool {
        self.seen_mask & flag != 0
    }

    fn set_seen(&mut self, flag: u8) {
        self.seen_mask |= flag;
    }

    fn apply_opaque_code(&mut self, code: u16) {
        match code {
            opcode::OP_CTOR_ENTRY => {
                self.set_seen(Self::SEEN_CTOR);
                self.ctor_count += 1;
            }
            opcode::OP_DTOR_ENTRY => {
                self.set_seen(Self::SEEN_DTOR);
                self.dtor_count += 1;
            }
            opcode::OP_DOCTOR => self.set_seen(Self::SEEN_DOCTOR),
            opcode::OP_DODTOR => self.set_seen(Self::SEEN_DODTOR),
            _ => {}
        }
    }

    fn set_header_size(&mut self, section: u8, size: u32) {
        match section {
            0x0c => self.ctor_header_size = Some(size),
            0x0d => self.dtor_header_size = Some(size),
            _ => {}
        }
    }

    fn push_mode_diagnostics(&self, diagnostics: &mut Vec<String>, obj_name: &str, g2lk_mode: bool) {
        if g2lk_mode {
            if self.has_seen(Self::SEEN_CTOR) && !self.has_seen(Self::SEEN_DOCTOR) {
                diagnostics.push(format!(".doctor なしで .ctor が使われています in {obj_name}"));
            }
            if self.has_seen(Self::SEEN_DTOR) && !self.has_seen(Self::SEEN_DODTOR) {
                diagnostics.push(format!(".dodtor なしで .dtor が使われています in {obj_name}"));
            }
            return;
        }
        if self.seen_mask != 0 {
            diagnostics.push(format!(
                "(do)ctor/dtor には -1 オプションの指定が必要です。 in {obj_name}"
            ));
        }
    }

    fn push_header_size_diagnostics(&self, diagnostics: &mut Vec<String>, obj_name: &str) {
        if let Some(size) = self.ctor_header_size {
            let expected = usize_to_u32_saturating(self.ctor_count).saturating_mul(4);
            if size != expected {
                diagnostics.push(format!(
                    "ctor header size mismatch in {obj_name}: header={size} expected={expected}"
                ));
            }
        }
        if let Some(size) = self.dtor_header_size {
            let expected = usize_to_u32_saturating(self.dtor_count).saturating_mul(4);
            if size != expected {
                diagnostics.push(format!(
                    "dtor header size mismatch in {obj_name}: header={size} expected={expected}"
                ));
            }
        }
    }
}

fn validate_unsupported_expression_commands(
    objects: &[ObjectFile],
    input_paths: &[String],
    summaries: &[ObjectSummary],
    g2lk_mode: bool,
) -> Result<()> {
    let global_symbols = collect_global_symbols(summaries);

    let mut diagnostics = Vec::<String>::new();
    for (obj_idx, (obj, summary)) in objects.iter().zip(summaries.iter()).enumerate() {
        diagnostics.extend(collect_object_expression_diagnostics(
            obj_idx,
            obj,
            summary,
            input_paths,
            &global_symbols,
            g2lk_mode,
        ));
    }
    if diagnostics.is_empty() {
        return Ok(());
    }
    bail!("{}", diagnostics.join("\n"));
}

fn collect_object_expression_diagnostics(
    obj_idx: usize,
    obj: &ObjectFile,
    summary: &ObjectSummary,
    input_paths: &[String],
    global_symbols: &HashMap<Vec<u8>, Symbol>,
    g2lk_mode: bool,
) -> Vec<String> {
    let obj_name = input_paths
        .get(obj_idx)
        .and_then(|p| std::path::Path::new(p).file_name())
        .and_then(|s| s.to_str())
        .map_or_else(|| format!("obj{obj_idx}.o"), std::borrow::ToOwned::to_owned);
    let mut diagnostics = Vec::<String>::new();
    let mut usage = CtorDtorUsage::default();
    walk_commands(obj, |cmd, current, local, calc_stack| match cmd {
        Command::Header { section, size, .. } => usage.set_header_size(*section, *size),
        Command::Opaque { code, .. } => {
            usage.apply_opaque_code(*code);
            let messages =
                expr::classify_expression_errors(*code, cmd, summary, global_symbols, current, calc_stack);
            for msg in messages {
                push_expr_diagnostic(&mut diagnostics, msg, &obj_name, local, current);
            }
        }
        _ => {}
    });
    usage.push_mode_diagnostics(&mut diagnostics, &obj_name, g2lk_mode);
    usage.push_header_size_diagnostics(&mut diagnostics, &obj_name);
    diagnostics
}

fn collect_global_symbols(summaries: &[ObjectSummary]) -> HashMap<Vec<u8>, Symbol> {
    let mut global_symbols = HashMap::<Vec<u8>, Symbol>::new();
    for summary in summaries {
        for sym in &summary.symbols {
            global_symbols.insert(sym.name.clone(), sym.clone());
        }
    }
    global_symbols
}

fn push_expr_diagnostic(
    diagnostics: &mut Vec<String>,
    message: &str,
    obj_name: &str,
    local: u32,
    current: SectionKind,
) {
    diagnostics.push(format!(
        "{message} in {obj_name}\n at {local:08x} ({})",
        section_name(current)
    ));
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
