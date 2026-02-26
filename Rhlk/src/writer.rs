use std::collections::{BTreeMap, HashMap, HashSet};

use anyhow::{bail, Context, Result};

use crate::format::obj::{Command, ObjectFile};
use crate::layout::LayoutPlan;
use crate::resolver::{ObjectSummary, SectionKind, Symbol};

pub fn write_output(
    output_path: &str,
    r_format: bool,
    r_no_check: bool,
    omit_bss: bool,
    make_mcs: bool,
    objects: &[ObjectFile],
    summaries: &[ObjectSummary],
    layout: &LayoutPlan,
) -> Result<()> {
    if r_format && !r_no_check {
        validate_r_convertibility(objects, summaries, layout)?;
    }

    let mut payload = if r_format {
        build_r_payload(objects, summaries, layout, omit_bss)?
    } else {
        build_x_image(objects, summaries, layout)?
    };

    if make_mcs {
        let bss_extra = if omit_bss {
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
        patch_mcs_size(&mut payload, bss_extra)?;
    }
    std::fs::write(output_path, payload).with_context(|| format!("failed to write {output_path}"))?;
    Ok(())
}

fn build_x_image(
    objects: &[ObjectFile],
    summaries: &[ObjectSummary],
    layout: &LayoutPlan,
) -> Result<Vec<u8>> {
    if objects.len() != summaries.len() || objects.len() != layout.placements.len() {
        bail!("internal mismatch: objects/summaries/layout length differs");
    }

    let linked = link_initialized_sections(
        objects,
        summaries,
        layout,
        &[SectionKind::Text, SectionKind::Data],
    )?;

    let text = linked.get(&SectionKind::Text).cloned().unwrap_or_default();
    let data = linked.get(&SectionKind::Data).cloned().unwrap_or_default();

    let text_size = text.len() as u32;
    let data_size = data.len() as u32;
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

    let symbol_data = build_symbol_table(
        summaries,
        layout,
        text_size,
        data_size,
        bss_only,
        common_only,
    );
    let symbol_size = symbol_data.len() as u32;
    let reloc_table = build_relocation_table(objects, layout, text_size);
    let reloc_size = reloc_table.len() as u32;
    let (scd_line, scd_info, scd_name) = build_scd_passthrough(objects, summaries, layout)?;
    let scd_line_size = scd_line.len() as u32;
    let scd_info_size = scd_info.len() as u32;
    let scd_name_size = scd_name.len() as u32;

    let exec = resolve_exec_address(summaries, text_size, data_size, bss_size).unwrap_or(0);
    let header = build_x_header(
        text_size,
        data_size,
        bss_size,
        reloc_size,
        symbol_size,
        scd_line_size,
        scd_info_size,
        scd_name_size,
        exec,
    );

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
        // NOTE: HLK make_scdinfo performs richer fixups.
        // Here we apply a reduced fixup:
        // - location!=0 : + text_pos
        // - location==0 : + sinfo_pos(entries)
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
    if input.len() % 6 != 0 {
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
        let add = match sect as u8 {
            0x01 => placement.get(&SectionKind::Text).copied().unwrap_or(0),
            0x02 => placement.get(&SectionKind::Data).copied().unwrap_or(0),
            0x03 => placement.get(&SectionKind::Bss).copied().unwrap_or(0),
            0x05 => placement.get(&SectionKind::RData).copied().unwrap_or(0),
            0x06 => placement.get(&SectionKind::RBss).copied().unwrap_or(0),
            0x08 => placement.get(&SectionKind::RLData).copied().unwrap_or(0),
            0x09 => placement.get(&SectionKind::RLBss).copied().unwrap_or(0),
            _ => 0,
        };
        if add != 0 {
            let mut val = u32::from_be_bytes([out[p + 8], out[p + 9], out[p + 10], out[p + 11]]);
            val = val.saturating_add(add);
            out[p + 8..p + 12].copy_from_slice(&val.to_be_bytes());
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
                    let adj = off.wrapping_add(delta as i32 as u32);
                    out[q + 4..q + 8].copy_from_slice(&adj.to_be_bytes());
                }
            }
        }
        q += 18;
    }
    Ok(out)
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

fn einfo_section_delta(
    sect: u16,
    placement: &BTreeMap<SectionKind, u32>,
    layout: &LayoutPlan,
) -> Option<i64> {
    let text_total = layout
        .total_size_by_section
        .get(&SectionKind::Text)
        .copied()
        .unwrap_or(0) as i64;
    let data_total = layout
        .total_size_by_section
        .get(&SectionKind::Data)
        .copied()
        .unwrap_or(0) as i64;
    let bss_total = layout
        .total_size_by_section
        .get(&SectionKind::Bss)
        .copied()
        .unwrap_or(0) as i64;
    let common_total = layout
        .total_size_by_section
        .get(&SectionKind::Common)
        .copied()
        .unwrap_or(0) as i64;
    let stack_total = layout
        .total_size_by_section
        .get(&SectionKind::Stack)
        .copied()
        .unwrap_or(0) as i64;
    let obj_size = text_total + data_total + bss_total + common_total + stack_total;

    let text_pos = placement.get(&SectionKind::Text).copied().unwrap_or(0) as i64;
    let data_pos = text_total + placement.get(&SectionKind::Data).copied().unwrap_or(0) as i64;
    let bss_pos = text_total + data_total + placement.get(&SectionKind::Bss).copied().unwrap_or(0) as i64;
    let rdata_pos = placement.get(&SectionKind::RData).copied().unwrap_or(0) as i64;
    let rbss_pos = placement.get(&SectionKind::RBss).copied().unwrap_or(0) as i64;
    let rldata_pos = placement.get(&SectionKind::RLData).copied().unwrap_or(0) as i64;
    let rlbss_pos = placement.get(&SectionKind::RLBss).copied().unwrap_or(0) as i64;

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
    layout: &LayoutPlan,
    total_text_size: u32,
) -> Vec<u8> {
    let mut offsets = Vec::<u32>::new();
    for (idx, obj) in objects.iter().enumerate() {
        collect_object_relocations(obj, &layout.placements[idx].by_section, total_text_size, &mut offsets);
    }
    offsets.sort_unstable();
    offsets.dedup();
    encode_relocation_offsets(&offsets)
}

