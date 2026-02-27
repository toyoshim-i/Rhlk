use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;

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
    cut_symbols: bool,
    base_address: u32,
    load_mode: u8,
    section_info: bool,
    objects: &[ObjectFile],
    input_paths: &[String],
    summaries: &[ObjectSummary],
    layout: &LayoutPlan,
) -> Result<()> {
    validate_link_inputs(objects, input_paths, summaries)?;

    if r_format && !r_no_check {
        validate_r_convertibility(objects, summaries, layout, output_path)?;
    }

    let mut payload = if r_format {
        build_r_payload(objects, summaries, layout, omit_bss)?
    } else {
        build_x_image_with_options(objects, summaries, layout, !cut_symbols).map_err(|err| {
            let text = err.to_string();
            if text.contains("relocation target address is odd") {
                anyhow::anyhow!(
                    "再配置対象が奇数アドレスにあります: {}",
                    to_human68k_path(output_path)
                )
            } else {
                err
            }
        })?
    };

    if !r_format && (base_address != 0 || load_mode != 0) {
        apply_x_header_options(&mut payload, base_address, load_mode)?;
    }
    if section_info {
        patch_section_size_info(&mut payload, r_format, summaries, layout)?;
    }

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
        patch_mcs_size(&mut payload, bss_extra).map_err(|_| {
            anyhow::anyhow!(
                "MACS形式ファイルではありません: {}",
                to_human68k_path(output_path)
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

pub fn write_map(
    exec_output_path: &str,
    output_path: &str,
    summaries: &[ObjectSummary],
    layout: &LayoutPlan,
    input_paths: &[String],
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
    let text = build_map_text(
        exec_output_path,
        summaries,
        layout,
        text_size,
        data_size,
        bss_only,
        common_only,
        input_paths,
    );
    let text = text.replace('\n', "\r\n");
    std::fs::write(output_path, text).with_context(|| format!("failed to write {output_path}"))?;
    Ok(())
}

fn build_map_text(
    exec_output_path: &str,
    summaries: &[ObjectSummary],
    layout: &LayoutPlan,
    text_size: u32,
    data_size: u32,
    bss_only: u32,
    common_only: u32,
    input_paths: &[String],
) -> String {
    let bss_size = bss_only
        .saturating_add(common_only)
        .saturating_add(section_total(layout, SectionKind::Stack));
    let exec = resolve_exec_address(summaries, text_size, data_size, bss_size)
        .ok()
        .flatten()
        .unwrap_or(0);
    let mut out = String::new();
    out.push_str("==========================================================\n");
    out.push_str(&to_human68k_path(exec_output_path));
    out.push('\n');
    out.push_str("==========================================================\n");
    out.push_str(&format_exec_line(exec));
    let text_sz = section_total(layout, SectionKind::Text);
    let data_sz = section_total(layout, SectionKind::Data);
    let bss_sz = section_total(layout, SectionKind::Bss);
    let common_sz = section_total(layout, SectionKind::Common);
    let stack_sz = section_total(layout, SectionKind::Stack);
    let mut cur = 0u32;
    out.push_str(&format_section_line("text", cur, text_sz));
    cur = cur.saturating_add(text_sz);
    out.push_str(&format_section_line("data", cur, data_sz));
    cur = cur.saturating_add(data_sz);
    out.push_str(&format_section_line("bss", cur, bss_sz));
    cur = cur.saturating_add(bss_sz);
    out.push_str(&format_section_line("common", cur, common_sz));
    cur = cur.saturating_add(common_sz);
    out.push_str(&format_section_line("stack", cur, stack_sz));

    let mut rcur = 0u32;
    for (name, kind) in [
        ("rdata", SectionKind::RData),
        ("rbss", SectionKind::RBss),
        ("rcommon", SectionKind::RCommon),
        ("rstack", SectionKind::RStack),
        ("rldata", SectionKind::RLData),
        ("rlbss", SectionKind::RLBss),
        ("rlcommon", SectionKind::RLCommon),
        ("rlstack", SectionKind::RLStack),
    ] {
        let sz = section_total(layout, kind);
        out.push_str(&format_section_line(name, rcur, sz));
        rcur = rcur.saturating_add(sz);
    }

    let def_owner = build_definition_owner_map(summaries, input_paths);
    for (idx, summary) in summaries.iter().enumerate() {
        out.push_str("\n\n");
        out.push_str("==========================================================\n");
        out.push_str(&format!("{}\n", display_obj_name(input_paths.get(idx), idx)));
        out.push_str("==========================================================\n");
        out.push_str(&format_align_line(summary.object_align));

        let placement = layout
            .placements
            .get(idx)
            .map(|p| &p.by_section)
            .cloned()
            .unwrap_or_default();
        for (name, kind) in [
            ("text", SectionKind::Text),
            ("data", SectionKind::Data),
            ("bss", SectionKind::Bss),
            ("stack", SectionKind::Stack),
        ] {
            let pos = placement.get(&kind).copied().unwrap_or(0);
            let size = summary
                .declared_section_sizes
                .get(&kind)
                .copied()
                .or_else(|| summary.observed_section_usage.get(&kind).copied())
                .unwrap_or(0);
            out.push_str(&format_section_line(name, pos, size));
        }

        if !summary.xrefs.is_empty() {
            out.push_str("-------------------------- xref --------------------------\n");
            for xr in &summary.xrefs {
                let n = String::from_utf8_lossy(&xr.name);
                let owner = def_owner
                    .get(xr.name.as_slice())
                    .cloned()
                    .unwrap_or_else(|| "<unknown>".to_string());
                out.push_str(&format!("{n:<24} : in {owner}\n"));
            }
        }
        if !summary.symbols.is_empty() {
            out.push_str("-------------------------- xdef --------------------------\n");
            let mut syms = summary.symbols.iter().collect::<Vec<_>>();
            syms.sort_by(|a, b| a.name.cmp(&b.name).then(a.value.cmp(&b.value)));
            for sym in syms {
                let n = String::from_utf8_lossy(&sym.name);
                out.push_str(&format_symbol_line(&n, sym.value, section_tag(sym.section)));
            }
        }
    }
    out
}

fn build_definition_owner_map(
    summaries: &[ObjectSummary],
    input_paths: &[String],
) -> HashMap<Vec<u8>, String> {
    let mut out = HashMap::<Vec<u8>, String>::new();
    for (idx, sum) in summaries.iter().enumerate() {
        let owner = display_obj_name(input_paths.get(idx), idx);
        for sym in &sum.symbols {
            out.entry(sym.name.clone()).or_insert_with(|| owner.clone());
        }
    }
    out
}

fn display_obj_name(path: Option<&String>, idx: usize) -> String {
    if let Some(p) = path {
        return Path::new(p)
            .file_name()
            .map(|v| v.to_string_lossy().to_string())
            .unwrap_or_else(|| p.clone());
    }
    format!("obj{idx}")
}

fn format_symbol_line(name: &str, addr: u32, sect: &str) -> String {
    let mut out = format_label_prefix(name);
    out.push_str(&format!("{addr:08x} ({sect:<7})\n"));
    out
}

fn format_exec_line(exec: u32) -> String {
    let mut out = format_label_prefix("exec");
    out.push_str(&format!("{exec:08x}\n"));
    out
}

fn format_align_line(align: u32) -> String {
    let mut out = format_label_prefix("align");
    out.push_str(&format!("{align:08x}\n"));
    out
}

fn format_section_line(name: &str, pos: u32, size: u32) -> String {
    let mut label = format_label_prefix(name);
    if size == 0 {
        label.push('\n');
        return label;
    }
    let end = pos.saturating_add(size).saturating_sub(1);
    label.push_str(&format!("{pos:08x} - {end:08x} ({size:08x})\n"));
    label
}

fn format_label_prefix(name: &str) -> String {
    let tabs = if name.len() < 8 {
        3
    } else if name.len() < 16 {
        2
    } else if name.len() < 24 {
        1
    } else {
        1
    };
    format!("{name}{} : ", "\t".repeat(tabs))
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
        .map(|v| v.len() as u32)
        .unwrap_or(0);
    let data_size = linked
        .get(&SectionKind::Data)
        .map(|v| v.len() as u32)
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
    )?;
    patch_ctor_dtor_tables(&mut linked, objects, layout, &global_symbol_addrs, text_size)?;

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
    let symbol_size = symbol_data.len() as u32;
    let reloc_table = build_relocation_table(objects, summaries, layout, text_size, &global_symbol_addrs)?;
    let reloc_size = reloc_table.len() as u32;
    let (scd_line, scd_info, scd_name) = build_scd_passthrough(objects, summaries, layout)?;
    let scd_line_size = scd_line.len() as u32;
    let scd_info_size = scd_info.len() as u32;
    let scd_name_size = scd_name.len() as u32;

    let exec = resolve_exec_address(summaries, text_size, data_size, bss_size)?.unwrap_or(0);
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
        bail!("relocation target address is odd: {odd:#x}");
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
            to_human68k_path(output_path)
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
            to_human68k_path(output_path)
        );
    }
    Ok(())
}

fn to_human68k_path(path: &str) -> String {
    format!("A:{}", path.replace('/', "\\"))
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
                bump_cursor(&mut cursor_by_section, current, bytes.len() as u32);
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
        0x40 | 0x50 | 0x90 => 2,
        0x43 | 0x53 | 0x57 | 0x6b | 0x93 => 1,
        0x41 | 0x45 | 0x51 | 0x55 | 0x65 | 0x69 | 0x91 | 0x99 => 2,
        0x42 | 0x46 | 0x52 | 0x56 | 0x6a | 0x92 | 0x96 | 0x9a => 4,
        _ => 0,
    }
}

fn needs_relocation(code: u16) -> bool {
    matches!((code >> 8) as u8, 0x42 | 0x46 | 0x52 | 0x56 | 0x6a)
}

#[derive(Clone, Copy, Debug)]
struct GlobalSymbolAddr {
    section: SectionKind,
    addr: u32,
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
) -> Result<()> {
    for (idx, (obj, summary)) in objects.iter().zip(summaries.iter()).enumerate() {
        let mut current = SectionKind::Text;
        let mut cursor_by_section = BTreeMap::<SectionKind, u32>::new();
        let mut calc_stack = Vec::<ExprEntry>::new();
        for cmd in &obj.commands {
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
                Command::Opaque { code, payload } => {
                    let hi = (*code >> 8) as u8;
                    let lo = *code as u8;
                    if hi == 0x80 {
                        if let Some(entry) =
                            evaluate_push_80_for_patch(lo, payload, summary, global_symbol_addrs)
                        {
                            calc_stack.push(entry);
                        }
                    } else if hi == 0xa0 {
                        let _ = evaluate_a0(lo, &mut calc_stack);
                    }
                    let local = cursor_by_section.get(&current).copied().unwrap_or(0);
                    if let Some(bytes) =
                        materialize_stack_write_opaque(*code, &mut calc_stack).or_else(|| {
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
                            bump_cursor(&mut cursor_by_section, current, opaque_write_size(*code) as u32);
                            continue;
                        };
                        let Some(target) = linked.get_mut(&current) else {
                            bump_cursor(&mut cursor_by_section, current, opaque_write_size(*code) as u32);
                            continue;
                        };
                        let begin = (start.saturating_add(local)) as usize;
                        let end = begin.saturating_add(bytes.len());
                        if end <= target.len() {
                            target[begin..end].copy_from_slice(&bytes);
                        }
                    }
                    bump_cursor(&mut cursor_by_section, current, opaque_write_size(*code) as u32);
                }
                _ => {}
            }
        }
    }
    Ok(())
}

