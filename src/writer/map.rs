use std::collections::HashMap;
use std::fmt::Write as _;
use std::path::Path;

use anyhow::{Context, Result};

use crate::layout::LayoutPlan;
use crate::resolver::{ObjectSummary, SectionKind};

/// Writes a CRLF-normalized map text file.
///
/// # Errors
/// Returns an error when writing `output_path` fails.
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

#[allow(clippy::too_many_arguments)]
pub(crate) fn build_map_text(
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
        .saturating_add(super::section_total(layout, SectionKind::Stack));
    let exec = super::resolve_exec_address(summaries, text_size, data_size, bss_size)
        .ok()
        .flatten()
        .unwrap_or(0);
    let mut out = String::new();
    out.push_str("==========================================================\n");
    out.push_str(&super::to_human68k_path(Path::new(exec_output_path)));
    out.push('\n');
    out.push_str("==========================================================\n");
    out.push_str(&format_exec_line(exec));
    let text_sz = super::section_total(layout, SectionKind::Text);
    let data_sz = super::section_total(layout, SectionKind::Data);
    let bss_sz = super::section_total(layout, SectionKind::Bss);
    let common_sz = super::section_total(layout, SectionKind::Common);
    let stack_sz = super::section_total(layout, SectionKind::Stack);
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
        let sz = super::section_total(layout, kind);
        out.push_str(&format_section_line(name, rcur, sz));
        rcur = rcur.saturating_add(sz);
    }

    let def_owner = build_definition_owner_map(summaries, input_paths);
    for (idx, summary) in summaries.iter().enumerate() {
        out.push_str("\n\n");
        out.push_str("==========================================================\n");
        let _ = writeln!(out, "{}", display_obj_name(input_paths.get(idx), idx));
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
                let _ = writeln!(out, "{n:<24} : in {owner}");
            }
        }
        if !summary.symbols.is_empty() {
            out.push_str("-------------------------- xdef --------------------------\n");
            let mut syms = summary.symbols.iter().collect::<Vec<_>>();
            syms.sort_by(|a, b| a.name.cmp(&b.name).then(a.value.cmp(&b.value)));
            for sym in syms {
                let n = String::from_utf8_lossy(&sym.name);
                out.push_str(&format_symbol_line(
                    &n,
                    sym.value,
                    super::section_tag(sym.section),
                ));
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
            .map_or_else(|| p.clone(), |v| v.to_string_lossy().to_string());
    }
    format!("obj{idx}")
}

fn format_symbol_line(name: &str, addr: u32, sect: &str) -> String {
    let mut out = format_label_prefix(name);
    let _ = writeln!(out, "{addr:08x} ({sect:<7})");
    out
}

fn format_exec_line(exec: u32) -> String {
    let mut out = format_label_prefix("exec");
    let _ = writeln!(out, "{exec:08x}");
    out
}

fn format_align_line(align: u32) -> String {
    let mut out = format_label_prefix("align");
    let _ = writeln!(out, "{align:08x}");
    out
}

fn format_section_line(name: &str, pos: u32, size: u32) -> String {
    let mut label = format_label_prefix(name);
    if size == 0 {
        label.push('\n');
        return label;
    }
    let end = pos.saturating_add(size).saturating_sub(1);
    let _ = writeln!(label, "{pos:08x} - {end:08x} ({size:08x})");
    label
}

fn format_label_prefix(name: &str) -> String {
    let tabs = if name.len() < 8 {
        3
    } else if name.len() < 16 {
        2
    } else {
        1
    };
    format!("{name}{} : ", "\t".repeat(tabs))
}