fn validate_r_convertibility(
    objects: &[ObjectFile],
    summaries: &[ObjectSummary],
    layout: &LayoutPlan,
) -> Result<()> {
    let text_size = layout
        .total_size_by_section
        .get(&SectionKind::Text)
        .copied()
        .unwrap_or(0);
    let reloc = build_relocation_table(objects, layout, text_size);
    if !reloc.is_empty() {
        bail!("relocation table is used; use --rn to force .r output");
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
    let exec = resolve_exec_address(summaries, text_size, data_size, bss_size).unwrap_or(0);
    if exec != 0 {
        bail!("exec address is not file head; use --rn to force .r output");
    }
    Ok(())
}

fn collect_object_relocations(
    object: &ObjectFile,
    placement: &BTreeMap<SectionKind, u32>,
    total_text_size: u32,
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
                bump_cursor(&mut cursor_by_section, current, bytes.len() as u32);
            }
            Command::DefineSpace { size } => {
                bump_cursor(&mut cursor_by_section, current, *size);
            }
            Command::Opaque { code, .. } => {
                let write_size = opaque_write_size(*code);
                if write_size == 0 {
                    continue;
                }

                if matches!(current, SectionKind::Text | SectionKind::Data)
                    && should_relocate(*code)
                {
                    let local = cursor_by_section.get(&current).copied().unwrap_or(0);
                    let section_base = match current {
                        SectionKind::Text => 0,
                        SectionKind::Data => total_text_size,
                        _ => 0,
                    };
                    let placed = placement.get(&current).copied().unwrap_or(0);
                    out.push(section_base.saturating_add(placed).saturating_add(local));
                }

                bump_cursor(&mut cursor_by_section, current, write_size as u32);
            }
            _ => {}
        }
    }
}

fn opaque_write_size(code: u16) -> u8 {
    let hi = (code >> 8) as u8;
    match hi {
        0x40 | 0x43 | 0x50 | 0x53 | 0x57 | 0x6b | 0x90 | 0x93 => 1,
        0x41 | 0x45 | 0x51 | 0x55 | 0x65 | 0x69 | 0x91 | 0x99 => 2,
        0x42 | 0x46 | 0x52 | 0x56 | 0x6a | 0x92 | 0x96 | 0x9a => 4,
        _ => 0,
    }
}

fn needs_relocation(code: u16) -> bool {
    matches!((code >> 8) as u8, 0x42 | 0x46 | 0x52 | 0x56 | 0x6a | 0x9a)
}

fn should_relocate(code: u16) -> bool {
    if !needs_relocation(code) {
        return false;
    }
    let hi = (code >> 8) as u8;
    if hi == 0x9a {
        return true;
    }
    is_reloc_section((code & 0x00ff) as u8)
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
            out.extend_from_slice(&(off as u16).to_be_bytes());
        } else {
            out.extend_from_slice(&1u16.to_be_bytes());
            out.extend_from_slice(&off.to_be_bytes());
        }
    }
    out
}

fn patch_mcs_size(payload: &mut [u8], bss_size: u32) -> Result<()> {
    if payload.len() < 14 {
        bail!("not MACS format: payload too small");
    }
    if &payload[0..4] != b"MACS" || &payload[4..8] != b"DATA" {
        bail!("not MACS format: missing MACS/DATA signature");
    }
    let total_size = (payload.len() as u32).saturating_add(bss_size);
    put_u32_be(payload, 10, total_size);
    Ok(())
}

fn build_x_header(
    text_size: u32,
    data_size: u32,
    bss_size: u32,
    reloc_size: u32,
    symbol_size: u32,
    scd_line_size: u32,
    scd_info_size: u32,
    scd_name_size: u32,
    exec: u32,
) -> Vec<u8> {
    let mut h = vec![0u8; 64];
    // 'HU'
    h[0] = b'H';
    h[1] = b'U';
    // load mode = 0, base = 0
    put_u32_be(&mut h, 8, exec);
    put_u32_be(&mut h, 12, text_size);
    put_u32_be(&mut h, 16, data_size);
    put_u32_be(&mut h, 20, bss_size);
    put_u32_be(&mut h, 24, reloc_size);
    put_u32_be(&mut h, 28, symbol_size);
    put_u32_be(&mut h, 32, scd_line_size);
    put_u32_be(&mut h, 36, scd_info_size);
    put_u32_be(&mut h, 40, scd_name_size);
    h
}