fn patch_ctor_dtor_tables(
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
            let Some(v) = read_u32_be(payload) else {
                continue;
            };
            match *code {
                0x4c01 => ctor_entries.push(text_base.saturating_add(v)),
                0x4d01 => dtor_entries.push(text_base.saturating_add(v)),
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

fn materialize_stack_write_opaque(code: u16, calc_stack: &mut Vec<ExprEntry>) -> Option<Vec<u8>> {
    let hi = (code >> 8) as u8;
    let v = match hi {
        0x90 | 0x91 | 0x92 | 0x93 | 0x96 | 0x99 | 0x9a => calc_stack.pop()?,
        _ => return None,
    };
    let out = match hi {
        0x90 => vec![0x00, v.value as u8],
        0x93 => vec![v.value as u8],
        0x91 | 0x99 => (v.value as u16).to_be_bytes().to_vec(),
        0x92 | 0x96 | 0x9a => (v.value as u32).to_be_bytes().to_vec(),
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
        let xref = summary.xrefs.iter().find(|x| x.value == label_no as u32)?;
        let sym = global_symbol_addrs.get(&xref.name)?;
        let stat = match section_stat(sym.section) {
            0 => 0,
            1 => 1,
            _ => 2,
        };
        return Some(ExprEntry {
            stat,
            value: sym.addr as i32,
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
    let hi = (code >> 8) as u8;
    let value = resolve_opaque_value(code, payload, summary, global_symbol_addrs, placement)?;
    match hi {
        0x40 | 0x50 => Some(vec![0x00, value as u8]),
        0x43 | 0x47 | 0x53 | 0x57 | 0x6b => Some(vec![value as u8]),
        0x41 | 0x45 | 0x51 | 0x55 | 0x65 | 0x69 => Some((value as u16).to_be_bytes().to_vec()),
        0x42 | 0x46 | 0x52 | 0x56 | 0x6a => Some((value as u32).to_be_bytes().to_vec()),
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
    let hi = (code >> 8) as u8;
    let lo = code as u8;

    if matches!(hi, 0x65 | 0x69 | 0x6a | 0x6b) {
        let adr = read_i32_be(payload)?;
        let label_no = read_u16_be(payload.get(4..)?)?;
        let xref = summary.xrefs.iter().find(|x| x.value == label_no as u32)?;
        let sym = global_symbol_addrs.get(&xref.name)?;
        let base = section_value_with_placement(lo, adr, placement)?;
        return Some((sym.addr as i32).wrapping_sub(base));
    }

    let mut base = if matches!(lo, 0xfc..=0xff) {
        let label_no = read_u16_be(payload)?;
        let xref = summary.xrefs.iter().find(|x| x.value == label_no as u32)?;
        global_symbol_addrs.get(&xref.name).map(|v| v.addr as i32)?
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
    let base = placement.get(&sect).copied().unwrap_or(0) as i32;
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
    let lo = (code & 0x00ff) as u8;
    if is_reloc_section(lo) {
        return true;
    }
    if lo == 0xff {
        let Some(label_no) = read_u16_be(payload) else {
            return false;
        };
        let Some(xref) = summary.xrefs.iter().find(|x| x.value == label_no as u32) else {
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
        bail!("not MACS format");
    }
    if &payload[0..4] != b"MACS" || &payload[4..8] != b"DATA" {
        bail!("not MACS format");
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
    let base = match sect as u8 {
        0x01 => 0,
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
) -> Result<()> {
    validate_unsupported_expression_commands(objects, input_paths, summaries)
}

fn validate_unsupported_expression_commands(
    objects: &[ObjectFile],
    input_paths: &[String],
    summaries: &[ObjectSummary],
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
            .map(|s| s.to_owned())
            .unwrap_or_else(|| format!("obj{obj_idx}.o"));
        let mut current = SectionKind::Text;
        let mut cursor_by_section = BTreeMap::<SectionKind, u32>::new();
        let mut calc_stack = Vec::<ExprEntry>::new();
        let mut has_ctor = false;
        let mut has_dtor = false;
        let mut has_doctor = false;
        let mut has_dodtor = false;
        let mut ctor_count = 0usize;
        let mut dtor_count = 0usize;
        let mut ctor_header_size = None::<u32>;
        let mut dtor_header_size = None::<u32>;
        for cmd in &obj.commands {
            match cmd {
                Command::Header { section, size, .. } => match *section {
                    0x0c => ctor_header_size = Some(*size),
                    0x0d => dtor_header_size = Some(*size),
                    _ => {}
                },
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
                    match *code {
                        0x4c01 => {
                            has_ctor = true;
                            ctor_count += 1;
                        }
                        0x4d01 => {
                            has_dtor = true;
                            dtor_count += 1;
                        }
                        0xe00c => has_doctor = true,
                        0xe00d => has_dodtor = true,
                        _ => {}
                    }
                    let local = cursor_by_section.get(&current).copied().unwrap_or(0);
                    let messages = classify_expression_errors(
                        *code,
                        cmd,
                        summary,
                        &global_symbols,
                        current,
                        &mut calc_stack,
                    );
                    for msg in messages {
                        diagnostics.push(format!("{msg} in {obj_name}\n at {local:08x} ({})", section_name(current)));
                    }
                    let write_size = opaque_write_size(*code) as u32;
                    bump_cursor(&mut cursor_by_section, current, write_size);
                }
                _ => {}
            }
        }
        if has_ctor && !has_doctor {
            diagnostics.push(format!(".doctor なしで .ctor が使われています in {obj_name}"));
        }
        if has_dtor && !has_dodtor {
            diagnostics.push(format!(".dodtor なしで .dtor が使われています in {obj_name}"));
        }
        if let Some(size) = ctor_header_size {
            let expected = (ctor_count as u32).saturating_mul(4);
            if size != expected {
                diagnostics.push(format!(
                    "ctor header size mismatch in {obj_name}: header={size} expected={expected}"
                ));
            }
        }
        if let Some(size) = dtor_header_size {
            let expected = (dtor_count as u32).saturating_mul(4);
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

#[derive(Clone, Copy, Debug)]
struct ExprEntry {
    stat: i16,
    value: i32,
}

fn classify_expression_errors(
    code: u16,
    cmd: &Command,
    summary: &ObjectSummary,
    global_symbols: &HashMap<Vec<u8>, Symbol>,
    current: SectionKind,
    calc_stack: &mut Vec<ExprEntry>,
) -> Vec<&'static str> {
    const CALC_STACK_SIZE_HLK: usize = 1024;
    let hi = (code >> 8) as u8;
    let lo = code as u8;
    let payload = match cmd {
        Command::Opaque { payload, .. } => payload.as_slice(),
        _ => &[],
    };
    match hi {
        0x80 => {
            if calc_stack.len() >= CALC_STACK_SIZE_HLK {
                return vec!["計算用スタックが溢れました"];
            }
            if let Some(entry) = evaluate_push_80(lo, payload, summary, global_symbols) {
                calc_stack.push(entry);
            }
            Vec::new()
        }
        0xa0 => evaluate_a0(lo, calc_stack),
        0x90 => evaluate_wrt_stk_9000(calc_stack),
        0x91 => evaluate_wrt_stk_9100(calc_stack, current),
        0x92 => evaluate_wrt_stk_long(calc_stack),
        0x93 => evaluate_wrt_stk_9300(calc_stack),
        0x96 => evaluate_wrt_stk_long(calc_stack),
        0x99 => evaluate_wrt_stk_9900(calc_stack, current),
        0x9a => evaluate_wrt_stk_long(calc_stack),
        0x40 | 0x43 => evaluate_direct_byte(hi, lo, payload, summary, global_symbols),
        0x50 | 0x53 => evaluate_direct_byte_with_offset(hi, lo, payload, summary, global_symbols),
        0x41 => evaluate_direct_word(lo, payload, summary, global_symbols, current),
        0x51 => evaluate_direct_word_with_offset(lo, payload, summary, global_symbols, current),
        0x65 => evaluate_rel_word(lo, payload, summary, global_symbols),
        0x6a => evaluate_d32_adrs(lo, payload, summary, global_symbols),
        0x6b => evaluate_rel_byte(lo, payload, summary, global_symbols),
        _ => Vec::new(),
    }
}

fn evaluate_push_80(
    lo: u8,
    payload: &[u8],
    summary: &ObjectSummary,
    global_symbols: &HashMap<Vec<u8>, Symbol>,
) -> Option<ExprEntry> {
    if matches!(lo, 0xfc..=0xff) {
        let label_no = read_u16_be(payload)?;
        let (section, value) = resolve_xref(label_no, summary, global_symbols)?;
        let stat = match section_stat(section) {
            0 => 0,
            1 => 1,
            _ => 2,
        };
        return Some(ExprEntry { stat, value });
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

fn evaluate_a0(lo: u8, calc_stack: &mut Vec<ExprEntry>) -> Vec<&'static str> {
    match lo {
        0x02 => {
            let Some(a) = calc_stack.pop() else {
                return vec!["計算用スタックに値がありません"];
            };
            calc_stack.push(a);
            Vec::new()
        }
        0x01 | 0x03 | 0x04 | 0x05 | 0x06 | 0x07 => {
            let Some(mut a) = calc_stack.pop() else {
                return vec!["計算用スタックに値がありません"];
            };
            if a.stat > 0 {
                a.stat = -1;
                calc_stack.push(a);
                return vec!["不正な式"];
            }
            if a.stat == 0 {
                a.value = match lo {
                    0x01 => a.value.wrapping_neg(),
                    0x03 => {
                        if a.value == 0 {
                            -1
                        } else {
                            0
                        }
                    }
                    0x04 => ((((a.value as u32) & 0xffff) >> 8) as u16 as i16) as i32,
                    0x05 => (a.value as u32 & 0xff) as i32,
                    0x06 => ((a.value as u32) >> 16) as i32,
                    0x07 => (a.value as u32 & 0xffff) as i32,
                    _ => a.value,
                };
            }
            calc_stack.push(a);
            Vec::new()
        }
        0x0a | 0x0b => {
            let Some(a) = calc_stack.pop() else {
                return vec!["計算用スタックに値がありません"];
            };
            let Some(b) = calc_stack.pop() else {
                calc_stack.push(a);
                return vec!["計算用スタックに値がありません"];
            };
            let (res, errors) = eval_chk_calcexp2(a, b);
            if let Some(mut r) = res {
                if r.stat >= 0 && a.value == 0 {
                    r.stat = -1;
                    calc_stack.push(r);
                    let mut out = errors;
                    out.push("ゼロ除算");
                    return out;
                }
                if r.stat == 0 {
                    if lo == 0x0a {
                        r.value = b.value.wrapping_div(a.value);
                    } else {
                        // HLK's divs_d0d1 leaves abs(remainder).
                        r.value = b.value.wrapping_rem(a.value).abs();
                    }
                }
                calc_stack.push(r);
            }
            errors
        }
        0x0f => {
            let Some(a) = calc_stack.pop() else {
                return vec!["計算用スタックに値がありません"];
            };
            let Some(b) = calc_stack.pop() else {
                calc_stack.push(a);
                return vec!["計算用スタックに値がありません"];
            };
            let mut errors = Vec::new();
            let mut out = ExprEntry {
                stat: -1,
                value: b.value.wrapping_sub(a.value),
            };
            if a.stat == 0 {
                out.stat = b.stat ^ a.stat;
            } else if a.stat < 0 || b.stat < 0 {
                out.stat = -1;
            } else if a.stat != b.stat {
                errors.push("不正な式");
            } else {
                out.stat = b.stat ^ a.stat;
            }
            calc_stack.push(out);
            errors
        }
        0x10 => {
            let Some(a) = calc_stack.pop() else {
                return vec!["計算用スタックに値がありません"];
            };
            let Some(b) = calc_stack.pop() else {
                calc_stack.push(a);
                return vec!["計算用スタックに値がありません"];
            };
            let mut errors = Vec::new();
            let mut out = ExprEntry {
                stat: -1,
                value: b.value.wrapping_add(a.value),
            };
            if a.stat == 0 {
                out.stat = b.stat ^ a.stat;
            } else if a.stat < 0 {
                out.stat = -1;
            } else if b.stat == 0 {
                out.stat = b.stat ^ a.stat;
            } else {
                if b.stat >= 0 {
                    errors.push("不正な式");
                }
                out.stat = -1;
            }
            calc_stack.push(out);
            errors
        }
        0x09 | 0x0c | 0x0d | 0x0e | 0x11..=0x1d => {
            let Some(a) = calc_stack.pop() else {
                return vec!["計算用スタックに値がありません"];
            };
            let Some(b) = calc_stack.pop() else {
                calc_stack.push(a);
                return vec!["計算用スタックに値がありません"];
            };
            let (res, errors) = eval_chk_calcexp2(a, b);
            if let Some(mut r) = res {
                if r.stat == 0 {
                    r.value = eval_a0_const_binop(lo, b.value, a.value);
                }
                calc_stack.push(r);
            }
            errors
        }
        _ => Vec::new(),
    }
}

fn eval_a0_const_binop(lo: u8, b: i32, a: i32) -> i32 {
    match lo {
        0x09 => b.wrapping_mul(a),
        0x0c => ((b as u32) >> ((a as u32) & 63)) as i32,
        0x0d => ((b as u32) << ((a as u32) & 63)) as i32,
        0x0e => b.wrapping_shr((a as u32) & 63),
        0x11 => if b == a { -1 } else { 0 },
        0x12 => if b != a { -1 } else { 0 },
        0x13 => if (b as u32) < (a as u32) { -1 } else { 0 },
        0x14 => if (b as u32) <= (a as u32) { -1 } else { 0 },
        0x15 => if (b as u32) > (a as u32) { -1 } else { 0 },
        0x16 => if (b as u32) >= (a as u32) { -1 } else { 0 },
        0x17 => if b < a { -1 } else { 0 },
        0x18 => if b <= a { -1 } else { 0 },
        0x19 => if b > a { -1 } else { 0 },
        0x1a => if b >= a { -1 } else { 0 },
        0x1b => b & a,
        0x1c => b ^ a,
        0x1d => b | a,
        _ => b,
    }
}

fn eval_chk_calcexp2(a: ExprEntry, b: ExprEntry) -> (Option<ExprEntry>, Vec<&'static str>) {
    let mut errors = Vec::new();
    let mut stat = 0;
    if a.stat != 0 {
        if a.stat > 0 {
            errors.push("不正な式");
        }
        stat = -1;
    } else if b.stat != 0 {
        if b.stat > 0 {
            errors.push("不正な式");
        }
        stat = -1;
    }
    (
        Some(ExprEntry {
            stat,
            value: b.value,
        }),
        errors,
    )
}

fn evaluate_wrt_stk_9000(calc_stack: &mut Vec<ExprEntry>) -> Vec<&'static str> {
    evaluate_wrt_stk_byte(calc_stack)
}

fn evaluate_wrt_stk_9300(calc_stack: &mut Vec<ExprEntry>) -> Vec<&'static str> {
    evaluate_wrt_stk_byte(calc_stack)
}

fn evaluate_wrt_stk_long(calc_stack: &mut Vec<ExprEntry>) -> Vec<&'static str> {
    let Some(_v) = calc_stack.pop() else {
        return vec!["計算用スタックに値がありません"];
    };
    Vec::new()
}

fn evaluate_wrt_stk_byte(calc_stack: &mut Vec<ExprEntry>) -> Vec<&'static str> {
    let Some(v) = calc_stack.pop() else {
        return vec!["計算用スタックに値がありません"];
    };
    if v.stat == 0 {
        if !fits_byte(v.value) {
            return vec!["バイトサイズ(-$80〜$ff)で表現できない値"];
        }
        return Vec::new();
    }
    if v.stat < 0 {
        return Vec::new();
    }
    vec!["アドレス属性シンボルの値をバイトサイズで出力"]
}

fn evaluate_wrt_stk_9100(calc_stack: &mut Vec<ExprEntry>, current: SectionKind) -> Vec<&'static str> {
    let Some(v) = calc_stack.pop() else {
        return vec!["計算用スタックに値がありません"];
    };
    if v.stat == 0 {
        if !fits_word(v.value) {
            return vec!["ワードサイズ(-$8000〜$ffff)で表現できない値"];
        }
        return Vec::new();
    }
    if v.stat < 0 {
        return Vec::new();
    }
    if v.stat == 1 {
        return vec!["アドレス属性シンボルの値をワードサイズで出力"];
    }
    if section_number(current) <= 4 {
        if !fits_word2(v.value) {
            return vec!["ワードサイズ(-$8000〜$7fff)で表現できない値"];
        }
        return Vec::new();
    }
    vec!["アドレス属性シンボルの値をワードサイズで出力"]
}

fn evaluate_wrt_stk_9900(calc_stack: &mut Vec<ExprEntry>, current: SectionKind) -> Vec<&'static str> {
    let Some(v) = calc_stack.pop() else {
        return vec!["計算用スタックに値がありません"];
    };
    if v.stat == 0 {
        if !fits_word2(v.value) {
            return vec!["ワードサイズ(-$8000〜$7fff)で表現できない値"];
        }
        return Vec::new();
    }
    if v.stat < 0 {
        return Vec::new();
    }
    if v.stat == 1 || section_number(current) > 4 {
        return vec!["アドレス属性シンボルの値をワードサイズで出力"];
    }
    if !fits_word2(v.value) {
        return vec!["ワードサイズ(-$8000〜$7fff)で表現できない値"];
    }
    Vec::new()
}

fn evaluate_direct_byte(
    _hi: u8,
    lo: u8,
    payload: &[u8],
    summary: &ObjectSummary,
    global_symbols: &HashMap<Vec<u8>, Symbol>,
) -> Vec<&'static str> {
    if matches!(lo, 0xfc..=0xff) {
        if lo != 0xff {
            return vec!["アドレス属性シンボルの値をバイトサイズで出力"];
        }
        let Some(label_no) = read_u16_be(payload) else {
            return Vec::new();
        };
        if let Some((section, value)) = resolve_xref(label_no, summary, global_symbols) {
            if section_stat(section) != 0 {
                return vec!["アドレス属性シンボルの値をバイトサイズで出力"];
            }
            if !fits_byte(value) {
                return vec!["バイトサイズ(-$80〜$ff)で表現できない値"];
            }
        }
        return Vec::new();
    }
    let Some(value) = read_i32_be(payload) else {
        return Vec::new();
    };
    if !fits_byte(value) {
        return vec!["バイトサイズ(-$80〜$ff)で表現できない値"];
    }
    Vec::new()
}

fn evaluate_direct_byte_with_offset(
    _hi: u8,
    lo: u8,
    payload: &[u8],
    summary: &ObjectSummary,
    global_symbols: &HashMap<Vec<u8>, Symbol>,
) -> Vec<&'static str> {
    if matches!(lo, 0xfc..=0xff) {
        if lo != 0xff {
            return vec!["アドレス属性シンボルの値をバイトサイズで出力"];
        }
        let Some(label_no) = read_u16_be(payload) else {
            return Vec::new();
        };
        let offset = read_i32_be(&payload[2..]).unwrap_or(0);
        if let Some((section, value)) = resolve_xref(label_no, summary, global_symbols) {
            let total = value.wrapping_add(offset);
            if section_stat(section) != 0 {
                return vec!["アドレス属性シンボルの値をバイトサイズで出力"];
            }
            if !fits_byte(total) {
                return vec!["バイトサイズ(-$80〜$ff)で表現できない値"];
            }
        }
        return Vec::new();
    }
    let Some(value) = read_i32_be(payload) else {
        return Vec::new();
    };
    let offset = read_i32_be(&payload[4..]).unwrap_or(0);
    if !fits_byte(value.wrapping_add(offset)) {
        return vec!["バイトサイズ(-$80〜$ff)で表現できない値"];
    }
    Vec::new()
}

fn evaluate_direct_word(
    lo: u8,
    payload: &[u8],
    summary: &ObjectSummary,
    global_symbols: &HashMap<Vec<u8>, Symbol>,
    current: SectionKind,
) -> Vec<&'static str> {
    if matches!(lo, 0xfc..=0xff) {
        if lo != 0xff {
            return vec!["アドレス属性シンボルの値をワードサイズで出力"];
        }
        let Some(label_no) = read_u16_be(payload) else {
            return Vec::new();
        };
        if let Some((section, value)) = resolve_xref(label_no, summary, global_symbols) {
            let stat = section_stat(section);
            if stat == 0 {
                if !fits_word(value) {
                    return vec!["ワードサイズ(-$8000〜$ffff)で表現できない値"];
                }
                return Vec::new();
            }
            if stat == 1 {
                return vec!["アドレス属性シンボルの値をワードサイズで出力"];
            }
            if section_number(current) <= 4 {
                if !fits_word2(value) {
                    return vec!["ワードサイズ(-$8000〜$7fff)で表現できない値"];
                }
                return Vec::new();
            }
            return vec!["アドレス属性シンボルの値をワードサイズで出力"];
        }
        return Vec::new();
    }
    let Some(value) = read_i32_be(payload) else {
        return Vec::new();
    };
    if !fits_word(value) {
        return vec!["ワードサイズ(-$8000〜$ffff)で表現できない値"];
    }
    Vec::new()
}

fn evaluate_direct_word_with_offset(
    lo: u8,
    payload: &[u8],
    summary: &ObjectSummary,
    global_symbols: &HashMap<Vec<u8>, Symbol>,
    current: SectionKind,
) -> Vec<&'static str> {
    if matches!(lo, 0xfc..=0xff) {
        if lo != 0xff {
            return vec!["アドレス属性シンボルの値をワードサイズで出力"];
        }
        let Some(label_no) = read_u16_be(payload) else {
            return Vec::new();
        };
        let offset = read_i32_be(&payload[2..]).unwrap_or(0);
        if let Some((section, value)) = resolve_xref(label_no, summary, global_symbols) {
            let total = value.wrapping_add(offset);
            let stat = section_stat(section);
            if stat == 0 {
                if !fits_word(total) {
                    return vec!["ワードサイズ(-$8000〜$ffff)で表現できない値"];
                }
                return Vec::new();
            }
            if stat == 1 {
                return vec!["アドレス属性シンボルの値をワードサイズで出力"];
            }
            if section_number(current) <= 4 {
                if !fits_word2(total) {
                    return vec!["ワードサイズ(-$8000〜$7fff)で表現できない値"];
                }
                return Vec::new();
            }
            return vec!["アドレス属性シンボルの値をワードサイズで出力"];
        }
        return Vec::new();
    }
    let Some(value) = read_i32_be(payload) else {
        return Vec::new();
    };
    let offset = read_i32_be(&payload[4..]).unwrap_or(0);
    if !fits_word(value.wrapping_add(offset)) {
        return vec!["ワードサイズ(-$8000〜$ffff)で表現できない値"];
    }
    Vec::new()
}

fn resolve_xref(
    label_no: u16,
    summary: &ObjectSummary,
    global_symbols: &HashMap<Vec<u8>, Symbol>,
) -> Option<(SectionKind, i32)> {
    let xref = summary
        .xrefs
        .iter()
        .find(|x| x.value == label_no as u32)?;
    let target = global_symbols.get(&xref.name)?;
    Some((target.section, target.value as i32))
}

fn evaluate_rel_word(
    _lo: u8,
    payload: &[u8],
    summary: &ObjectSummary,
    global_symbols: &HashMap<Vec<u8>, Symbol>,
) -> Vec<&'static str> {
    let Some(label_no) = read_u16_be(payload.get(4..).unwrap_or(&[])) else {
        return Vec::new();
    };
    if let Some((section, _)) = resolve_xref(label_no, summary, global_symbols) {
        if section_stat(section) == 0 {
            return vec!["アドレス属性シンボルの値をワードサイズで出力"];
        }
    }
    vec!["ワードサイズ(-$8000〜$7fff)で表現できない値"]
}

fn evaluate_rel_byte(
    _lo: u8,
    payload: &[u8],
    summary: &ObjectSummary,
    global_symbols: &HashMap<Vec<u8>, Symbol>,
) -> Vec<&'static str> {
    let Some(label_no) = read_u16_be(payload.get(4..).unwrap_or(&[])) else {
        return Vec::new();
    };
    if let Some((section, _)) = resolve_xref(label_no, summary, global_symbols) {
        if section_stat(section) == 0 {
            return vec!["アドレス属性シンボルの値をバイトサイズで出力"];
        }
    }
    vec!["バイトサイズ(-$80〜$7f)で表現できない値"]
}

fn evaluate_d32_adrs(
    _lo: u8,
    payload: &[u8],
    summary: &ObjectSummary,
    global_symbols: &HashMap<Vec<u8>, Symbol>,
) -> Vec<&'static str> {
    let Some(label_no) = read_u16_be(payload.get(4..).unwrap_or(&[])) else {
        return Vec::new();
    };
    if let Some((section, _)) = resolve_xref(label_no, summary, global_symbols) {
        if matches!(
            section,
            SectionKind::Text | SectionKind::Data | SectionKind::Bss | SectionKind::Stack | SectionKind::Common
        ) {
            return Vec::new();
        }
    }
    vec!["32ビットディスプレースメントにアドレス属性シンボルの値を出力"]
}