fn resolve_exec_address(
    summaries: &[ObjectSummary],
    text_size: u32,
    data_size: u32,
    _bss_size: u32,
) -> Option<u32> {
    let start = summaries.iter().filter_map(|s| s.start_address).last()?;
    let (sect, addr) = start;
    let base = match sect as u8 {
        0x01 => 0,
        0x02 => text_size,
        0x03 => text_size.saturating_add(data_size),
        _ => 0,
    };
    Some(base.saturating_add(addr))
}

fn put_u32_be(buf: &mut [u8], at: usize, v: u32) {
    let b = v.to_be_bytes();
    buf[at..at + 4].copy_from_slice(&b);
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use crate::format::obj::{Command, ObjectFile};
    use crate::layout::plan_layout;
    use crate::resolver::{ObjectSummary, SectionKind, Symbol};
    use crate::writer::{build_r_payload, build_x_image, validate_r_convertibility};

    #[test]
    fn builds_r_payload_from_layouted_sections() {
        let obj0 = ObjectFile {
            commands: vec![
                Command::Header {
                    section: 0x01,
                    size: 2,
                    name: b"text".to_vec(),
                },
                Command::ChangeSection { section: 0x01 },
                Command::RawData(vec![0xaa, 0xbb]),
                Command::End,
            ],
            scd_tail: Vec::new(),
        };
        let obj1 = ObjectFile {
            commands: vec![
                Command::Header {
                    section: 0x01,
                    size: 2,
                    name: b"text".to_vec(),
                },
                Command::Header {
                    section: 0x02,
                    size: 2,
                    name: b"data".to_vec(),
                },
                Command::DefineSymbol {
                    section: 0x01,
                    value: 2,
                    name: b"*align".to_vec(),
                },
                Command::ChangeSection { section: 0x01 },
                Command::RawData(vec![0xcc, 0xdd]),
                Command::ChangeSection { section: 0x02 },
                Command::RawData(vec![0x11, 0x22]),
                Command::End,
            ],
            scd_tail: Vec::new(),
        };

        let sum0 = mk_summary(2, 2, 0);
        let sum1 = mk_summary(4, 2, 2);
        let layout = plan_layout(&[sum0.clone(), sum1.clone()]);

        let payload = build_r_payload(&[obj0, obj1], &[sum0, sum1], &layout, true).expect("payload");
        // text: [aa bb 00 00 cc dd], data: [11 22]
        assert_eq!(payload, vec![0xaa, 0xbb, 0x00, 0x00, 0xcc, 0xdd, 0x11, 0x22]);
    }

    #[test]
    fn r_payload_includes_bss_when_not_omitted() {
        let obj = ObjectFile {
            commands: vec![
                Command::Header {
                    section: 0x01,
                    size: 2,
                    name: b"text".to_vec(),
                },
                Command::Header {
                    section: 0x03,
                    size: 4,
                    name: b"bss".to_vec(),
                },
                Command::ChangeSection { section: 0x01 },
                Command::RawData(vec![0xde, 0xad]),
                Command::End,
            ],
            scd_tail: Vec::new(),
        };
        let mut sum = mk_summary(2, 2, 0);
        sum.declared_section_sizes.insert(SectionKind::Bss, 4);
        let layout = plan_layout(&[sum.clone()]);
        let with_bss = build_r_payload(&[obj.clone()], &[sum.clone()], &layout, false).expect("r with bss");
        let without_bss = build_r_payload(&[obj], &[sum], &layout, true).expect("r without bss");
        assert_eq!(with_bss.len(), without_bss.len() + 4);
        assert_eq!(&with_bss[0..2], &[0xde, 0xad]);
        assert_eq!(&with_bss[2..], &[0, 0, 0, 0]);
    }

    #[test]
    fn builds_minimal_x_image() {
        let obj = ObjectFile {
            commands: vec![
                Command::Header {
                    section: 0x01,
                    size: 2,
                    name: b"text".to_vec(),
                },
                Command::Header {
                    section: 0x02,
                    size: 2,
                    name: b"data".to_vec(),
                },
                Command::ChangeSection { section: 0x01 },
                Command::RawData(vec![0x01, 0x02]),
                Command::ChangeSection { section: 0x02 },
                Command::RawData(vec![0x11, 0x22]),
                Command::DefineSymbol {
                    section: 0x01,
                    value: 1,
                    name: b"_label".to_vec(),
                },
                Command::StartAddress {
                    section: 0x02,
                    address: 1,
                },
                Command::End,
            ],
            // [linfo=6][sinfo+einfo=4][ninfo=2] + payloads
            scd_tail: vec![
                0, 0, 0, 6, 0, 0, 0, 4, 0, 0, 0, 2, // sizes
                1, 2, 3, 4, 5, 6, // linfo
                0xaa, 0xbb, 0xcc, 0xdd, // sinfo+einfo
                0x31, 0x00, // ninfo
            ],
        };
        let mut sum = mk_summary(2, 2, 2);
        sum.start_address = Some((0x02, 1));
        sum.symbols.push(Symbol {
            name: b"_label".to_vec(),
            section: SectionKind::Text,
            value: 1,
        });

        let layout = plan_layout(&[sum.clone()]);
        let image = build_x_image(&[obj], &[sum], &layout).expect("x image");

        assert_eq!(&image[0..2], b"HU");
        assert_eq!(&image[8..12], &(3u32.to_be_bytes())); // text(2) + addr(1)
        assert_eq!(&image[12..16], &(2u32.to_be_bytes()));
        assert_eq!(&image[16..20], &(2u32.to_be_bytes()));
        let symbol_size = u32::from_be_bytes([image[28], image[29], image[30], image[31]]) as usize;
        assert!(symbol_size >= 8);
        let reloc_size = u32::from_be_bytes([image[24], image[25], image[26], image[27]]) as usize;
        assert_eq!(reloc_size, 0);
        assert_eq!(&image[32..36], &(6u32.to_be_bytes()));
        assert_eq!(&image[36..40], &(4u32.to_be_bytes()));
        assert_eq!(&image[40..44], &(2u32.to_be_bytes()));
        assert_eq!(&image[64..68], &[0x01, 0x02, 0x11, 0x22]);
    }

    #[test]
    fn writes_relocation_table_for_long_section_refs() {
        let obj = ObjectFile {
            commands: vec![
                Command::Header {
                    section: 0x01,
                    size: 14,
                    name: b"text".to_vec(),
                },
                Command::ChangeSection { section: 0x01 },
                Command::Opaque {
                    code: 0x4201, // dc.l text:adr
                    payload: vec![0, 0, 0, 0],
                },
                Command::RawData(vec![0xaa, 0xbb]),
                Command::Opaque {
                    code: 0x6a01, // dc.l label-sect:adr
                    payload: vec![0, 0, 0, 2, 0, 1],
                },
                Command::Opaque {
                    code: 0x9a00, // dc.l (sp):sp++
                    payload: vec![],
                },
                Command::End,
            ],
            scd_tail: Vec::new(),
        };
        let sum = mk_summary(2, 14, 0);
        let layout = plan_layout(&[sum.clone()]);
        let image = build_x_image(&[obj], &[sum], &layout).expect("x image");
        let reloc_size = u32::from_be_bytes([image[24], image[25], image[26], image[27]]) as usize;
        assert_eq!(reloc_size, 6);
        // relocation table begins after header + text + data
        let reloc_pos = 64 + 14;
        assert_eq!(
            &image[reloc_pos..reloc_pos + 6],
            &[0x00, 0x00, 0x00, 0x06, 0x00, 0x0a]
        );
    }

    #[test]
    fn rejects_r_without_rn_if_relocation_exists() {
        let obj = ObjectFile {
            commands: vec![
                Command::Header {
                    section: 0x01,
                    size: 4,
                    name: b"text".to_vec(),
                },
                Command::ChangeSection { section: 0x01 },
                Command::Opaque {
                    code: 0x4201,
                    payload: vec![0, 0, 0, 0],
                },
                Command::End,
            ],
            scd_tail: Vec::new(),
        };
        let sum = mk_summary(2, 4, 0);
        let layout = plan_layout(&[sum.clone()]);
        let err =
            validate_r_convertibility(&[obj], &[sum], &layout).expect_err("should reject conversion");
        assert!(err.to_string().contains("relocation table is used"));
    }

    #[test]
    fn rejects_r_without_rn_if_exec_not_head() {
        let obj = ObjectFile {
            commands: vec![
                Command::Header {
                    section: 0x01,
                    size: 2,
                    name: b"text".to_vec(),
                },
                Command::StartAddress {
                    section: 0x02,
                    address: 1,
                },
                Command::End,
            ],
            scd_tail: Vec::new(),
        };
        let mut sum = mk_summary(2, 2, 0);
        sum.start_address = Some((0x02, 1));
        let layout = plan_layout(&[sum.clone()]);
        let err =
            validate_r_convertibility(&[obj], &[sum], &layout).expect_err("should reject conversion");
        assert!(err.to_string().contains("exec address is not file head"));
    }

    #[test]
    fn patches_mcs_total_size() {
        let mut payload = b"MACSDATA\x01\x00\x00\x00\x00\x00MORE".to_vec();
        super::patch_mcs_size(&mut payload, 6).expect("must patch");
        // len 18 + 6 = 24 => 0x00000018 at offset 10
        assert_eq!(&payload[10..14], &[0x00, 0x00, 0x00, 0x18]);
    }

    #[test]
    fn rejects_non_mcs_payload() {
        let mut payload = b"XXXXDATA\x01\x00\x00\x00\x00\x00".to_vec();
        let err = super::patch_mcs_size(&mut payload, 0).expect_err("must fail");
        assert!(err.to_string().contains("not MACS format"));
    }

    #[test]
    fn rebases_scd_line_locations_by_text_pos() {
        let obj0 = ObjectFile {
            commands: vec![
                Command::Header {
                    section: 0x01,
                    size: 2,
                    name: b"text".to_vec(),
                },
                Command::RawData(vec![0xaa, 0xbb]),
                Command::End,
            ],
            scd_tail: Vec::new(),
        };
        let obj1 = ObjectFile {
            commands: vec![
                Command::Header {
                    section: 0x01,
                    size: 2,
                    name: b"text".to_vec(),
                },
                Command::RawData(vec![0xcc, 0xdd]),
                Command::End,
            ],
            // linfo_size=6, sinfo+einfo=0, ninfo=0; linfo=(loc=2, line=7)
            scd_tail: vec![
                0, 0, 0, 6, 0, 0, 0, 0, 0, 0, 0, 0, //
                0, 0, 0, 2, 0, 7,
            ],
        };

        let sum0 = mk_summary(2, 2, 0);
        let sum1 = mk_summary(4, 2, 0); // second text starts at 4
        let layout = plan_layout(&[sum0.clone(), sum1.clone()]);
        let image = build_x_image(&[obj0, obj1], &[sum0, sum1], &layout).expect("x image");
        // header[32..36] = line size
        assert_eq!(&image[32..36], &(6u32.to_be_bytes()));
        // After header(64) + text(6) + data(0) + reloc(0) + symbol(0), line starts.
        let line_pos = 64 + 6;
        assert_eq!(&image[line_pos..line_pos + 4], &[0, 0, 0, 6]); // 2 + text_pos(4)
        assert_eq!(&image[line_pos + 4..line_pos + 6], &[0, 7]);
    }

    #[test]
    fn rebases_zero_line_location_by_sinfo_pos() {
        let obj0 = ObjectFile {
            commands: vec![Command::End],
            // linfo=0, sinfo+einfo=18, ninfo=0.
            // sinfo_count is read from offset 20 (= val.l of first 18-byte entry here).
            scd_tail: vec![
                0, 0, 0, 0, 0, 0, 0, 18, 0, 0, 0, 0, //
                b'.', b'f', b'i', b'l', 0, 0, 0, 0, //
                0, 0, 0, 1, //
                0, 1, //
                0, 0, //
                0, 0,
            ],
        };
        let obj1 = ObjectFile {
            commands: vec![Command::End],
            // linfo record with location 0, line 9
            scd_tail: vec![
                0, 0, 0, 6, 0, 0, 0, 0, 0, 0, 0, 0, //
                0, 0, 0, 0, 0, 9,
            ],
        };

        let sum0 = mk_summary(2, 0, 0);
        let sum1 = mk_summary(2, 0, 0);
        let layout = plan_layout(&[sum0.clone(), sum1.clone()]);
        let image = build_x_image(&[obj0, obj1], &[sum0, sum1], &layout).expect("x image");
        let line_size = u32::from_be_bytes([image[32], image[33], image[34], image[35]]) as usize;
        assert_eq!(line_size, 6);
        // no text/data/reloc/symbol, line starts right after header
        let line_pos = 64;
        assert_eq!(&image[line_pos..line_pos + 4], &[0, 0, 0, 1]); // sinfo_pos(1)
        assert_eq!(&image[line_pos + 4..line_pos + 6], &[0, 9]);
    }

    #[test]
    fn rebases_scd_info_value_by_section_position() {
        let obj0 = ObjectFile {
            commands: vec![
                Command::Header {
                    section: 0x01,
                    size: 2,
                    name: b"text".to_vec(),
                },
                Command::RawData(vec![1, 2]),
                Command::End,
            ],
            scd_tail: Vec::new(),
        };
        let obj1 = ObjectFile {
            commands: vec![
                Command::Header {
                    section: 0x01,
                    size: 2,
                    name: b"text".to_vec(),
                },
                Command::RawData(vec![3, 4]),
                Command::End,
            ],
            // linfo=0, sinfo+einfo=18, ninfo=0, sinfo_count=1
            // entry: name "_a\0\0\0\0\0", val=1, sect=1(text), type=0, scl=0,0
            scd_tail: vec![
                0, 0, 0, 0, 0, 0, 0, 18, 0, 0, 0, 0, //
                b'_', b'a', 0, 0, 0, 0, 0, 0, //
                0, 0, 0, 1, //
                0, 1, //
                0, 0, //
                0, 0,
            ],
        };

        let sum0 = mk_summary(2, 2, 0);
        let sum1 = mk_summary(4, 2, 0); // text_pos = 4
        let layout = plan_layout(&[sum0.clone(), sum1.clone()]);
        let image = build_x_image(&[obj0, obj1], &[sum0, sum1], &layout).expect("x image");
        let text_size = u32::from_be_bytes([image[12], image[13], image[14], image[15]]) as usize;
        let data_size = u32::from_be_bytes([image[16], image[17], image[18], image[19]]) as usize;
        let reloc_size = u32::from_be_bytes([image[24], image[25], image[26], image[27]]) as usize;
        let sym_size = u32::from_be_bytes([image[28], image[29], image[30], image[31]]) as usize;
        let line_size = u32::from_be_bytes([image[32], image[33], image[34], image[35]]) as usize;
        let scd_info_pos = 64 + text_size + data_size + reloc_size + sym_size + line_size;
        // val.l should be 1 + text_pos(4) = 5
        assert_eq!(&image[scd_info_pos + 8..scd_info_pos + 12], &[0, 0, 0, 5]);
    }

    #[test]
    fn rebases_scd_einfo_sinfo_index_by_cumulative_sinfo_pos() {
        let obj0 = ObjectFile {
            commands: vec![Command::End],
            // linfo=0, sinfo+einfo=18, ninfo=0, sinfo_count=3
            scd_tail: vec![
                0, 0, 0, 0, 0, 0, 0, 18, 0, 0, 0, 0, //
                b'.', b'f', b'i', b'l', 0, 0, 0, 0, //
                0, 0, 0, 3, //
                0, 0, //
                0, 0, //
                0, 0,
            ],
        };
        let obj1 = ObjectFile {
            commands: vec![Command::End],
            // linfo=0, sinfo+einfo=36, ninfo=0, sinfo_count=1
            // entry0(sinfo): dummy
            // entry1(einfo): d6==0, ref_idx=2 -> expected 2 + previous_sinfo_pos(3) = 5
            scd_tail: vec![
                0, 0, 0, 0, 0, 0, 0, 36, 0, 0, 0, 0, //
                b'.', b't', b'e', b'x', 0, 0, 0, 0, //
                0, 0, 0, 1, //
                0, 0, //
                0, 0, //
                0, 0, //
                0, 0, 0, 0, // d6 == 0
                0, 0, 0, 2, // ref idx
                0, 0, 0, 0, //
                0, 0, 0, 0, //
                0, 0, //
            ],
        };

        let sum0 = mk_summary(2, 0, 0);
        let sum1 = mk_summary(2, 0, 0);
        let layout = plan_layout(&[sum0.clone(), sum1.clone()]);
        let image = build_x_image(&[obj0, obj1], &[sum0, sum1], &layout).expect("x image");
        let text_size = u32::from_be_bytes([image[12], image[13], image[14], image[15]]) as usize;
        let data_size = u32::from_be_bytes([image[16], image[17], image[18], image[19]]) as usize;
        let reloc_size = u32::from_be_bytes([image[24], image[25], image[26], image[27]]) as usize;
        let sym_size = u32::from_be_bytes([image[28], image[29], image[30], image[31]]) as usize;
        let line_size = u32::from_be_bytes([image[32], image[33], image[34], image[35]]) as usize;
        let scd_info_pos = 64 + text_size + data_size + reloc_size + sym_size + line_size;
        // obj0(18) + obj1 first entry(18) + offset 4 (second long)
        let einfo_ref_pos = scd_info_pos + 18 + 18 + 4;
        assert_eq!(&image[einfo_ref_pos..einfo_ref_pos + 4], &[0, 0, 0, 5]);
    }

    #[test]
    fn rebases_scd_einfo_offset_by_section_position() {
        let obj0 = ObjectFile {
            commands: vec![
                Command::Header {
                    section: 0x01,
                    size: 2,
                    name: b"text".to_vec(),
                },
                Command::RawData(vec![1, 2]),
                Command::End,
            ],
            scd_tail: Vec::new(),
        };
        let obj1 = ObjectFile {
            commands: vec![
                Command::Header {
                    section: 0x01,
                    size: 2,
                    name: b"text".to_vec(),
                },
                Command::RawData(vec![3, 4]),
                Command::End,
            ],
            // linfo=0, sinfo+einfo=36, ninfo=0, sinfo_count=1
            // second entry (einfo): d6!=0, off=2, sect=1(text)
            scd_tail: vec![
                0, 0, 0, 0, 0, 0, 0, 36, 0, 0, 0, 0, //
                b'_', b'a', 0, 0, 0, 0, 0, 0, //
                0, 0, 0, 1, //
                0, 1, //
                0, 0, //
                0, 0, //
                0, 0, 0, 1, // d6 != 0
                0, 0, 0, 2, // off
                0, 1, // sect = text
                0, 0, 0, 0, 0, 0, 0, 0, // rest(8 bytes) to fill 18-byte entry
            ],
        };

        let sum0 = mk_summary(2, 2, 0);
        let sum1 = mk_summary(4, 2, 0); // text_pos = 4
        let layout = plan_layout(&[sum0.clone(), sum1.clone()]);
        let image = build_x_image(&[obj0, obj1], &[sum0, sum1], &layout).expect("x image");
        let text_size = u32::from_be_bytes([image[12], image[13], image[14], image[15]]) as usize;
        let data_size = u32::from_be_bytes([image[16], image[17], image[18], image[19]]) as usize;
        let reloc_size = u32::from_be_bytes([image[24], image[25], image[26], image[27]]) as usize;
        let sym_size = u32::from_be_bytes([image[28], image[29], image[30], image[31]]) as usize;
        let line_size = u32::from_be_bytes([image[32], image[33], image[34], image[35]]) as usize;
        let scd_info_pos = 64 + text_size + data_size + reloc_size + sym_size + line_size;
        // obj1 second entry offset field at +18(first entry)+4(second entry off)
        let off_pos = scd_info_pos + 18 + 4;
        assert_eq!(&image[off_pos..off_pos + 4], &[0, 0, 0, 6]); // 2 + text_pos(4)
    }

    #[test]
    fn rebases_scd_einfo_bss_offset_with_obj_size_rule() {
        let obj = ObjectFile {
            commands: vec![
                Command::Header {
                    section: 0x01,
                    size: 2,
                    name: b"text".to_vec(),
                },
                Command::Header {
                    section: 0x02,
                    size: 2,
                    name: b"data".to_vec(),
                },
                Command::Header {
                    section: 0x03,
                    size: 4,
                    name: b"bss".to_vec(),
                },
                Command::End,
            ],
            // linfo=0, sinfo+einfo=36, ninfo=0, sinfo_count=1
            // second entry (einfo): d6!=0, off=10, sect=3(bss)
            scd_tail: vec![
                0, 0, 0, 0, 0, 0, 0, 36, 0, 0, 0, 0, //
                b'_', b'a', 0, 0, 0, 0, 0, 0, //
                0, 0, 0, 1, //
                0, 1, //
                0, 0, //
                0, 0, //
                0, 0, 0, 1, //
                0, 0, 0, 10, //
                0, 3, //
                0, 0, 0, 0, 0, 0, 0, 0,
            ],
        };
        let mut sum = mk_summary(2, 2, 2);
        sum.declared_section_sizes.insert(SectionKind::Bss, 4);
        let layout = plan_layout(&[sum.clone()]);
        let image = build_x_image(&[obj], &[sum], &layout).expect("x image");

        let text_size = u32::from_be_bytes([image[12], image[13], image[14], image[15]]) as usize;
        let data_size = u32::from_be_bytes([image[16], image[17], image[18], image[19]]) as usize;
        let reloc_size = u32::from_be_bytes([image[24], image[25], image[26], image[27]]) as usize;
        let sym_size = u32::from_be_bytes([image[28], image[29], image[30], image[31]]) as usize;
        let line_size = u32::from_be_bytes([image[32], image[33], image[34], image[35]]) as usize;
        let scd_info_pos = 64 + text_size + data_size + reloc_size + sym_size + line_size;
        let off_pos = scd_info_pos + 18 + 4;
        // bss_pos - obj_size = (text+data+0) - (text+data+bss) = -bss = -4
        // 10 + (-4) = 6
        assert_eq!(&image[off_pos..off_pos + 4], &[0, 0, 0, 6]);
    }

    #[test]
    fn rebases_remaining_scd_einfo_section_branches() {
        let obj0 = ObjectFile {
            commands: vec![
                Command::Header {
                    section: 0x01,
                    size: 2,
                    name: b"text".to_vec(),
                },
                Command::Header {
                    section: 0x02,
                    size: 2,
                    name: b"data".to_vec(),
                },
                Command::Header {
                    section: 0x05,
                    size: 2,
                    name: b"rdata".to_vec(),
                },
                Command::Header {
                    section: 0x06,
                    size: 2,
                    name: b"rbss".to_vec(),
                },
                Command::Header {
                    section: 0x08,
                    size: 2,
                    name: b"rldata".to_vec(),
                },
                Command::Header {
                    section: 0x09,
                    size: 2,
                    name: b"rlbss".to_vec(),
                },
                Command::End,
            ],
            scd_tail: Vec::new(),
        };

        let obj1 = ObjectFile {
            commands: vec![
                Command::Header {
                    section: 0x01,
                    size: 2,
                    name: b"text".to_vec(),
                },
                Command::Header {
                    section: 0x02,
                    size: 2,
                    name: b"data".to_vec(),
                },
                Command::Header {
                    section: 0x05,
                    size: 2,
                    name: b"rdata".to_vec(),
                },
                Command::Header {
                    section: 0x06,
                    size: 2,
                    name: b"rbss".to_vec(),
                },
                Command::Header {
                    section: 0x08,
                    size: 2,
                    name: b"rldata".to_vec(),
                },
                Command::Header {
                    section: 0x09,
                    size: 2,
                    name: b"rlbss".to_vec(),
                },
                Command::End,
            ],
            // linfo=0, sinfo+einfo=126, ninfo=0, sinfo_count=1
            // entry0(sinfo): dummy
            // entry1..6(einfo): d6!=0, off=3, sect=2/5/6/8/9/1
            scd_tail: vec![
                0, 0, 0, 0, 0, 0, 0, 126, 0, 0, 0, 0, //
                b'_', b'a', 0, 0, 0, 0, 0, 0, //
                0, 0, 0, 1, //
                0, 1, //
                0, 0, //
                0, 0, //
                0, 0, 0, 1, //
                0, 0, 0, 3, //
                0, 2, //
                0, 0, 0, 0, 0, 0, 0, 0, //
                0, 0, 0, 1, //
                0, 0, 0, 3, //
                0, 5, //
                0, 0, 0, 0, 0, 0, 0, 0, //
                0, 0, 0, 1, //
                0, 0, 0, 3, //
                0, 6, //
                0, 0, 0, 0, 0, 0, 0, 0, //
                0, 0, 0, 1, //
                0, 0, 0, 3, //
                0, 8, //
                0, 0, 0, 0, 0, 0, 0, 0, //
                0, 0, 0, 1, //
                0, 0, 0, 3, //
                0, 9, //
                0, 0, 0, 0, 0, 0, 0, 0, //
                0, 0, 0, 1, //
                0, 0, 0, 3, //
                0, 1, //
                0, 0, 0, 0, 0, 0, 0, 0,
            ],
        };

        let mut declared = BTreeMap::new();
        declared.insert(SectionKind::Text, 2);
        declared.insert(SectionKind::Data, 2);
        declared.insert(SectionKind::RData, 2);
        declared.insert(SectionKind::RBss, 2);
        declared.insert(SectionKind::RLData, 2);
        declared.insert(SectionKind::RLBss, 2);
        let sum0 = ObjectSummary {
            object_align: 2,
            declared_section_sizes: declared.clone(),
            observed_section_usage: BTreeMap::new(),
            symbols: Vec::<Symbol>::new(),
            xrefs: Vec::<Symbol>::new(),
            requests: Vec::new(),
            start_address: None,
        };
        let sum1 = ObjectSummary {
            object_align: 2,
            declared_section_sizes: declared,
            observed_section_usage: BTreeMap::new(),
            symbols: Vec::<Symbol>::new(),
            xrefs: Vec::<Symbol>::new(),
            requests: Vec::new(),
            start_address: None,
        };
        let layout = plan_layout(&[sum0.clone(), sum1.clone()]);
        let (_, info, _) =
            super::build_scd_passthrough(&[obj0, obj1], &[sum0, sum1], &layout).expect("scd");

        for i in 0..6usize {
            let off_pos = 18 + (i * 18) + 4;
            assert_eq!(&info[off_pos..off_pos + 4], &[0, 0, 0, 5]); // off(3) + delta(2)
        }
    }

    #[test]
    fn rejects_scd_einfo_stack_section_for_nonzero_d6() {
        let obj = ObjectFile {
            commands: vec![
                Command::Header {
                    section: 0x01,
                    size: 2,
                    name: b"text".to_vec(),
                },
                Command::End,
            ],
            // linfo=0, sinfo+einfo=36, ninfo=0, sinfo_count=1
            // second entry (einfo): d6!=0, off=1, sect=4(stack)
            scd_tail: vec![
                0, 0, 0, 0, 0, 0, 0, 36, 0, 0, 0, 0, //
                b'_', b'a', 0, 0, 0, 0, 0, 0, //
                0, 0, 0, 1, //
                0, 1, //
                0, 0, //
                0, 0, //
                0, 0, 0, 1, //
                0, 0, 0, 1, //
                0, 4, //
                0, 0, 0, 0, 0, 0, 0, 0,
            ],
        };
        let sum = mk_summary(2, 2, 0);
        let layout = plan_layout(&[sum.clone()]);
        let err = build_x_image(&[obj], &[sum], &layout).expect_err("must reject unsupported stack sect");
        assert!(err.to_string().contains("unsupported SCD einfo section"));
    }

    #[test]
    fn rejects_scd_einfo_common_reference_for_nonzero_d6() {
        let obj = ObjectFile {
            commands: vec![
                Command::Header {
                    section: 0x01,
                    size: 2,
                    name: b"text".to_vec(),
                },
                Command::End,
            ],
            // linfo=0, sinfo+einfo=36, ninfo=0, sinfo_count=1
            // second entry (einfo): d6!=0, off=1, sect=0x00fe(.comm-like)
            scd_tail: vec![
                0, 0, 0, 0, 0, 0, 0, 36, 0, 0, 0, 0, //
                b'_', b'a', 0, 0, 0, 0, 0, 0, //
                0, 0, 0, 1, //
                0, 1, //
                0, 0, //
                0, 0, //
                0, 0, 0, 1, //
                0, 0, 0, 1, //
                0, 0xfe, //
                0, 0, 0, 0, 0, 0, 0, 0,
            ],
        };
        let sum = mk_summary(2, 2, 0);
        let layout = plan_layout(&[sum.clone()]);
        let err = build_x_image(&[obj], &[sum], &layout)
            .expect_err("must reject unsupported common reference");
        assert!(err.to_string().contains("SCD einfo common-reference"));
    }

    #[test]
    fn resolves_scd_einfo_common_reference_for_nonzero_d6() {
        let obj = ObjectFile {
            commands: vec![
                Command::Header {
                    section: 0x01,
                    size: 2,
                    name: b"text".to_vec(),
                },
                Command::End,
            ],
            // linfo=0, sinfo+einfo=36, ninfo=0, sinfo_count=1
            // second entry (einfo): d6!=0, name=\"_cmn\", sect=0x00fe
            scd_tail: vec![
                0, 0, 0, 0, 0, 0, 0, 36, 0, 0, 0, 0, //
                b'_', b'a', 0, 0, 0, 0, 0, 0, //
                0, 0, 0, 1, //
                0, 1, //
                0, 0, //
                0, 0, //
                b'_', b'c', b'm', b'n', 0, 0, 0, 0, //
                0, 0xfe, //
                0, 0, 0, 0, 0, 0, 0, 0, //
                0, 0, 0, 0,
            ],
        };
        let mut sum = mk_summary(2, 2, 0);
        sum.symbols.push(Symbol {
            name: b"_cmn".to_vec(),
            section: SectionKind::Common,
            value: 8,
        });
        let layout = plan_layout(&[sum.clone()]);
        let image = build_x_image(&[obj], &[sum], &layout).expect("x image");

        let text_size = u32::from_be_bytes([image[12], image[13], image[14], image[15]]) as usize;
        let data_size = u32::from_be_bytes([image[16], image[17], image[18], image[19]]) as usize;
        let reloc_size = u32::from_be_bytes([image[24], image[25], image[26], image[27]]) as usize;
        let sym_size = u32::from_be_bytes([image[28], image[29], image[30], image[31]]) as usize;
        let line_size = u32::from_be_bytes([image[32], image[33], image[34], image[35]]) as usize;
        let scd_info_pos = 64 + text_size + data_size + reloc_size + sym_size + line_size;
        let off_pos = scd_info_pos + 18 + 4;
        let sect_pos = scd_info_pos + 18 + 8;
        // Common symbol first allocation offset is 0.
        assert_eq!(&image[off_pos..off_pos + 4], &[0, 0, 0, 0]);
        assert_eq!(&image[sect_pos..sect_pos + 2], &[0, 3]);
    }

    fn mk_summary(align: u32, text: u32, data: u32) -> ObjectSummary {
        let mut declared = BTreeMap::new();
        if text > 0 {
            declared.insert(SectionKind::Text, text);
        }
        if data > 0 {
            declared.insert(SectionKind::Data, data);
        }

        ObjectSummary {
            object_align: align,
            declared_section_sizes: declared,
            observed_section_usage: BTreeMap::new(),
            symbols: Vec::<Symbol>::new(),
            xrefs: Vec::<Symbol>::new(),
            requests: Vec::new(),
            start_address: None,
        }
    }
}