fn section_stat(section: SectionKind) -> i16 {
    match section {
        SectionKind::Abs => 0,
        SectionKind::Text
        | SectionKind::Data
        | SectionKind::Bss
        | SectionKind::Stack
        | SectionKind::Common => 1,
        _ => 2,
    }
}

fn section_number(section: SectionKind) -> u8 {
    match section {
        SectionKind::Text => 1,
        SectionKind::Data => 2,
        SectionKind::Bss => 3,
        SectionKind::Stack => 4,
        SectionKind::RData => 5,
        SectionKind::RBss => 6,
        SectionKind::RStack => 7,
        SectionKind::RLData => 8,
        SectionKind::RLBss => 9,
        SectionKind::RLStack => 10,
        _ => 0,
    }
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

fn fits_byte(v: i32) -> bool {
    (-0x80..=0xff).contains(&v)
}

fn fits_word(v: i32) -> bool {
    (-0x8000..=0xffff).contains(&v)
}

fn fits_word2(v: i32) -> bool {
    (-0x8000..=0x7fff).contains(&v)
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
mod tests {
    use std::collections::{BTreeMap, HashMap};

    use super::{classify_expression_errors, evaluate_a0, ExprEntry};

    use crate::format::obj::{Command, ObjectFile};
    use crate::layout::plan_layout;
    use crate::resolver::{ObjectSummary, SectionKind, Symbol, resolve_object};
    use crate::writer::{
        apply_x_header_options, build_map_text, build_r_payload, build_x_image,
        build_x_image_with_options, validate_link_inputs,
        patch_section_size_info,
        validate_r_convertibility,
    };

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
    fn cuts_symbol_table_with_x_option() {
        let obj = ObjectFile {
            commands: vec![
                Command::Header {
                    section: 0x01,
                    size: 2,
                    name: b"text".to_vec(),
                },
                Command::ChangeSection { section: 0x01 },
                Command::RawData(vec![0x4e, 0x75]),
                Command::DefineSymbol {
                    section: 0x01,
                    value: 0,
                    name: b"_entry".to_vec(),
                },
                Command::End,
            ],
            scd_tail: Vec::new(),
        };
        let mut sum = mk_summary(2, 2, 0);
        sum.symbols.push(Symbol {
            name: b"_entry".to_vec(),
            section: SectionKind::Text,
            value: 0,
        });
        let layout = plan_layout(std::slice::from_ref(&sum));
        let with_symbols =
            build_x_image_with_options(&[obj.clone()], &[sum.clone()], &layout, true).expect("x image");
        let without_symbols = build_x_image_with_options(&[obj], &[sum], &layout, false).expect("x image");

        let with_sym_size =
            u32::from_be_bytes([with_symbols[28], with_symbols[29], with_symbols[30], with_symbols[31]]);
        let without_sym_size = u32::from_be_bytes([
            without_symbols[28],
            without_symbols[29],
            without_symbols[30],
            without_symbols[31],
        ]);
        assert!(with_sym_size > 0);
        assert_eq!(without_sym_size, 0);
    }

    #[test]
    fn applies_x_header_options() {
        let mut payload = vec![0u8; 64];
        payload[0] = b'H';
        payload[1] = b'U';
        payload[8..12].copy_from_slice(&0x0000_0012u32.to_be_bytes());
        apply_x_header_options(&mut payload, 0x0000_6800, 2).expect("patch header");
        assert_eq!(payload[3], 2);
        assert_eq!(&payload[4..8], &0x0000_6800u32.to_be_bytes());
        assert_eq!(&payload[8..12], &0x0000_6812u32.to_be_bytes());
    }

    #[test]
    fn patches_section_size_info_block() {
        let obj = ObjectFile {
            commands: vec![
                Command::Header {
                    section: 0x02,
                    size: 0x40,
                    name: b"data".to_vec(),
                },
                Command::DefineSymbol {
                    section: 0x02,
                    value: 0,
                    name: b"___size_info".to_vec(),
                },
                Command::ChangeSection { section: 0x02 },
                Command::DefineSpace { size: 0x40 },
                Command::End,
            ],
            scd_tail: Vec::new(),
        };
        let sum = resolve_object(&obj);
        let layout = plan_layout(std::slice::from_ref(&sum));
        let mut image = build_x_image(&[obj], &[sum], &layout).expect("x");
        image[24..28].copy_from_slice(&0x1122_3344u32.to_be_bytes());
        patch_section_size_info(&mut image, false, &layout_dummy_summaries_with_size_info(), &layout)
            .expect("patch");
        // text/data/bss/common/stack/r*/roff
        let base = 64usize;
        assert_eq!(&image[base..base + 4], &0u32.to_be_bytes());
        assert_eq!(&image[base + 4..base + 8], &0x40u32.to_be_bytes());
        assert_eq!(&image[base + 52..base + 56], &0x1122_3344u32.to_be_bytes());
    }

    fn layout_dummy_summaries_with_size_info() -> Vec<ObjectSummary> {
        vec![ObjectSummary {
            object_align: 2,
            declared_section_sizes: BTreeMap::new(),
            observed_section_usage: BTreeMap::new(),
            symbols: vec![Symbol {
                name: b"___size_info".to_vec(),
                section: SectionKind::Data,
                value: 0,
            }],
            xrefs: Vec::new(),
            requests: Vec::new(),
            start_address: None,
        }]
    }

    #[test]
    fn builds_map_text_with_symbol_addresses() {
        let mut s0 = mk_summary(2, 2, 0);
        s0.symbols.push(Symbol {
            name: b"_text0".to_vec(),
            section: SectionKind::Text,
            value: 0,
        });
        let mut s1 = mk_summary(2, 2, 2);
        s1.symbols.push(Symbol {
            name: b"_data0".to_vec(),
            section: SectionKind::Data,
            value: 1,
        });
        let layout = plan_layout(&[s0.clone(), s1.clone()]);
        let text = build_map_text("a.x", &[s0, s1], &layout, 4, 2, 0, 0, &[]);
        assert!(text.contains("=========================================================="));
        assert!(text.contains("A:a.x"));
        assert!(text.contains("exec\t\t\t : 00000000"));
        assert!(text.contains("text\t\t\t : 00000000 - 00000003 (00000004)"));
        assert!(text.contains("data\t\t\t : 00000004 - 00000005 (00000002)"));
        assert!(text.contains("-------------------------- xdef --------------------------"));
        assert!(text.contains("_text0\t\t\t : 00000000 (text   )"));
        assert!(text.contains("_data0\t\t\t : 00000001 (data   )"));
        assert!(text.contains("obj0"));
        assert!(text.contains("align\t\t\t : 00000002"));
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
        assert_eq!(reloc_size, 4);
        // relocation table begins after header + text + data
        let reloc_pos = 64 + 14;
        assert_eq!(&image[reloc_pos..reloc_pos + 4], &[0x00, 0x00, 0x00, 0x06]);
    }

    #[test]
    fn patches_xref_long_value_and_keeps_following_rawdata_position() {
        let main_obj = ObjectFile {
            commands: vec![
                Command::Header {
                    section: 0x01,
                    size: 8,
                    name: b"text".to_vec(),
                },
                Command::DefineSymbol {
                    section: 0xff, // xref
                    value: 1,      // label no
                    name: b"func".to_vec(),
                },
                Command::DefineSymbol {
                    section: 0x01,
                    value: 0,
                    name: b"start".to_vec(),
                },
                Command::ChangeSection { section: 0x01 },
                Command::RawData(vec![0x4e, 0xb9]), // jsr abs.l
                Command::Opaque {
                    code: 0x42ff, // long xref
                    payload: vec![0x00, 0x01],
                },
                Command::RawData(vec![0x4e, 0x75]), // rts
                Command::End,
            ],
            scd_tail: Vec::new(),
        };
        let sub_obj = ObjectFile {
            commands: vec![
                Command::Header {
                    section: 0x01,
                    size: 2,
                    name: b"text".to_vec(),
                },
                Command::DefineSymbol {
                    section: 0x01,
                    value: 0,
                    name: b"func".to_vec(),
                },
                Command::ChangeSection { section: 0x01 },
                Command::RawData(vec![0x4e, 0x75]),
                Command::End,
            ],
            scd_tail: Vec::new(),
        };

        let mut main_sum = mk_summary(2, 8, 0);
        main_sum.xrefs.push(Symbol {
            name: b"func".to_vec(),
            section: SectionKind::Xref,
            value: 1,
        });
        main_sum.symbols.push(Symbol {
            name: b"start".to_vec(),
            section: SectionKind::Text,
            value: 0,
        });

        let mut sub_sum = mk_summary(2, 2, 0);
        sub_sum.symbols.push(Symbol {
            name: b"func".to_vec(),
            section: SectionKind::Text,
            value: 0,
        });

        let layout = plan_layout(&[main_sum.clone(), sub_sum.clone()]);
        let image = build_x_image(&[main_obj, sub_obj], &[main_sum, sub_sum], &layout).expect("x image");

        // text: jsr abs.l func ; rts ; func: rts
        assert_eq!(
            &image[64..74],
            &[0x4e, 0xb9, 0x00, 0x00, 0x00, 0x08, 0x4e, 0x75, 0x4e, 0x75]
        );
        // relocation size should contain one entry for the long address at text+2
        assert_eq!(&image[24..28], &(2u32.to_be_bytes()));
        assert_eq!(&image[74..76], &[0x00, 0x02]);
    }

    #[test]
    fn does_not_relocate_xref_long_when_symbol_is_absolute() {
        let user_obj = ObjectFile {
            commands: vec![
                Command::Header {
                    section: 0x01,
                    size: 4,
                    name: b"text".to_vec(),
                },
                Command::DefineSymbol {
                    section: 0xff,
                    value: 1,
                    name: b"abs_sym".to_vec(),
                },
                Command::ChangeSection { section: 0x01 },
                Command::Opaque {
                    code: 0x42ff,
                    payload: vec![0x00, 0x01],
                },
                Command::End,
            ],
            scd_tail: Vec::new(),
        };
        let abs_obj = ObjectFile {
            commands: vec![
                Command::Header {
                    section: 0x01,
                    size: 0,
                    name: b"text".to_vec(),
                },
                Command::DefineSymbol {
                    section: 0x00, // absolute
                    value: 0x1234_5678,
                    name: b"abs_sym".to_vec(),
                },
                Command::End,
            ],
            scd_tail: Vec::new(),
        };

        let mut user_sum = mk_summary(2, 4, 0);
        user_sum.xrefs.push(Symbol {
            name: b"abs_sym".to_vec(),
            section: SectionKind::Xref,
            value: 1,
        });
        let mut abs_sum = mk_summary(2, 0, 0);
        abs_sum.symbols.push(Symbol {
            name: b"abs_sym".to_vec(),
            section: SectionKind::Abs,
            value: 0x1234_5678,
        });

        let layout = plan_layout(&[user_sum.clone(), abs_sum.clone()]);
        let image = build_x_image(&[user_obj, abs_obj], &[user_sum, abs_sum], &layout).expect("x image");
        // text payload must be absolute immediate, with no relocation entries.
        assert_eq!(&image[64..68], &[0x12, 0x34, 0x56, 0x78]);
        assert_eq!(&image[24..28], &(0u32.to_be_bytes()));
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
        let err = validate_r_convertibility(&[obj], &[sum], &layout, "out.r")
            .expect_err("should reject conversion");
        assert!(err.to_string().contains("再配置テーブルが使われています"));
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
        let err = validate_r_convertibility(&[obj], &[sum], &layout, "out.r")
            .expect_err("should reject conversion");
        assert!(err.to_string().contains("実行開始アドレスがファイル先頭ではありません"));
    }

    #[test]
    fn rejects_multiple_start_addresses() {
        let obj0 = ObjectFile {
            commands: vec![
                Command::Header {
                    section: 0x01,
                    size: 2,
                    name: b"text".to_vec(),
                },
                Command::End,
            ],
            scd_tail: Vec::new(),
        };
        let obj1 = obj0.clone();
        let mut sum0 = mk_summary(2, 2, 0);
        let mut sum1 = mk_summary(2, 2, 0);
        sum0.start_address = Some((0x01, 0));
        sum1.start_address = Some((0x01, 0));
        let layout = plan_layout(&[sum0.clone(), sum1.clone()]);
        let err = build_x_image(&[obj0, obj1], &[sum0, sum1], &layout).expect_err("must reject");
        assert!(err.to_string().contains("multiple start addresses"));
    }

    #[test]
    fn rejects_odd_relocation_target() {
        let obj = ObjectFile {
            commands: vec![
                Command::Header {
                    section: 0x01,
                    size: 6,
                    name: b"text".to_vec(),
                },
                Command::ChangeSection { section: 0x01 },
                Command::RawData(vec![0xaa]), // make relocation position odd
                Command::Opaque {
                    code: 0x4201, // long relocation candidate
                    payload: vec![0, 0, 0, 0],
                },
                Command::End,
            ],
            scd_tail: Vec::new(),
        };
        let sum = mk_summary(2, 6, 0);
        let layout = plan_layout(&[sum.clone()]);
        let err = build_x_image(&[obj], &[sum], &layout).expect_err("must reject odd relocation");
        assert!(err.to_string().contains("relocation target address is odd"));
    }

    #[test]
    fn rejects_unimplemented_expression_commands() {
        let obj = ObjectFile {
            commands: vec![
                Command::Header {
                    section: 0x01,
                    size: 0,
                    name: b"text".to_vec(),
                },
                Command::Opaque {
                    code: 0x8001,
                    payload: vec![0, 0, 0, 0],
                },
                Command::Opaque {
                    code: 0xa001,
                    payload: Vec::new(),
                },
                Command::End,
            ],
            scd_tail: Vec::new(),
        };
        let err =
            validate_link_inputs(&[obj], &[], &[mk_summary(2, 0, 0)]).expect_err("must reject expression command");
        assert!(err.to_string().contains("不正な式"));
    }

    #[test]
    fn patches_ctor_dtor_tables_from_opaque_commands() {
        let sys = ObjectFile {
            commands: vec![
                Command::Header {
                    section: 0x01,
                    size: 2,
                    name: b"text".to_vec(),
                },
                Command::Header {
                    section: 0x02,
                    size: 32,
                    name: b"data".to_vec(),
                },
                Command::ChangeSection { section: 0x01 },
                Command::RawData(vec![0x4e, 0x75]),
                Command::ChangeSection { section: 0x02 },
                Command::RawData(vec![0; 32]),
                Command::DefineSymbol {
                    section: 0x02,
                    value: 4,
                    name: b"___CTOR_LIST__".to_vec(),
                },
                Command::DefineSymbol {
                    section: 0x02,
                    value: 16,
                    name: b"___DTOR_LIST__".to_vec(),
                },
                Command::End,
            ],
            scd_tail: Vec::new(),
        };
        let app = ObjectFile {
            commands: vec![
                Command::Header {
                    section: 0x01,
                    size: 6,
                    name: b"text".to_vec(),
                },
                Command::Opaque {
                    code: 0xe00c,
                    payload: Vec::new(),
                },
                Command::Opaque {
                    code: 0xe00d,
                    payload: Vec::new(),
                },
                Command::Opaque {
                    code: 0x4c01,
                    payload: vec![0x00, 0x00, 0x00, 0x02],
                },
                Command::Opaque {
                    code: 0x4d01,
                    payload: vec![0x00, 0x00, 0x00, 0x04],
                },
                Command::End,
            ],
            scd_tail: Vec::new(),
        };

        let mut sum0 = mk_summary(2, 2, 32);
        sum0.symbols.push(Symbol {
            name: b"___CTOR_LIST__".to_vec(),
            section: SectionKind::Data,
            value: 4,
        });
        sum0.symbols.push(Symbol {
            name: b"___DTOR_LIST__".to_vec(),
            section: SectionKind::Data,
            value: 16,
        });
        let sum1 = mk_summary(2, 6, 0);
        let layout = plan_layout(&[sum0.clone(), sum1.clone()]);
        let image = build_x_image_with_options(&[sys.clone(), app], &[sum0, sum1], &layout, false)
            .expect("x image");
        let data_pos = 64 + 8;
        // ctor table at data+4: -1, entry(text+2), 0
        assert_eq!(
            &image[data_pos + 4..data_pos + 16],
            &[0xff, 0xff, 0xff, 0xff, 0x00, 0x00, 0x00, 0x04, 0x00, 0x00, 0x00, 0x00]
        );
        // dtor table at data+16: -1, entry(text+4), 0
        assert_eq!(
            &image[data_pos + 16..data_pos + 28],
            &[0xff, 0xff, 0xff, 0xff, 0x00, 0x00, 0x00, 0x06, 0x00, 0x00, 0x00, 0x00]
        );
    }

    #[test]
    fn ctor_patch_requires_ctor_symbol() {
        let obj = ObjectFile {
            commands: vec![
                Command::Header {
                    section: 0x01,
                    size: 4,
                    name: b"text".to_vec(),
                },
                Command::Opaque {
                    code: 0x4c01,
                    payload: vec![0x00, 0x00, 0x00, 0x00],
                },
                Command::End,
            ],
            scd_tail: Vec::new(),
        };
        let sum = mk_summary(2, 4, 0);
        let layout = plan_layout(std::slice::from_ref(&sum));
        let err = build_x_image_with_options(&[obj], &[sum], &layout, false).expect_err("must fail");
        assert!(err.to_string().contains("ctor table symbol is missing"));
    }

    #[test]
    fn rejects_ctor_without_doctor_flag() {
        let obj = ObjectFile {
            commands: vec![
                Command::Header {
                    section: 0x01,
                    size: 4,
                    name: b"text".to_vec(),
                },
                Command::Opaque {
                    code: 0x4c01,
                    payload: vec![0x00, 0x00, 0x00, 0x00],
                },
                Command::End,
            ],
            scd_tail: Vec::new(),
        };
        let err = validate_link_inputs(&[obj], &[], &[mk_summary(2, 4, 0)]).expect_err("must reject");
        assert!(err.to_string().contains(".doctor なしで .ctor"));
    }

    #[test]
    fn rejects_dtor_without_dodtor_flag() {
        let obj = ObjectFile {
            commands: vec![
                Command::Header {
                    section: 0x01,
                    size: 4,
                    name: b"text".to_vec(),
                },
                Command::Opaque {
                    code: 0x4d01,
                    payload: vec![0x00, 0x00, 0x00, 0x00],
                },
                Command::End,
            ],
            scd_tail: Vec::new(),
        };
        let err = validate_link_inputs(&[obj], &[], &[mk_summary(2, 4, 0)]).expect_err("must reject");
        assert!(err.to_string().contains(".dodtor なしで .dtor"));
    }

    #[test]
    fn rejects_ctor_header_size_mismatch() {
        let obj = ObjectFile {
            commands: vec![
                Command::Header {
                    section: 0x01,
                    size: 4,
                    name: b"text".to_vec(),
                },
                Command::Header {
                    section: 0x0c,
                    size: 8, // should be 4 for one ctor
                    name: b"ctor".to_vec(),
                },
                Command::Opaque {
                    code: 0xe00c,
                    payload: Vec::new(),
                },
                Command::Opaque {
                    code: 0x4c01,
                    payload: vec![0, 0, 0, 0],
                },
                Command::End,
            ],
            scd_tail: Vec::new(),
        };
        let err = validate_link_inputs(&[obj], &[], &[mk_summary(2, 4, 0)]).expect_err("must reject");
        assert!(err.to_string().contains("ctor header size mismatch"));
    }

    #[test]
    fn rejects_dtor_header_size_mismatch() {
        let obj = ObjectFile {
            commands: vec![
                Command::Header {
                    section: 0x01,
                    size: 4,
                    name: b"text".to_vec(),
                },
                Command::Header {
                    section: 0x0d,
                    size: 0, // should be 4 for one dtor
                    name: b"dtor".to_vec(),
                },
                Command::Opaque {
                    code: 0xe00d,
                    payload: Vec::new(),
                },
                Command::Opaque {
                    code: 0x4d01,
                    payload: vec![0, 0, 0, 0],
                },
                Command::End,
            ],
            scd_tail: Vec::new(),
        };
        let err = validate_link_inputs(&[obj], &[], &[mk_summary(2, 4, 0)]).expect_err("must reject");
        assert!(err.to_string().contains("dtor header size mismatch"));
    }

    #[test]
    fn ctor_patch_rejects_table_overflow() {
        let obj0 = ObjectFile {
            commands: vec![
                Command::Header {
                    section: 0x01,
                    size: 0,
                    name: b"text".to_vec(),
                },
                Command::Header {
                    section: 0x02,
                    size: 8,
                    name: b"data".to_vec(),
                },
                Command::ChangeSection { section: 0x02 },
                Command::RawData(vec![0; 8]),
                Command::DefineSymbol {
                    section: 0x02,
                    value: 0,
                    name: b"___CTOR_LIST__".to_vec(),
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
                Command::Opaque {
                    code: 0x4c01,
                    payload: vec![0x00, 0x00, 0x00, 0x00],
                },
                Command::End,
            ],
            scd_tail: Vec::new(),
        };
        let mut sum0 = mk_summary(2, 0, 8);
        sum0.symbols.push(Symbol {
            name: b"___CTOR_LIST__".to_vec(),
            section: SectionKind::Data,
            value: 0,
        });
        let sum1 = mk_summary(2, 2, 0);
        let layout = plan_layout(&[sum0.clone(), sum1.clone()]);
        let err =
            build_x_image_with_options(&[obj0, obj1], &[sum0, sum1], &layout, false).expect_err("must fail");
        assert!(err.to_string().contains("ctor/dtor table overflows"));
    }

    #[test]
    fn ctor_patch_rejects_non_text_data_symbol_section() {
        let obj = ObjectFile {
            commands: vec![
                Command::Header {
                    section: 0x01,
                    size: 2,
                    name: b"text".to_vec(),
                },
                Command::Header {
                    section: 0x03,
                    size: 16,
                    name: b"bss".to_vec(),
                },
                Command::ChangeSection { section: 0x01 },
                Command::RawData(vec![0x4e, 0x75]),
                Command::Opaque {
                    code: 0x4c01,
                    payload: vec![0x00, 0x00, 0x00, 0x00],
                },
                Command::DefineSymbol {
                    section: 0x03,
                    value: 0,
                    name: b"___CTOR_LIST__".to_vec(),
                },
                Command::End,
            ],
            scd_tail: Vec::new(),
        };
        let mut sum = mk_summary(2, 2, 0);
        sum.declared_section_sizes.insert(SectionKind::Bss, 16);
        sum.symbols.push(Symbol {
            name: b"___CTOR_LIST__".to_vec(),
            section: SectionKind::Bss,
            value: 0,
        });
        let layout = plan_layout(std::slice::from_ref(&sum));
        let err = build_x_image_with_options(&[obj], &[sum], &layout, false).expect_err("must fail");
        assert!(err
            .to_string()
            .contains("ctor table symbol must be in text/data"));
    }

    #[test]
    fn accepts_doctor_dodtor_commands_as_noop() {
        for code in [0xe00c, 0xe00d] {
            let obj = ObjectFile {
                commands: vec![
                    Command::Header {
                        section: 0x01,
                        size: 0,
                        name: b"text".to_vec(),
                    },
                    Command::Opaque {
                        code,
                        payload: Vec::new(),
                    },
                    Command::End,
                ],
                scd_tail: Vec::new(),
            };
            validate_link_inputs(&[obj], &[], &[mk_summary(2, 0, 0)])
                .expect("doctor/dodtor should be accepted as no-op");
        }
    }

    #[test]
    fn accepts_ctor_dtor_section_headers_as_noop() {
        for section in [0x0c, 0x0d] {
            let obj = ObjectFile {
                commands: vec![
                    Command::Header {
                        section,
                        size: 0,
                        name: if section == 0x0c {
                            b"ctor".to_vec()
                        } else {
                            b"dtor".to_vec()
                        },
                    },
                    Command::End,
                ],
                scd_tail: Vec::new(),
            };
            validate_link_inputs(&[obj], &[], &[mk_summary(2, 0, 0)])
                .expect("ctor/dtor section header should be accepted as no-op");
        }
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

    #[test]
    fn a002_only_requires_stack_value() {
        let mut st = vec![ExprEntry { stat: 2, value: 123 }];
        let msgs = evaluate_a0(0x02, &mut st);
        assert!(msgs.is_empty());
        assert_eq!(st.len(), 1);
        assert_eq!(st[0].stat, 2);
        assert_eq!(st[0].value, 123);
    }

    #[test]
    fn wrt_stk_9200_reports_underflow() {
        let mut st = Vec::<ExprEntry>::new();
        let msgs = classify_expression_errors(
            0x9200,
            &Command::Opaque {
                code: 0x9200,
                payload: Vec::new(),
            },
            &mk_summary(2, 0, 0),
            &HashMap::new(),
            SectionKind::Text,
            &mut st,
        );
        assert_eq!(msgs, vec!["計算用スタックに値がありません"]);
    }

    #[test]
    fn wrt_stk_9100_reports_underflow() {
        let mut st = Vec::<ExprEntry>::new();
        let msgs = classify_expression_errors(
            0x9100,
            &Command::Opaque {
                code: 0x9100,
                payload: Vec::new(),
            },
            &mk_summary(2, 0, 0),
            &HashMap::new(),
            SectionKind::Text,
            &mut st,
        );
        assert_eq!(msgs, vec!["計算用スタックに値がありません"]);
    }

    #[test]
    fn wrt_stk_9300_reports_underflow() {
        let mut st = Vec::<ExprEntry>::new();
        let msgs = classify_expression_errors(
            0x9300,
            &Command::Opaque {
                code: 0x9300,
                payload: Vec::new(),
            },
            &mk_summary(2, 0, 0),
            &HashMap::new(),
            SectionKind::Text,
            &mut st,
        );
        assert_eq!(msgs, vec!["計算用スタックに値がありません"]);
    }

    #[test]
    fn wrt_stk_9600_reports_underflow() {
        let mut st = Vec::<ExprEntry>::new();
        let msgs = classify_expression_errors(
            0x9600,
            &Command::Opaque {
                code: 0x9600,
                payload: Vec::new(),
            },
            &mk_summary(2, 0, 0),
            &HashMap::new(),
            SectionKind::Text,
            &mut st,
        );
        assert_eq!(msgs, vec!["計算用スタックに値がありません"]);
    }

    #[test]
    fn wrt_stk_9900_reports_underflow() {
        let mut st = Vec::<ExprEntry>::new();
        let msgs = classify_expression_errors(
            0x9900,
            &Command::Opaque {
                code: 0x9900,
                payload: Vec::new(),
            },
            &mk_summary(2, 0, 0),
            &HashMap::new(),
            SectionKind::Text,
            &mut st,
        );
        assert_eq!(msgs, vec!["計算用スタックに値がありません"]);
    }

    #[test]
    fn wrt_stk_9a00_reports_underflow() {
        let mut st = Vec::<ExprEntry>::new();
        let msgs = classify_expression_errors(
            0x9a00,
            &Command::Opaque {
                code: 0x9a00,
                payload: Vec::new(),
            },
            &mk_summary(2, 0, 0),
            &HashMap::new(),
            SectionKind::Text,
            &mut st,
        );
        assert_eq!(msgs, vec!["計算用スタックに値がありません"]);
    }

    #[test]
    fn materializes_47ff_with_global_xref_value() {
        let obj0 = ObjectFile {
            commands: vec![
                Command::Header {
                    section: 0x01,
                    size: 2,
                    name: b"text".to_vec(),
                },
                Command::ChangeSection { section: 0x01 },
                Command::Opaque {
                    code: 0x47ff,
                    payload: vec![0x00, 0x01],
                },
                Command::End,
            ],
            scd_tail: Vec::new(),
        };
        let obj1 = ObjectFile {
            commands: vec![
                Command::Header {
                    section: 0x01,
                    size: 1,
                    name: b"text".to_vec(),
                },
                Command::ChangeSection { section: 0x01 },
                Command::RawData(vec![0xaa]),
                Command::End,
            ],
            scd_tail: Vec::new(),
        };
        let mut s0 = mk_summary(2, 2, 0);
        s0.xrefs.push(Symbol {
            name: b"_sym".to_vec(),
            section: SectionKind::Xref,
            value: 1,
        });
        let mut s1 = mk_summary(2, 1, 0);
        s1.symbols.push(Symbol {
            name: b"_sym".to_vec(),
            section: SectionKind::Text,
            value: 0,
        });
        let layout = plan_layout(&[s0.clone(), s1.clone()]);
        let image = build_x_image(&[obj0, obj1], &[s0, s1], &layout).expect("x image");
        // text payload: obj0(2 bytes) + obj1(1 byte). obj0 second byte remains zero.
        assert_eq!(&image[64..67], &[0x02, 0x00, 0xaa]);
    }

    #[test]
    fn materializes_6501_word_displacement() {
        let obj0 = ObjectFile {
            commands: vec![
                Command::Header {
                    section: 0x01,
                    size: 2,
                    name: b"text".to_vec(),
                },
                Command::ChangeSection { section: 0x01 },
                Command::Opaque {
                    code: 0x6501,
                    payload: vec![0x00, 0x00, 0x00, 0x00, 0x00, 0x01],
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
                Command::ChangeSection { section: 0x01 },
                Command::RawData(vec![0x4e, 0x71]),
                Command::End,
            ],
            scd_tail: Vec::new(),
        };
        let mut s0 = mk_summary(2, 2, 0);
        s0.xrefs.push(Symbol {
            name: b"_sym".to_vec(),
            section: SectionKind::Xref,
            value: 1,
        });
        let mut s1 = mk_summary(2, 2, 0);
        s1.symbols.push(Symbol {
            name: b"_sym".to_vec(),
            section: SectionKind::Text,
            value: 0,
        });
        let layout = plan_layout(&[s0.clone(), s1.clone()]);
        let image = build_x_image(&[obj0, obj1], &[s0, s1], &layout).expect("x image");
        // disp = sym(2) - (text base(0)+adr(0)) = 2
        assert_eq!(&image[64..66], &[0x00, 0x02]);
    }

    #[test]
    fn materializes_6b01_byte_displacement() {
        let obj0 = ObjectFile {
            commands: vec![
                Command::Header {
                    section: 0x01,
                    size: 1,
                    name: b"text".to_vec(),
                },
                Command::ChangeSection { section: 0x01 },
                Command::Opaque {
                    code: 0x6b01,
                    payload: vec![0x00, 0x00, 0x00, 0x00, 0x00, 0x01],
                },
                Command::End,
            ],
            scd_tail: Vec::new(),
        };
        let obj1 = ObjectFile {
            commands: vec![
                Command::Header {
                    section: 0x01,
                    size: 1,
                    name: b"text".to_vec(),
                },
                Command::ChangeSection { section: 0x01 },
                Command::RawData(vec![0xbb]),
                Command::End,
            ],
            scd_tail: Vec::new(),
        };
        let mut s0 = mk_summary(2, 1, 0);
        s0.xrefs.push(Symbol {
            name: b"_sym".to_vec(),
            section: SectionKind::Xref,
            value: 1,
        });
        let mut s1 = mk_summary(2, 1, 0);
        s1.symbols.push(Symbol {
            name: b"_sym".to_vec(),
            section: SectionKind::Text,
            value: 0,
        });
        let layout = plan_layout(&[s0.clone(), s1.clone()]);
        let image = build_x_image(&[obj0, obj1], &[s0, s1], &layout).expect("x image");
        assert_eq!(&image[64..65], &[0x02]); // disp = 2 (obj align=2)
    }

    #[test]
    fn materializes_55ff_word_with_offset() {
        let obj0 = ObjectFile {
            commands: vec![
                Command::Header {
                    section: 0x01,
                    size: 2,
                    name: b"text".to_vec(),
                },
                Command::ChangeSection { section: 0x01 },
                Command::Opaque {
                    code: 0x55ff,
                    payload: vec![0x00, 0x01, 0xff, 0xff, 0xff, 0xff],
                },
                Command::End,
            ],
            scd_tail: Vec::new(),
        };
        let obj1 = ObjectFile {
            commands: vec![
                Command::Header {
                    section: 0x01,
                    size: 1,
                    name: b"text".to_vec(),
                },
                Command::ChangeSection { section: 0x01 },
                Command::RawData(vec![0xcc]),
                Command::End,
            ],
            scd_tail: Vec::new(),
        };
        let mut s0 = mk_summary(2, 2, 0);
        s0.xrefs.push(Symbol {
            name: b"_sym".to_vec(),
            section: SectionKind::Xref,
            value: 1,
        });
        let mut s1 = mk_summary(2, 1, 0);
        s1.symbols.push(Symbol {
            name: b"_sym".to_vec(),
            section: SectionKind::Text,
            value: 0,
        });
        let layout = plan_layout(&[s0.clone(), s1.clone()]);
        let image = build_x_image(&[obj0, obj1], &[s0, s1], &layout).expect("x image");
        // sym addr 2 + (-1) => 1
        assert_eq!(&image[64..66], &[0x00, 0x01]);
    }

    #[test]
    fn materializes_6a01_long_displacement() {
        let obj0 = ObjectFile {
            commands: vec![
                Command::Header {
                    section: 0x01,
                    size: 4,
                    name: b"text".to_vec(),
                },
                Command::ChangeSection { section: 0x01 },
                Command::Opaque {
                    code: 0x6a01,
                    payload: vec![0x00, 0x00, 0x00, 0x00, 0x00, 0x01],
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
                Command::ChangeSection { section: 0x01 },
                Command::RawData(vec![0xde, 0xad]),
                Command::End,
            ],
            scd_tail: Vec::new(),
        };
        let mut s0 = mk_summary(2, 4, 0);
        s0.xrefs.push(Symbol {
            name: b"_sym".to_vec(),
            section: SectionKind::Xref,
            value: 1,
        });
        let mut s1 = mk_summary(2, 2, 0);
        s1.symbols.push(Symbol {
            name: b"_sym".to_vec(),
            section: SectionKind::Text,
            value: 0,
        });
        let layout = plan_layout(&[s0.clone(), s1.clone()]);
        let image = build_x_image(&[obj0, obj1], &[s0, s1], &layout).expect("x image");
        // sym(4) - adr(0) => 4
        assert_eq!(&image[64..68], &[0x00, 0x00, 0x00, 0x04]);
    }

    #[test]
    fn materializes_45ff_word_from_xref() {
        let obj0 = ObjectFile {
            commands: vec![
                Command::Header {
                    section: 0x01,
                    size: 2,
                    name: b"text".to_vec(),
                },
                Command::ChangeSection { section: 0x01 },
                Command::Opaque {
                    code: 0x45ff,
                    payload: vec![0x00, 0x01],
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
                Command::ChangeSection { section: 0x01 },
                Command::RawData(vec![0xfa, 0xce]),
                Command::End,
            ],
            scd_tail: Vec::new(),
        };
        let mut s0 = mk_summary(2, 2, 0);
        s0.xrefs.push(Symbol {
            name: b"_sym".to_vec(),
            section: SectionKind::Xref,
            value: 1,
        });
        let mut s1 = mk_summary(2, 2, 0);
        s1.symbols.push(Symbol {
            name: b"_sym".to_vec(),
            section: SectionKind::Text,
            value: 0,
        });
        let layout = plan_layout(&[s0.clone(), s1.clone()]);
        let image = build_x_image(&[obj0, obj1], &[s0, s1], &layout).expect("x image");
        assert_eq!(&image[64..66], &[0x00, 0x02]);
    }

    #[test]
    fn materializes_57ff_byte_with_offset() {
        let obj0 = ObjectFile {
            commands: vec![
                Command::Header {
                    section: 0x01,
                    size: 1,
                    name: b"text".to_vec(),
                },
                Command::ChangeSection { section: 0x01 },
                Command::Opaque {
                    code: 0x57ff,
                    payload: vec![0x00, 0x01, 0x00, 0x00, 0x00, 0x01],
                },
                Command::End,
            ],
            scd_tail: Vec::new(),
        };
        let obj1 = ObjectFile {
            commands: vec![
                Command::Header {
                    section: 0x01,
                    size: 1,
                    name: b"text".to_vec(),
                },
                Command::ChangeSection { section: 0x01 },
                Command::RawData(vec![0xab]),
                Command::End,
            ],
            scd_tail: Vec::new(),
        };
        let mut s0 = mk_summary(2, 1, 0);
        s0.xrefs.push(Symbol {
            name: b"_sym".to_vec(),
            section: SectionKind::Xref,
            value: 1,
        });
        let mut s1 = mk_summary(2, 1, 0);
        s1.symbols.push(Symbol {
            name: b"_sym".to_vec(),
            section: SectionKind::Text,
            value: 0,
        });
        let layout = plan_layout(&[s0.clone(), s1.clone()]);
        let image = build_x_image(&[obj0, obj1], &[s0, s1], &layout).expect("x image");
        // sym=2, +1 => 3
        assert_eq!(&image[64..65], &[0x03]);
    }

    #[test]
    fn materializes_6901_word_displacement_alias() {
        let obj0 = ObjectFile {
            commands: vec![
                Command::Header {
                    section: 0x01,
                    size: 2,
                    name: b"text".to_vec(),
                },
                Command::ChangeSection { section: 0x01 },
                Command::Opaque {
                    code: 0x6901,
                    payload: vec![0x00, 0x00, 0x00, 0x00, 0x00, 0x01],
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
                Command::ChangeSection { section: 0x01 },
                Command::RawData(vec![0x01, 0x02]),
                Command::End,
            ],
            scd_tail: Vec::new(),
        };
        let mut s0 = mk_summary(2, 2, 0);
        s0.xrefs.push(Symbol {
            name: b"_sym".to_vec(),
            section: SectionKind::Xref,
            value: 1,
        });
        let mut s1 = mk_summary(2, 2, 0);
        s1.symbols.push(Symbol {
            name: b"_sym".to_vec(),
            section: SectionKind::Text,
            value: 0,
        });
        let layout = plan_layout(&[s0.clone(), s1.clone()]);
        let image = build_x_image(&[obj0, obj1], &[s0, s1], &layout).expect("x image");
        assert_eq!(&image[64..66], &[0x00, 0x02]);
    }

    #[test]
    fn materializes_4605_with_rdata_section_base() {
        let obj = ObjectFile {
            commands: vec![
                Command::Header {
                    section: 0x01,
                    size: 4,
                    name: b"text".to_vec(),
                },
                Command::Header {
                    section: 0x05,
                    size: 4,
                    name: b"rdata".to_vec(),
                },
                Command::ChangeSection { section: 0x01 },
                Command::Opaque {
                    code: 0x4605,
                    payload: vec![0x00, 0x00, 0x00, 0x03],
                },
                Command::ChangeSection { section: 0x05 },
                Command::RawData(vec![0x11, 0x22, 0x33, 0x44]),
                Command::End,
            ],
            scd_tail: Vec::new(),
        };
        let sum = mk_summary(2, 4, 0);
        let layout = plan_layout(std::slice::from_ref(&sum));
        let image = build_x_image(&[obj], &[sum], &layout).expect("x image");
        // lo=05 => RDATA placement(0) + adr(3)
        assert_eq!(&image[64..68], &[0x00, 0x00, 0x00, 0x03]);
    }

    #[test]
    fn materializes_5608_with_rldata_section_base_and_offset() {
        let obj = ObjectFile {
            commands: vec![
                Command::Header {
                    section: 0x01,
                    size: 4,
                    name: b"text".to_vec(),
                },
                Command::Header {
                    section: 0x08,
                    size: 8,
                    name: b"rldata".to_vec(),
                },
                Command::ChangeSection { section: 0x01 },
                Command::Opaque {
                    code: 0x5608,
                    payload: vec![0x00, 0x00, 0x00, 0x04, 0x00, 0x00, 0x00, 0x02],
                },
                Command::ChangeSection { section: 0x08 },
                Command::RawData(vec![0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff, 0x00, 0x11]),
                Command::End,
            ],
            scd_tail: Vec::new(),
        };
        let sum = mk_summary(2, 4, 0);
        let layout = plan_layout(std::slice::from_ref(&sum));
        let image = build_x_image(&[obj], &[sum], &layout).expect("x image");
        // lo=08 => RLDATA placement(0) + adr(4) + off(2) = 6
        assert_eq!(&image[64..68], &[0x00, 0x00, 0x00, 0x06]);
    }

    #[test]
    fn materializes_5605_with_rdata_section_base_and_offset() {
        let obj = ObjectFile {
            commands: vec![
                Command::Header {
                    section: 0x01,
                    size: 4,
                    name: b"text".to_vec(),
                },
                Command::ChangeSection { section: 0x01 },
                Command::Opaque {
                    code: 0x5605,
                    payload: vec![0x00, 0x00, 0x00, 0x03, 0x00, 0x00, 0x00, 0x04],
                },
                Command::End,
            ],
            scd_tail: Vec::new(),
        };
        let sum = mk_summary(2, 4, 0);
        let layout = plan_layout(std::slice::from_ref(&sum));
        let image = build_x_image(&[obj], &[sum], &layout).expect("x image");
        assert_eq!(&image[64..68], &[0x00, 0x00, 0x00, 0x07]);
    }

    #[test]
    fn materializes_5606_with_rbss_section_base_and_offset() {
        let obj = ObjectFile {
            commands: vec![
                Command::Header {
                    section: 0x01,
                    size: 4,
                    name: b"text".to_vec(),
                },
                Command::ChangeSection { section: 0x01 },
                Command::Opaque {
                    code: 0x5606,
                    payload: vec![0x00, 0x00, 0x00, 0x03, 0x00, 0x00, 0x00, 0x05],
                },
                Command::End,
            ],
            scd_tail: Vec::new(),
        };
        let sum = mk_summary(2, 4, 0);
        let layout = plan_layout(std::slice::from_ref(&sum));
        let image = build_x_image(&[obj], &[sum], &layout).expect("x image");
        assert_eq!(&image[64..68], &[0x00, 0x00, 0x00, 0x08]);
    }

    #[test]
    fn materializes_5607_with_rstack_section_base_and_offset() {
        let obj = ObjectFile {
            commands: vec![
                Command::Header {
                    section: 0x01,
                    size: 4,
                    name: b"text".to_vec(),
                },
                Command::ChangeSection { section: 0x01 },
                Command::Opaque {
                    code: 0x5607,
                    payload: vec![0x00, 0x00, 0x00, 0x03, 0x00, 0x00, 0x00, 0x06],
                },
                Command::End,
            ],
            scd_tail: Vec::new(),
        };
        let sum = mk_summary(2, 4, 0);
        let layout = plan_layout(std::slice::from_ref(&sum));
        let image = build_x_image(&[obj], &[sum], &layout).expect("x image");
        assert_eq!(&image[64..68], &[0x00, 0x00, 0x00, 0x09]);
    }

    #[test]
    fn materializes_5609_with_rlbss_section_base_and_offset() {
        let obj = ObjectFile {
            commands: vec![
                Command::Header {
                    section: 0x01,
                    size: 4,
                    name: b"text".to_vec(),
                },
                Command::ChangeSection { section: 0x01 },
                Command::Opaque {
                    code: 0x5609,
                    payload: vec![0x00, 0x00, 0x00, 0x03, 0x00, 0x00, 0x00, 0x07],
                },
                Command::End,
            ],
            scd_tail: Vec::new(),
        };
        let sum = mk_summary(2, 4, 0);
        let layout = plan_layout(std::slice::from_ref(&sum));
        let image = build_x_image(&[obj], &[sum], &layout).expect("x image");
        assert_eq!(&image[64..68], &[0x00, 0x00, 0x00, 0x0a]);
    }

    #[test]
    fn materializes_560a_with_rlstack_section_base_and_offset() {
        let obj = ObjectFile {
            commands: vec![
                Command::Header {
                    section: 0x01,
                    size: 4,
                    name: b"text".to_vec(),
                },
                Command::ChangeSection { section: 0x01 },
                Command::Opaque {
                    code: 0x560a,
                    payload: vec![0x00, 0x00, 0x00, 0x03, 0x00, 0x00, 0x00, 0x08],
                },
                Command::End,
            ],
            scd_tail: Vec::new(),
        };
        let sum = mk_summary(2, 4, 0);
        let layout = plan_layout(std::slice::from_ref(&sum));
        let image = build_x_image(&[obj], &[sum], &layout).expect("x image");
        assert_eq!(&image[64..68], &[0x00, 0x00, 0x00, 0x0b]);
    }

    #[test]
    fn a0_const_binary_ops_match_expected_values() {
        let cases: &[(u8, i32, i32, i32)] = &[
            (0x09, 6, 7, 42),
            (0x0c, 16, 2, 4),
            (0x0d, 3, 4, 48),
            (0x0e, -8, 1, -4),
            (0x11, 5, 5, -1),
            (0x12, 5, 6, -1),
            (0x13, -1, 1, 0),
            (0x14, 1, 1, -1),
            (0x15, 2, 1, -1),
            (0x16, 2, 2, -1),
            (0x17, -2, 1, -1),
            (0x18, 1, 1, -1),
            (0x19, 3, 2, -1),
            (0x1a, 2, 2, -1),
            (0x1b, 0b1100, 0b1010, 0b1000),
            (0x1c, 0b1100, 0b1010, 0b0110),
            (0x1d, 0b1100, 0b1010, 0b1110),
        ];
        for (op, b, a, want) in cases {
            let mut st = vec![
                ExprEntry { stat: 0, value: *b },
                ExprEntry { stat: 0, value: *a },
            ];
            let msgs = evaluate_a0(*op, &mut st);
            assert!(msgs.is_empty(), "op={op:02x}");
            assert_eq!(st.len(), 1, "op={op:02x}");
            assert_eq!(st[0].stat, 0, "op={op:02x}");
            assert_eq!(st[0].value, *want, "op={op:02x}");
        }
    }

    #[test]
    fn a00a_and_a00b_division_semantics() {
        let mut div_st = vec![ExprEntry { stat: 0, value: 7 }, ExprEntry { stat: 0, value: 3 }];
        assert!(evaluate_a0(0x0a, &mut div_st).is_empty());
        assert_eq!(div_st[0].value, 2);

        let mut mod_st = vec![ExprEntry { stat: 0, value: -7 }, ExprEntry { stat: 0, value: 3 }];
        assert!(evaluate_a0(0x0b, &mut mod_st).is_empty());
        assert_eq!(mod_st[0].value, 1); // abs(remainder)
    }

    #[test]
    fn materializes_4606_with_rbss_section_base() {
        let obj = ObjectFile {
            commands: vec![
                Command::Header {
                    section: 0x01,
                    size: 4,
                    name: b"text".to_vec(),
                },
                Command::ChangeSection { section: 0x01 },
                Command::Opaque {
                    code: 0x4606,
                    payload: vec![0x00, 0x00, 0x00, 0x09],
                },
                Command::End,
            ],
            scd_tail: Vec::new(),
        };
        let sum = mk_summary(2, 4, 0);
        let layout = plan_layout(std::slice::from_ref(&sum));
        let image = build_x_image(&[obj], &[sum], &layout).expect("x image");
        assert_eq!(&image[64..68], &[0x00, 0x00, 0x00, 0x09]);
    }

    #[test]
    fn materializes_4607_with_rstack_section_base() {
        let obj = ObjectFile {
            commands: vec![
                Command::Header {
                    section: 0x01,
                    size: 4,
                    name: b"text".to_vec(),
                },
                Command::ChangeSection { section: 0x01 },
                Command::Opaque {
                    code: 0x4607,
                    payload: vec![0x00, 0x00, 0x00, 0x0a],
                },
                Command::End,
            ],
            scd_tail: Vec::new(),
        };
        let sum = mk_summary(2, 4, 0);
        let layout = plan_layout(std::slice::from_ref(&sum));
        let image = build_x_image(&[obj], &[sum], &layout).expect("x image");
        assert_eq!(&image[64..68], &[0x00, 0x00, 0x00, 0x0a]);
    }

    #[test]
    fn materializes_4609_with_rlbss_section_base() {
        let obj = ObjectFile {
            commands: vec![
                Command::Header {
                    section: 0x01,
                    size: 4,
                    name: b"text".to_vec(),
                },
                Command::ChangeSection { section: 0x01 },
                Command::Opaque {
                    code: 0x4609,
                    payload: vec![0x00, 0x00, 0x00, 0x0b],
                },
                Command::End,
            ],
            scd_tail: Vec::new(),
        };
        let sum = mk_summary(2, 4, 0);
        let layout = plan_layout(std::slice::from_ref(&sum));
        let image = build_x_image(&[obj], &[sum], &layout).expect("x image");
        assert_eq!(&image[64..68], &[0x00, 0x00, 0x00, 0x0b]);
    }

    #[test]
    fn materializes_460a_with_rlstack_section_base() {
        let obj = ObjectFile {
            commands: vec![
                Command::Header {
                    section: 0x01,
                    size: 4,
                    name: b"text".to_vec(),
                },
                Command::ChangeSection { section: 0x01 },
                Command::Opaque {
                    code: 0x460a,
                    payload: vec![0x00, 0x00, 0x00, 0x0c],
                },
                Command::End,
            ],
            scd_tail: Vec::new(),
        };
        let sum = mk_summary(2, 4, 0);
        let layout = plan_layout(std::slice::from_ref(&sum));
        let image = build_x_image(&[obj], &[sum], &layout).expect("x image");
        assert_eq!(&image[64..68], &[0x00, 0x00, 0x00, 0x0c]);
    }

    #[test]
    fn materializes_6b08_byte_displacement_rldata() {
        let obj0 = ObjectFile {
            commands: vec![
                Command::Header {
                    section: 0x01,
                    size: 1,
                    name: b"text".to_vec(),
                },
                Command::ChangeSection { section: 0x01 },
                Command::Opaque {
                    code: 0x6b08,
                    payload: vec![0x00, 0x00, 0x00, 0x00, 0x00, 0x01],
                },
                Command::End,
            ],
            scd_tail: Vec::new(),
        };
        let obj1 = ObjectFile {
            commands: vec![
                Command::Header {
                    section: 0x01,
                    size: 1,
                    name: b"text".to_vec(),
                },
                Command::ChangeSection { section: 0x01 },
                Command::RawData(vec![0x5a]),
                Command::End,
            ],
            scd_tail: Vec::new(),
        };
        let mut s0 = mk_summary(2, 1, 0);
        s0.xrefs.push(Symbol {
            name: b"_sym".to_vec(),
            section: SectionKind::Xref,
            value: 1,
        });
        let mut s1 = mk_summary(2, 1, 0);
        s1.symbols.push(Symbol {
            name: b"_sym".to_vec(),
            section: SectionKind::Text,
            value: 0,
        });
        let layout = plan_layout(&[s0.clone(), s1.clone()]);
        let image = build_x_image(&[obj0, obj1], &[s0, s1], &layout).expect("x image");
        // lo=08 branch is exercised; with no RLData placement, base=0 and sym(text)=2.
        assert_eq!(&image[64..65], &[0x02]);
    }

    fn assert_6b_r_section_displacement(lo: u8) {
        let obj0 = ObjectFile {
            commands: vec![
                Command::Header {
                    section: 0x01,
                    size: 1,
                    name: b"text".to_vec(),
                },
                Command::ChangeSection { section: 0x01 },
                Command::Opaque {
                    code: 0x6b00 | lo as u16,
                    payload: vec![0x00, 0x00, 0x00, 0x00, 0x00, 0x01],
                },
                Command::End,
            ],
            scd_tail: Vec::new(),
        };
        let obj1 = ObjectFile {
            commands: vec![
                Command::Header {
                    section: 0x01,
                    size: 1,
                    name: b"text".to_vec(),
                },
                Command::ChangeSection { section: 0x01 },
                Command::RawData(vec![0x5a]),
                Command::End,
            ],
            scd_tail: Vec::new(),
        };
        let mut s0 = mk_summary(2, 1, 0);
        s0.xrefs.push(Symbol {
            name: b"_sym".to_vec(),
            section: SectionKind::Xref,
            value: 1,
        });
        let mut s1 = mk_summary(2, 1, 0);
        s1.symbols.push(Symbol {
            name: b"_sym".to_vec(),
            section: SectionKind::Text,
            value: 0,
        });
        let layout = plan_layout(&[s0.clone(), s1.clone()]);
        let image = build_x_image(&[obj0, obj1], &[s0, s1], &layout).expect("x image");
        assert_eq!(&image[64..65], &[0x02]); // sym(text)=2, no r-section placement offset
    }

    #[test]
    fn materializes_6b05_byte_displacement_rdata() {
        assert_6b_r_section_displacement(0x05);
    }

    #[test]
    fn materializes_6b02_byte_displacement_data() {
        assert_6b_r_section_displacement(0x02);
    }

    #[test]
    fn materializes_6b03_byte_displacement_bss() {
        assert_6b_r_section_displacement(0x03);
    }

    #[test]
    fn materializes_6b04_byte_displacement_stack() {
        assert_6b_r_section_displacement(0x04);
    }

    #[test]
    fn materializes_6b06_byte_displacement_rbss() {
        assert_6b_r_section_displacement(0x06);
    }

    #[test]
    fn materializes_6b07_byte_displacement_rstack() {
        assert_6b_r_section_displacement(0x07);
    }

    #[test]
    fn materializes_6b09_byte_displacement_rlbss() {
        assert_6b_r_section_displacement(0x09);
    }

    #[test]
    fn materializes_6b0a_byte_displacement_rlstack() {
        assert_6b_r_section_displacement(0x0a);
    }

    #[test]
    fn materializes_9200_from_calc_stack_value() {
        let obj = ObjectFile {
            commands: vec![
                Command::Header {
                    section: 0x01,
                    size: 4,
                    name: b"text".to_vec(),
                },
                Command::ChangeSection { section: 0x01 },
                Command::Opaque {
                    code: 0x8000,
                    payload: vec![0x12, 0x34, 0x56, 0x78],
                },
                Command::Opaque {
                    code: 0x9200,
                    payload: Vec::new(),
                },
                Command::End,
            ],
            scd_tail: Vec::new(),
        };
        let sum = mk_summary(2, 4, 0);
        let layout = plan_layout(std::slice::from_ref(&sum));
        let image = build_x_image(&[obj], &[sum], &layout).expect("x image");
        assert_eq!(&image[64..68], &[0x12, 0x34, 0x56, 0x78]);
    }

    #[test]
    fn materializes_9000_from_calc_stack_value() {
        let obj = ObjectFile {
            commands: vec![
                Command::Header {
                    section: 0x01,
                    size: 2,
                    name: b"text".to_vec(),
                },
                Command::ChangeSection { section: 0x01 },
                Command::Opaque {
                    code: 0x8000,
                    payload: vec![0x00, 0x00, 0x00, 0x7f],
                },
                Command::Opaque {
                    code: 0x9000,
                    payload: Vec::new(),
                },
                Command::End,
            ],
            scd_tail: Vec::new(),
        };
        let sum = mk_summary(2, 2, 0);
        let layout = plan_layout(std::slice::from_ref(&sum));
        let image = build_x_image(&[obj], &[sum], &layout).expect("x image");
        assert_eq!(&image[64..66], &[0x00, 0x7f]);
    }

    #[test]
    fn materializes_9100_from_calc_stack_value() {
        let obj = ObjectFile {
            commands: vec![
                Command::Header {
                    section: 0x01,
                    size: 2,
                    name: b"text".to_vec(),
                },
                Command::ChangeSection { section: 0x01 },
                Command::Opaque {
                    code: 0x8000,
                    payload: vec![0x00, 0x00, 0x12, 0x34],
                },
                Command::Opaque {
                    code: 0x9100,
                    payload: Vec::new(),
                },
                Command::End,
            ],
            scd_tail: Vec::new(),
        };
        let sum = mk_summary(2, 2, 0);
        let layout = plan_layout(std::slice::from_ref(&sum));
        let image = build_x_image(&[obj], &[sum], &layout).expect("x image");
        assert_eq!(&image[64..66], &[0x12, 0x34]);
    }

    #[test]
    fn materializes_9300_from_calc_stack_value() {
        let obj = ObjectFile {
            commands: vec![
                Command::Header {
                    section: 0x01,
                    size: 1,
                    name: b"text".to_vec(),
                },
                Command::ChangeSection { section: 0x01 },
                Command::Opaque {
                    code: 0x8000,
                    payload: vec![0x00, 0x00, 0x00, 0x7f],
                },
                Command::Opaque {
                    code: 0x9300,
                    payload: Vec::new(),
                },
                Command::End,
            ],
            scd_tail: Vec::new(),
        };
        let sum = mk_summary(2, 1, 0);
        let layout = plan_layout(std::slice::from_ref(&sum));
        let image = build_x_image(&[obj], &[sum], &layout).expect("x image");
        assert_eq!(&image[64..65], &[0x7f]);
    }

    #[test]
    fn materializes_9900_from_calc_stack_value() {
        let obj = ObjectFile {
            commands: vec![
                Command::Header {
                    section: 0x01,
                    size: 2,
                    name: b"text".to_vec(),
                },
                Command::ChangeSection { section: 0x01 },
                Command::Opaque {
                    code: 0x8000,
                    payload: vec![0x00, 0x00, 0x12, 0x34],
                },
                Command::Opaque {
                    code: 0x9900,
                    payload: Vec::new(),
                },
                Command::End,
            ],
            scd_tail: Vec::new(),
        };
        let sum = mk_summary(2, 2, 0);
        let layout = plan_layout(std::slice::from_ref(&sum));
        let image = build_x_image(&[obj], &[sum], &layout).expect("x image");
        assert_eq!(&image[64..66], &[0x12, 0x34]);
    }

    fn assert_label_displacement_for_lo(code_hi: u8, lo: u8, expected: &[u8]) {
        let obj0 = ObjectFile {
            commands: vec![
                Command::Header {
                    section: 0x01,
                    size: expected.len() as u32,
                    name: b"text".to_vec(),
                },
                Command::ChangeSection { section: 0x01 },
                Command::Opaque {
                    code: ((code_hi as u16) << 8) | lo as u16,
                    payload: vec![0x00, 0x00, 0x00, 0x00, 0x00, 0x01],
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
                Command::ChangeSection { section: 0x01 },
                Command::RawData(vec![0x5a, 0x5b]),
                Command::End,
            ],
            scd_tail: Vec::new(),
        };
        let mut s0 = mk_summary(2, expected.len() as u32, 0);
        s0.xrefs.push(Symbol {
            name: b"_sym".to_vec(),
            section: SectionKind::Xref,
            value: 1,
        });
        let mut s1 = mk_summary(2, 2, 0);
        s1.symbols.push(Symbol {
            name: b"_sym".to_vec(),
            section: SectionKind::Text,
            value: 0,
        });
        let layout = plan_layout(&[s0.clone(), s1.clone()]);
        let image = build_x_image(&[obj0, obj1], &[s0, s1], &layout).expect("x image");
        assert_eq!(&image[64..64 + expected.len()], expected);
    }

    #[test]
    fn materializes_6502_word_displacement_data() {
        assert_label_displacement_for_lo(0x65, 0x02, &[0x00, 0x02]);
    }

    #[test]
    fn materializes_6503_word_displacement_bss() {
        assert_label_displacement_for_lo(0x65, 0x03, &[0x00, 0x02]);
    }

    #[test]
    fn materializes_6504_word_displacement_stack() {
        assert_label_displacement_for_lo(0x65, 0x04, &[0x00, 0x02]);
    }

    #[test]
    fn materializes_6902_word_displacement_alias_data() {
        assert_label_displacement_for_lo(0x69, 0x02, &[0x00, 0x02]);
    }

    #[test]
    fn materializes_6903_word_displacement_alias_bss() {
        assert_label_displacement_for_lo(0x69, 0x03, &[0x00, 0x02]);
    }

    #[test]
    fn materializes_6904_word_displacement_alias_stack() {
        assert_label_displacement_for_lo(0x69, 0x04, &[0x00, 0x02]);
    }

    #[test]
    fn materializes_6a02_long_displacement_data() {
        assert_label_displacement_for_lo(0x6a, 0x02, &[0x00, 0x00, 0x00, 0x04]);
    }

    #[test]
    fn materializes_6a03_long_displacement_bss() {
        assert_label_displacement_for_lo(0x6a, 0x03, &[0x00, 0x00, 0x00, 0x04]);
    }

    #[test]
    fn materializes_6a04_long_displacement_stack() {
        assert_label_displacement_for_lo(0x6a, 0x04, &[0x00, 0x00, 0x00, 0x04]);
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
