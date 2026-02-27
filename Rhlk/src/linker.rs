use crate::cli::Args;
use crate::format::FormatError;
use crate::format::obj::{Command, ObjectFile, parse_object};
use crate::layout::plan_layout;
use crate::resolver::resolve_object;
use crate::resolver::ObjectSummary;
use crate::writer::{write_map, write_output};
use std::env;
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};

pub fn run(args: Args) -> anyhow::Result<()> {
    if args.title {
        println!("rhlk {}", env!("CARGO_PKG_VERSION"));
    }
    if let Some(align) = args.align {
        if !(2..=256).contains(&align) || !align.is_power_of_two() {
            anyhow::bail!("align size must be power of two in [2, 256]: {align}");
        }
    }

    let g2lk_mode = if args.g2lk_off {
        false
    } else if args.g2lk_on {
        true
    } else {
        true
    };

    let mut expanded_inputs = args.inputs.clone();
    for indirect in &args.indirect_files {
        expanded_inputs.extend(load_indirect_inputs(indirect)?);
    }
    expanded_inputs.extend(resolve_lib_inputs(&args)?);
    if expanded_inputs.is_empty() {
        anyhow::bail!("no input files")
    }

    let (objects, summaries, input_names) = load_objects_with_requests(&expanded_inputs, args.verbose)?;
    let mut objects = objects;
    let mut summaries = summaries;
    let mut input_names = input_names;
    if !args.defines.is_empty() {
        inject_define_symbols(&args, &mut objects, &mut summaries, &mut input_names);
    }
    if args.section_info {
        inject_section_info_object(&mut objects, &mut summaries, &mut input_names);
    }
    if let Some(align) = args.align {
        for s in &mut summaries {
            // Apply global default align only to objects that still have the default value.
            if s.object_align == 2 {
                s.object_align = align;
            }
        }
    }
    validate_unresolved_symbols(&summaries, &input_names)?;

    let mut start_seen = false;
    for (idx, summary) in summaries.iter().enumerate() {
        if summary.start_address.is_none() {
            continue;
        }
        if start_seen {
            let name = Path::new(&input_names[idx])
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or(&input_names[idx]);
            anyhow::bail!("複数の実行開始アドレスを指定することはできません in {name}");
        }
        start_seen = true;
    }

    let layout = plan_layout(&summaries);
    if args.section_info {
        update_section_info_rsize(&mut summaries, &layout);
    }
    if args.verbose {
        println!("layout totals:");
        for (section, size) in &layout.total_size_by_section {
            println!("  {section:?}: {size}");
        }
        println!(
            "layout diagnostics: common_conflicts={} common_warnings={}",
            layout.diagnostics.common_conflicts, layout.diagnostics.common_warnings
        );
    }

    let effective_r = args.r_format || args.make_mcs;
    let output = resolve_output_path(&args, &expanded_inputs);
    write_output(
        &output,
        effective_r,
        args.r_no_check,
        args.omit_bss,
        args.make_mcs,
        args.cut_symbols,
        args.base_address.unwrap_or(0),
        args.load_mode.unwrap_or(0),
        args.section_info,
        g2lk_mode,
        &objects,
        &input_names,
        &summaries,
        &layout,
    )?;
    if args.verbose {
        println!("wrote output: {output}");
    }
    if let Some(map_output) = resolve_map_output(&args.map, &Some(output.clone()), &expanded_inputs) {
        write_map(&output, &map_output, &summaries, &layout, &input_names)?;
        if args.verbose {
            println!("wrote map: {map_output}");
        }
    }

    if args.verbose {
        println!("rhlk: parsed {} input file(s)", input_names.len());
    }
    Ok(())
}

fn load_indirect_inputs(path: &str) -> anyhow::Result<Vec<String>> {
    let text = std::fs::read_to_string(path)
        .map_err(|_| anyhow::anyhow!("ファイルがありません: {}", path))?;
    Ok(text
        .split_whitespace()
        .map(|s| s.to_string())
        .collect::<Vec<_>>())
}

fn resolve_lib_inputs(args: &Args) -> anyhow::Result<Vec<String>> {
    let mut out = Vec::<String>::new();
    if args.libs.is_empty() && !args.use_env_lib {
        return Ok(out);
    }
    let mut search_paths = Vec::<PathBuf>::new();
    for p in &args.lib_paths {
        search_paths.push(PathBuf::from(p));
    }
    if args.use_env_lib {
        if let Some(v) = env::var_os("LIB") {
            search_paths.extend(env::split_paths(&v));
        }
    }
    for lib in &args.libs {
        let file = format!("lib{lib}.a");
        let mut resolved = None::<PathBuf>;
        for dir in &search_paths {
            let c = dir.join(&file);
            if c.exists() {
                resolved = Some(c);
                break;
            }
        }
        if resolved.is_none() {
            let c = PathBuf::from(&file);
            if c.exists() {
                resolved = Some(c);
            }
        }
        let Some(path) = resolved else {
            anyhow::bail!("ファイルがありません: {file}");
        };
        out.push(path.to_string_lossy().to_string());
    }
    Ok(out)
}

fn inject_define_symbols(
    args: &Args,
    objects: &mut Vec<ObjectFile>,
    summaries: &mut Vec<ObjectSummary>,
    input_names: &mut Vec<String>,
) {
    let mut commands = Vec::new();
    for def in &args.defines {
        commands.push(Command::DefineSymbol {
            section: 0x00,
            value: def.value,
            name: def.name.as_bytes().to_vec(),
        });
    }
    commands.push(Command::End);
    let obj = ObjectFile {
        commands,
        scd_tail: Vec::new(),
    };
    let sum = resolve_object(&obj);
    objects.push(obj);
    summaries.push(sum);
    input_names.push("*DEFINE*".to_string());
}

fn inject_section_info_object(
    objects: &mut Vec<ObjectFile>,
    summaries: &mut Vec<ObjectSummary>,
    input_names: &mut Vec<String>,
) {
    const SYS_INFO_LEN: u32 = 0x40;
    let commands = vec![
        Command::SourceFile {
            size: 0,
            name: b"*SYSTEM*".to_vec(),
        },
        Command::Header {
            section: 0x01,
            size: 0,
            name: b"text".to_vec(),
        },
        Command::Header {
            section: 0x02,
            size: 0,
            name: b"data".to_vec(),
        },
        Command::Header {
            section: 0x03,
            size: 0,
            name: b"bss".to_vec(),
        },
        Command::Header {
            section: 0x04,
            size: 0,
            name: b"stack".to_vec(),
        },
        Command::Header {
            section: 0x05,
            size: 0,
            name: b"rdata".to_vec(),
        },
        Command::Header {
            section: 0x06,
            size: 0,
            name: b"rbss".to_vec(),
        },
        Command::Header {
            section: 0x07,
            size: 0,
            name: b"rstack".to_vec(),
        },
        Command::Header {
            section: 0x08,
            size: 0,
            name: b"rldata".to_vec(),
        },
        Command::Header {
            section: 0x09,
            size: 0,
            name: b"rlbss".to_vec(),
        },
        Command::Header {
            section: 0x0a,
            size: 0,
            name: b"rlstack".to_vec(),
        },
        Command::DefineSymbol {
            section: 0x02,
            value: 0,
            name: b"___size_info".to_vec(),
        },
        Command::DefineSymbol {
            section: 0x00,
            value: 0,
            name: b"___rsize".to_vec(),
        },
        Command::ChangeSection { section: 0x02 },
        Command::DefineSpace { size: SYS_INFO_LEN },
        Command::End,
    ];
    let obj = ObjectFile {
        commands,
        scd_tail: Vec::new(),
    };
    let sum = resolve_object(&obj);
    objects.insert(0, obj);
    summaries.insert(0, sum);
    input_names.insert(0, "*SYSTEM*".to_string());
}

fn update_section_info_rsize(summaries: &mut [ObjectSummary], layout: &crate::layout::LayoutPlan) {
    use crate::resolver::SectionKind;
    let rsize = layout
        .total_size_by_section
        .get(&SectionKind::RData)
        .copied()
        .unwrap_or(0)
        .saturating_add(
            layout
                .total_size_by_section
                .get(&SectionKind::RBss)
                .copied()
                .unwrap_or(0),
        )
        .saturating_add(
            layout
                .total_size_by_section
                .get(&SectionKind::RCommon)
                .copied()
                .unwrap_or(0),
        )
        .saturating_add(
            layout
                .total_size_by_section
                .get(&SectionKind::RStack)
                .copied()
                .unwrap_or(0),
        )
        .saturating_add(
            layout
                .total_size_by_section
                .get(&SectionKind::RLData)
                .copied()
                .unwrap_or(0),
        )
        .saturating_add(
            layout
                .total_size_by_section
                .get(&SectionKind::RLBss)
                .copied()
                .unwrap_or(0),
        )
        .saturating_add(
            layout
                .total_size_by_section
                .get(&SectionKind::RLCommon)
                .copied()
                .unwrap_or(0),
        )
        .saturating_add(
            layout
                .total_size_by_section
                .get(&SectionKind::RLStack)
                .copied()
                .unwrap_or(0),
        );
    for summary in summaries.iter_mut() {
        for sym in &mut summary.symbols {
            if sym.section == SectionKind::Abs && sym.name == b"___rsize" {
                sym.value = rsize;
            }
        }
    }
}

fn resolve_output_path(args: &Args, inputs: &[String]) -> String {
    let explicit = args.output.is_some();
    let base = if let Some(out) = args.output.clone() {
        out
    } else if let Some(first) = inputs.first() {
        let p = PathBuf::from(first);
        if let Some(stem) = p.file_stem().and_then(|s| s.to_str()) {
            if let Some(parent) = p.parent() {
                if !parent.as_os_str().is_empty() {
                    parent.join(stem).to_string_lossy().to_string()
                } else {
                    stem.to_string()
                }
            } else {
                stem.to_string()
            }
        } else {
            first.clone()
        }
    } else {
        "a".to_string()
    };
    let p = PathBuf::from(&base);
    if p.extension().is_some() {
        return base;
    }
    if args.make_mcs {
        return format!("{base}.mcs");
    }
    if args.r_format {
        return format!("{base}.r");
    }
    let add_x = if args.opt_an {
        !explicit
    } else {
        !args.no_x_ext
    };
    if add_x {
        format!("{base}.x")
    } else {
        base
    }
}

fn resolve_map_output(
    map_opt: &Option<String>,
    output_opt: &Option<String>,
    inputs: &[String],
) -> Option<String> {
    let raw = map_opt.as_ref()?;
    if !raw.is_empty() {
        let p = PathBuf::from(raw);
        if p.extension().is_some() {
            return Some(raw.clone());
        }
        let mut with = raw.clone();
        with.push_str(".map");
        return Some(with);
    }
    let base = output_opt
        .as_ref()
        .cloned()
        .or_else(|| inputs.first().cloned())?;
    let p = PathBuf::from(base);
    let stem = p.file_stem()?.to_string_lossy();
    let mut out = if let Some(parent) = p.parent() {
        parent.join(format!("{stem}.map"))
    } else {
        PathBuf::from(format!("{stem}.map"))
    };
    if out.as_os_str().is_empty() {
        out = PathBuf::from("a.map");
    }
    Some(out.to_string_lossy().to_string())
}

fn validate_unresolved_symbols(summaries: &[ObjectSummary], input_names: &[String]) -> anyhow::Result<()> {
    let mut defs = HashSet::<Vec<u8>>::new();
    for s in summaries {
        for sym in &s.symbols {
            defs.insert(sym.name.clone());
        }
    }
    let mut messages = Vec::<String>::new();
    for (idx, s) in summaries.iter().enumerate() {
        for xr in &s.xrefs {
            if defs.contains(&xr.name) {
                continue;
            }
            let name = String::from_utf8_lossy(&xr.name);
            let file = input_names.get(idx).cloned().unwrap_or_else(|| "<unknown>".to_string());
            messages.push(format!("未定義シンボル: {name} in {file}"));
        }
    }
    if messages.is_empty() {
        return Ok(());
    }
    messages.sort();
    messages.dedup();
    anyhow::bail!("{}", messages.join("\n"));
}

fn load_objects_with_requests(
    initial_inputs: &[String],
    verbose: bool,
) -> anyhow::Result<(Vec<crate::format::obj::ObjectFile>, Vec<ObjectSummary>, Vec<String>)> {
    const MAX_ARCHIVE_VISITS: usize = 64;
    let mut objects = Vec::new();
    let mut summaries = Vec::new();
    let mut input_names = Vec::new();
    let mut pending = VecDeque::<PathBuf>::new();
    let mut loaded = HashSet::<PathBuf>::new();
    let mut archive_visits = HashMap::<PathBuf, usize>::new();

    for input in initial_inputs {
        pending.push_back(PathBuf::from(input));
    }

    while let Some(path) = pending.pop_front() {
        let abs = absolutize_path(&path)?;
        if is_archive_like(&path) {
            let cnt = archive_visits.entry(abs.clone()).or_insert(0);
            *cnt += 1;
            if *cnt > MAX_ARCHIVE_VISITS {
                let name = path
                    .file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or_else(|| path.to_str().unwrap_or("<non-utf8>"));
                anyhow::bail!("archive request loop detected: {name}");
            }
        }
        // Keep one-pass semantics for normal object files, but allow re-reading
        // archive inputs when they are explicitly listed multiple times.
        if !is_archive_like(&path) && !loaded.insert(abs.clone()) {
            continue;
        }
        let bytes = std::fs::read(&abs).map_err(|_| {
            let name = path
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or_else(|| path.to_str().unwrap_or("<non-utf8>"));
            anyhow::anyhow!("ファイルがありません: {name}")
        })?;
        match parse_object(&bytes) {
            Ok(object) => {
                let command_count = object.commands.len();
                let summary = resolve_object(&object);

                if verbose {
                    let label = path.to_string_lossy();
                    println!("parsed {label}: {command_count} commands");
                    println!(
                        "  align={} sections: declared={} observed={} symbols={} xrefs={} requests={}",
                        summary.object_align,
                        summary.declared_section_sizes.len(),
                        summary.observed_section_usage.len(),
                        summary.symbols.len(),
                        summary.xrefs.len(),
                        summary.requests.len()
                    );
                }

                let base_dir = abs.parent().unwrap_or(Path::new("."));
                enqueue_requests(
                    &mut pending,
                    base_dir,
                    &summary.requests,
                )?;
                objects.push(object);
                summaries.push(summary);
                input_names.push(path.to_string_lossy().to_string());
            }
            Err(FormatError::UnsupportedCommand(_)) if is_archive_like(&path) && is_ar_archive(&bytes) => {
                let members = parse_ar_members(&bytes)?;
                if members.is_empty() {
                    let name = path
                        .file_name()
                        .and_then(|s| s.to_str())
                        .unwrap_or_else(|| path.to_str().unwrap_or("<non-utf8>"));
                    anyhow::bail!("archive has no members: {name}");
                }
                let base_dir = abs.parent().unwrap_or(Path::new("."));
                let mut parsed_members = Vec::new();
                for (member_name, payload) in members {
                    let object = parse_object(&payload).map_err(|e| {
                        anyhow::anyhow!("{}({}): {}", path.to_string_lossy(), member_name, e)
                    })?;
                    let summary = resolve_object(&object);
                    parsed_members.push((member_name, object, summary));
                }
                let select_indices = select_archive_members(&summaries, &parsed_members);
                for idx in select_indices {
                    let (member_name, object, summary) = &parsed_members[idx];
                    if verbose {
                        println!(
                            "parsed {}({}): {} commands",
                            path.to_string_lossy(),
                            member_name,
                            object.commands.len()
                        );
                    }
                    enqueue_requests(
                        &mut pending,
                        base_dir,
                        &summary.requests,
                    )?;
                    objects.push(object.clone());
                    summaries.push(summary.clone());
                    input_names.push(format!("{}({})", path.to_string_lossy(), member_name));
                }
            }
            Err(e) => {
                let name = path
                    .file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or_else(|| path.to_str().unwrap_or("<non-utf8>"));
                anyhow::bail!("{name}: {e}");
            }
        }
    }

    Ok((objects, summaries, input_names))
}

fn enqueue_requests(
    pending: &mut VecDeque<PathBuf>,
    base_dir: &Path,
    requests: &[Vec<u8>],
) -> anyhow::Result<()> {
    for req in requests {
        let req_name = String::from_utf8_lossy(req).to_string();
        let req_path = resolve_requested_path(base_dir, &req_name)
            .ok_or_else(|| anyhow::anyhow!("ファイルがありません: {}", req_name))?;
        pending.push_back(req_path);
    }
    Ok(())
}

fn resolve_requested_path(base_dir: &Path, req_name: &str) -> Option<PathBuf> {
    let req = Path::new(req_name);
    let mut candidates = Vec::<PathBuf>::new();
    let mut add_candidates = |base: PathBuf| {
        candidates.push(base.clone());
        // For extension-less request names, try common object/library suffixes.
        if base.extension().is_none() {
            for ext in ["o", "obj", "a", "lib"] {
                candidates.push(base.with_extension(ext));
            }
        }
    };
    if req.is_absolute() {
        add_candidates(req.to_path_buf());
    } else {
        add_candidates(base_dir.join(req));
        add_candidates(PathBuf::from(req));
    }
    for c in candidates {
        if c.exists() {
            return Some(c);
        }
    }
    None
}

fn absolutize_path(path: &Path) -> anyhow::Result<PathBuf> {
    if path.is_absolute() {
        return Ok(path.to_path_buf());
    }
    let cwd = std::env::current_dir()?;
    Ok(cwd.join(path))
}

fn is_archive_like(path: &Path) -> bool {
    let Some(ext) = path.extension().and_then(|s| s.to_str()) else {
        return false;
    };
    ext.eq_ignore_ascii_case("a") || ext.eq_ignore_ascii_case("lib")
}

fn is_ar_archive(bytes: &[u8]) -> bool {
    bytes.starts_with(b"!<arch>\n")
}

fn parse_ar_members(bytes: &[u8]) -> anyhow::Result<Vec<(String, Vec<u8>)>> {
    if !is_ar_archive(bytes) {
        anyhow::bail!("not ar archive");
    }
    let mut out = Vec::new();
    let mut gnu_long_names: Option<Vec<u8>> = None;
    let mut pos = 8usize;
    while pos < bytes.len() {
        if bytes.len().saturating_sub(pos) < 60 {
            anyhow::bail!("invalid ar header");
        }
        let hdr = &bytes[pos..pos + 60];
        pos += 60;
        if &hdr[58..60] != b"`\n" {
            anyhow::bail!("invalid ar header magic");
        }
        let size_str = std::str::from_utf8(&hdr[48..58])?.trim();
        let size = size_str.parse::<usize>()?;
        if bytes.len().saturating_sub(pos) < size {
            anyhow::bail!("invalid ar member size");
        }
        let raw_name = std::str::from_utf8(&hdr[0..16])?.trim().to_string();
        let mut data = bytes[pos..pos + size].to_vec();
        pos += size;
        if pos % 2 == 1 {
            pos = pos.saturating_add(1);
        }

        if raw_name == "/" {
            continue;
        }
        if raw_name == "//" {
            gnu_long_names = Some(data);
            continue;
        }
        if let Some(rest) = raw_name.strip_prefix("#1/") {
            let n = rest.parse::<usize>()?;
            if data.len() < n {
                anyhow::bail!("invalid BSD ar extended name");
            }
            let name = trim_member_name(&String::from_utf8_lossy(&data[..n]));
            data = data[n..].to_vec();
            out.push((name, data));
            continue;
        }
        if let Some(rest) = raw_name.strip_prefix('/') {
            if let Ok(offset) = rest.parse::<usize>() {
                if let Some(name) = resolve_gnu_long_name(gnu_long_names.as_deref(), offset) {
                    out.push((name, data));
                    continue;
                }
                anyhow::bail!("invalid GNU long-name reference: /{offset}");
            }
            anyhow::bail!("unsupported ar special member name: {raw_name}");
        }
        out.push((trim_member_name(&raw_name), data));
    }
    Ok(out)
}

fn select_archive_members(
    loaded_summaries: &[ObjectSummary],
    members: &[(String, crate::format::obj::ObjectFile, ObjectSummary)],
) -> Vec<usize> {
    let mut selected = Vec::<usize>::new();
    let mut selected_set = HashSet::<usize>::new();
    let mut unresolved = unresolved_symbols(loaded_summaries);
    if unresolved.is_empty() {
        return selected;
    }

    loop {
        let mut changed = false;
        for (idx, (_, _, sum)) in members.iter().enumerate() {
            if selected_set.contains(&idx) {
                continue;
            }
            let provides_needed = sum.symbols.iter().any(|s| unresolved.contains(&s.name));
            if !provides_needed {
                continue;
            }
            selected.push(idx);
            selected_set.insert(idx);
            changed = true;

            let mut all = loaded_summaries.to_vec();
            all.extend(selected.iter().map(|i| members[*i].2.clone()));
            unresolved = unresolved_symbols(&all);
        }
        if !changed {
            break;
        }
    }
    selected
}

fn unresolved_symbols(summaries: &[ObjectSummary]) -> HashSet<Vec<u8>> {
    let mut defs = HashSet::<Vec<u8>>::new();
    let mut xrefs = HashSet::<Vec<u8>>::new();
    for s in summaries {
        for sym in &s.symbols {
            defs.insert(sym.name.clone());
        }
        for xr in &s.xrefs {
            xrefs.insert(xr.name.clone());
        }
    }
    xrefs.retain(|n| !defs.contains(n));
    xrefs
}

fn trim_member_name(name: &str) -> String {
    let mut n = name.trim().to_string();
    if n.ends_with('/') {
        n.pop();
    }
    n
}

fn resolve_gnu_long_name(table: Option<&[u8]>, offset: usize) -> Option<String> {
    let t = table?;
    if offset >= t.len() {
        return None;
    }
    let mut end = offset;
    while end < t.len() {
        if t[end] == b'\n' {
            break;
        }
        end += 1;
    }
    let raw = String::from_utf8_lossy(&t[offset..end]).to_string();
    Some(trim_member_name(raw.trim_end_matches('/')))
}

#[cfg(test)]
mod tests {
    use super::{
        inject_define_symbols, inject_section_info_object, is_ar_archive, load_objects_with_requests,
        parse_ar_members, resolve_lib_inputs, resolve_map_output, resolve_output_path, run,
        select_archive_members,
        update_section_info_rsize, validate_unresolved_symbols,
    };
    use crate::cli::{Args, DefineArg};
    use crate::layout::plan_layout;
    use crate::format::obj::{Command, ObjectFile, parse_object};
    use crate::resolver::{SectionKind, resolve_object};
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn make_simple_ar(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(b"!<arch>\n");
        for (name, data) in entries {
            let mut name_field = format!("{name}/");
            if name_field.len() > 16 {
                name_field.truncate(16);
            }
            while name_field.len() < 16 {
                name_field.push(' ');
            }
            let mut size_field = data.len().to_string();
            while size_field.len() < 10 {
                size_field.insert(0, ' ');
            }
            let header = format!("{name_field}{:>12}{:>6}{:>6}{:>8}{size_field}`\n", 0, 0, 0, 0);
            out.extend_from_slice(header.as_bytes());
            out.extend_from_slice(data);
            if data.len() % 2 == 1 {
                out.push(b'\n');
            }
        }
        out
    }

    fn obj_with_xref_and_request(xref: &str, req: &str) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&[0xb2, 0xff, 0x00, 0x00, 0x00, 0x01]);
        out.extend_from_slice(xref.as_bytes());
        out.push(0x00);
        if out.len() % 2 == 1 {
            out.push(0x00);
        }
        out.extend_from_slice(&[0xe0, 0x01]);
        out.extend_from_slice(req.as_bytes());
        out.push(0x00);
        if out.len() % 2 == 1 {
            out.push(0x00);
        }
        out.extend_from_slice(&[0x00, 0x00]);
        out
    }

    fn obj_with_xref(xref: &str) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&[0xb2, 0xff, 0x00, 0x00, 0x00, 0x01]);
        out.extend_from_slice(xref.as_bytes());
        out.push(0x00);
        if out.len() % 2 == 1 {
            out.push(0x00);
        }
        out.extend_from_slice(&[0x00, 0x00]);
        out
    }

    fn obj_with_def(name: &str) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&[0xb2, 0x01, 0x00, 0x00, 0x00, 0x00]);
        out.extend_from_slice(name.as_bytes());
        out.push(0x00);
        if out.len() % 2 == 1 {
            out.push(0x00);
        }
        out.extend_from_slice(&[0x00, 0x00]);
        out
    }

    fn obj_with_def_and_xref(def: &str, xref: &str) -> Vec<u8> {
        let mut out = obj_with_def(def);
        out.truncate(out.len().saturating_sub(2)); // remove end
        out.extend_from_slice(&[0xb2, 0xff, 0x00, 0x00, 0x00, 0x01]);
        out.extend_from_slice(xref.as_bytes());
        out.push(0x00);
        if out.len() % 2 == 1 {
            out.push(0x00);
        }
        out.extend_from_slice(&[0x00, 0x00]);
        out
    }

    fn make_gnu_longname_ar(long_name: &str, payload: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(b"!<arch>\n");
        let mut long_table = Vec::new();
        long_table.extend_from_slice(long_name.as_bytes());
        long_table.extend_from_slice(b"/\n");

        let mut long_name_header = String::from("//");
        while long_name_header.len() < 16 {
            long_name_header.push(' ');
        }
        let mut size_field = long_table.len().to_string();
        while size_field.len() < 10 {
            size_field.insert(0, ' ');
        }
        let header = format!("{long_name_header}{:>12}{:>6}{:>6}{:>8}{size_field}`\n", 0, 0, 0, 0);
        out.extend_from_slice(header.as_bytes());
        out.extend_from_slice(&long_table);
        if long_table.len() % 2 == 1 {
            out.push(b'\n');
        }

        let mut member_name = String::from("/0");
        while member_name.len() < 16 {
            member_name.push(' ');
        }
        let mut mem_size = payload.len().to_string();
        while mem_size.len() < 10 {
            mem_size.insert(0, ' ');
        }
        let header2 = format!("{member_name}{:>12}{:>6}{:>6}{:>8}{mem_size}`\n", 0, 0, 0, 0);
        out.extend_from_slice(header2.as_bytes());
        out.extend_from_slice(payload);
        if payload.len() % 2 == 1 {
            out.push(b'\n');
        }
        out
    }

    fn make_bsd_longname_ar(long_name: &str, payload: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(b"!<arch>\n");
        let mut name_field = format!("#1/{}", long_name.len());
        while name_field.len() < 16 {
            name_field.push(' ');
        }
        let size_total = long_name.len() + payload.len();
        let mut size_field = size_total.to_string();
        while size_field.len() < 10 {
            size_field.insert(0, ' ');
        }
        let header = format!("{name_field}{:>12}{:>6}{:>6}{:>8}{size_field}`\n", 0, 0, 0, 0);
        out.extend_from_slice(header.as_bytes());
        out.extend_from_slice(long_name.as_bytes());
        out.extend_from_slice(payload);
        if size_total % 2 == 1 {
            out.push(b'\n');
        }
        out
    }

    #[test]
    fn loads_requested_object_from_same_directory() {
        let uniq = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time")
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("rhlk-linker-test-{uniq}"));
        fs::create_dir_all(&dir).expect("mkdir");

        let main = dir.join("main.o");
        let sub = dir.join("sub.o");

        // e001 "sub.o", 0000(end)
        fs::write(&main, [0xe0, 0x01, b's', b'u', b'b', b'.', b'o', 0x00, 0x00, 0x00]).expect("write main");
        // 0000(end)
        fs::write(&sub, [0x00, 0x00]).expect("write sub");

        let inputs = vec![main.to_string_lossy().to_string()];
        let (_, _, names) = load_objects_with_requests(&inputs, false).expect("load");
        assert_eq!(names.len(), 2);
        assert_eq!(names[0], main.to_string_lossy());
        assert!(names.iter().any(|v| v.ends_with("sub.o")));

        let _ = fs::remove_file(main);
        let _ = fs::remove_file(sub);
        let _ = fs::remove_dir(dir);
    }

    #[test]
    fn reports_missing_requested_object() {
        let uniq = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time")
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("rhlk-linker-test-{uniq}"));
        fs::create_dir_all(&dir).expect("mkdir");

        let main = dir.join("main.o");
        fs::write(&main, [0xe0, 0x01, b'n', b'o', b'n', b'e', b'.', b'o', 0x00, 0x00]).expect("write main");

        let inputs = vec![main.to_string_lossy().to_string()];
        let err = load_objects_with_requests(&inputs, false).expect_err("must fail");
        assert!(err.to_string().contains("ファイルがありません: none.o"));

        let _ = fs::remove_file(main);
        let _ = fs::remove_dir(dir);
    }

    #[test]
    fn resolves_requested_object_with_o_extension() {
        let uniq = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time")
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("rhlk-linker-test-{uniq}"));
        fs::create_dir_all(&dir).expect("mkdir");

        let main = dir.join("main.o");
        let sub = dir.join("sub.o");

        // e001 "sub", 0000(end)
        fs::write(&main, [0xe0, 0x01, b's', b'u', b'b', 0x00, 0x00, 0x00]).expect("write main");
        fs::write(&sub, [0x00, 0x00]).expect("write sub");

        let inputs = vec![main.to_string_lossy().to_string()];
        let (_, _, names) = load_objects_with_requests(&inputs, false).expect("load");
        assert_eq!(names.len(), 2);
        assert!(names.iter().any(|v| v.ends_with("sub.o")));

        let _ = fs::remove_file(main);
        let _ = fs::remove_file(sub);
        let _ = fs::remove_dir(dir);
    }

    #[test]
    fn resolves_requested_archive_with_a_extension() {
        let uniq = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time")
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("rhlk-linker-test-{uniq}"));
        fs::create_dir_all(&dir).expect("mkdir");
        let main = dir.join("main.o");
        let lib = dir.join("libx.a");

        fs::write(&main, obj_with_xref_and_request("foo", "libx")).expect("write main");
        let ar = make_simple_ar(&[("foo.o", &obj_with_def("foo"))]);
        fs::write(&lib, ar).expect("write lib");

        let inputs = vec![main.to_string_lossy().to_string()];
        let (_, sums, names) = load_objects_with_requests(&inputs, false).expect("load");
        validate_unresolved_symbols(&sums, &names).expect("resolved");
        assert!(names.iter().any(|v| v.ends_with("libx.a(foo.o)")));

        let _ = fs::remove_file(main);
        let _ = fs::remove_file(lib);
        let _ = fs::remove_dir(dir);
    }

    #[test]
    fn resolves_l_option_library_from_l_path() {
        let uniq = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time")
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("rhlk-linker-test-{uniq}"));
        fs::create_dir_all(&dir).expect("mkdir");
        let lib = dir.join("libfoo.a");
        fs::write(&lib, make_simple_ar(&[("x.o", &[0x00, 0x00])])).expect("write lib");
        let args = Args {
            output: None,
            r_format: false,
            r_no_check: false,
            no_x_ext: false,
            opt_an: false,
            align: None,
            base_address: None,
            load_mode: None,
            defines: Vec::new(),
            indirect_files: Vec::new(),
            lib_paths: vec![dir.to_string_lossy().to_string()],
            libs: vec!["foo".to_string()],
            use_env_lib: false,
            g2lk_off: false,
            g2lk_on: false,
            make_mcs: false,
            omit_bss: false,
            cut_symbols: false,
            map: None,
            verbose: false,
            quiet: false,
            warn_off: false,
            title: false,
            section_info: false,
            inputs: vec![],
        };
        let libs = resolve_lib_inputs(&args).expect("resolve");
        assert_eq!(libs.len(), 1);
        assert_eq!(libs[0], lib.to_string_lossy());
        let _ = fs::remove_file(lib);
        let _ = fs::remove_dir(dir);
    }

    #[test]
    fn request_resolution_prefers_input_base_dir_over_cwd() {
        let uniq = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("rhlk-linker-test-{uniq}"));
        let dir_input = root.join("input");
        let dir_cwd = root.join("cwd");
        fs::create_dir_all(&dir_input).expect("mkdir input");
        fs::create_dir_all(&dir_cwd).expect("mkdir cwd");

        let main = dir_input.join("main.o");
        let sub_input = dir_input.join("sub.o");
        let sub_cwd = dir_cwd.join("sub.o");
        fs::write(&main, [0xe0, 0x01, b's', b'u', b'b', 0x00, 0x00, 0x00]).expect("write main");
        fs::write(&sub_input, [0x00, 0x00]).expect("write sub input");
        fs::write(&sub_cwd, [0x00, 0x00]).expect("write sub cwd");

        let prev = std::env::current_dir().expect("cwd");
        std::env::set_current_dir(&dir_cwd).expect("chdir");
        let inputs = vec![main.to_string_lossy().to_string()];
        let (_, _, names) = load_objects_with_requests(&inputs, false).expect("load");
        std::env::set_current_dir(prev).expect("restore cwd");

        assert_eq!(names.len(), 2);
        assert_eq!(names[1], sub_input.to_string_lossy());

        let _ = fs::remove_file(main);
        let _ = fs::remove_file(sub_input);
        let _ = fs::remove_file(sub_cwd);
        let _ = fs::remove_dir(&dir_input);
        let _ = fs::remove_dir(&dir_cwd);
        let _ = fs::remove_dir(root);
    }

    #[test]
    fn loads_archive_members_by_unresolved_xref() {
        let uniq = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time")
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("rhlk-linker-test-{uniq}"));
        fs::create_dir_all(&dir).expect("mkdir");

        let main = dir.join("main.o");
        let lib = dir.join("libx.a");
        fs::write(&main, obj_with_xref_and_request("foo", "libx.a")).expect("write main");
        let ar = make_simple_ar(&[("foo.o", &obj_with_def("foo")), ("bar.o", &obj_with_def("bar"))]);
        fs::write(&lib, ar).expect("write lib");

        let inputs = vec![main.to_string_lossy().to_string()];
        let (_, _, names) = load_objects_with_requests(&inputs, false).expect("must load");
        assert_eq!(names.len(), 2);
        assert!(names.iter().any(|v| v.ends_with("libx.a(foo.o)")));
        assert!(!names.iter().any(|v| v.ends_with("libx.a(bar.o)")));

        let _ = fs::remove_file(main);
        let _ = fs::remove_file(lib);
        let _ = fs::remove_dir(dir);
    }

    #[test]
    fn parses_simple_ar_members() {
        let ar = make_simple_ar(&[("x.o", &[0x00, 0x00]), ("y.o", &[0x10, 0x00, 0x00, 0x00])]);
        assert!(is_ar_archive(&ar));
        let members = parse_ar_members(&ar).expect("parse");
        assert_eq!(members.len(), 2);
        assert_eq!(members[0].0, "x.o");
        assert_eq!(members[0].1, vec![0x00, 0x00]);
        assert_eq!(members[1].0, "y.o");
        assert_eq!(members[1].1, vec![0x10, 0x00, 0x00, 0x00]);
    }

    #[test]
    fn parses_gnu_longname_ar_members() {
        let ar = make_gnu_longname_ar("very_long_name_member.o", &[0x00, 0x00]);
        let members = parse_ar_members(&ar).expect("parse");
        assert_eq!(members.len(), 1);
        assert_eq!(members[0].0, "very_long_name_member.o");
        assert_eq!(members[0].1, vec![0x00, 0x00]);
    }

    #[test]
    fn parses_bsd_longname_ar_members() {
        let ar = make_bsd_longname_ar("bsd_long_name_member.o", &[0x00, 0x00]);
        let members = parse_ar_members(&ar).expect("parse");
        assert_eq!(members.len(), 1);
        assert_eq!(members[0].0, "bsd_long_name_member.o");
        assert_eq!(members[0].1, vec![0x00, 0x00]);
    }

    #[test]
    fn rejects_invalid_gnu_longname_reference() {
        let mut ar = Vec::new();
        ar.extend_from_slice(b"!<arch>\n");
        // Member name /12 without // table
        let mut name = String::from("/12");
        while name.len() < 16 {
            name.push(' ');
        }
        let mut size = String::from("2");
        while size.len() < 10 {
            size.insert(0, ' ');
        }
        let header = format!("{name}{:>12}{:>6}{:>6}{:>8}{size}`\n", 0, 0, 0, 0);
        ar.extend_from_slice(header.as_bytes());
        ar.extend_from_slice(&[0x00, 0x00]);

        let err = parse_ar_members(&ar).expect_err("must fail");
        assert!(err.to_string().contains("invalid GNU long-name reference"));
    }

    #[test]
    fn selects_archive_member_that_resolves_unresolved_xref() {
        let main_bytes = obj_with_xref_and_request("foo", "libx.a");
        let main = parse_object(&main_bytes).expect("main parse");
        let main_sum = resolve_object(&main);

        let foo_obj = parse_object(&obj_with_def("foo")).expect("foo parse");
        let foo_sum = resolve_object(&foo_obj);
        let bar_obj = parse_object(&obj_with_def("bar")).expect("bar parse");
        let bar_sum = resolve_object(&bar_obj);

        let members = vec![
            ("foo.o".to_string(), foo_obj.clone(), foo_sum.clone()),
            ("bar.o".to_string(), bar_obj.clone(), bar_sum.clone()),
        ];
        let picked = select_archive_members(&[main_sum], &members);
        assert_eq!(picked, vec![0]);
    }

    #[test]
    fn selects_archive_members_with_dependency_chain() {
        let main_bytes = obj_with_xref_and_request("foo", "libx.a");
        let main = parse_object(&main_bytes).expect("main parse");
        let main_sum = resolve_object(&main);

        let foo_obj = parse_object(&obj_with_def_and_xref("foo", "bar")).expect("foo parse");
        let foo_sum = resolve_object(&foo_obj);
        let bar_obj = parse_object(&obj_with_def("bar")).expect("bar parse");
        let bar_sum = resolve_object(&bar_obj);

        let members = vec![
            ("foo.o".to_string(), foo_obj, foo_sum),
            ("bar.o".to_string(), bar_obj, bar_sum),
        ];
        let picked = select_archive_members(&[main_sum], &members);
        assert_eq!(picked, vec![0, 1]);
    }

    #[test]
    fn rescans_archive_from_head_after_new_unresolved() {
        let main_bytes = obj_with_xref_and_request("foo", "libx.a");
        let main = parse_object(&main_bytes).expect("main parse");
        let main_sum = resolve_object(&main);

        // bar member appears first, but is not needed until foo member is selected.
        let bar_obj = parse_object(&obj_with_def("bar")).expect("bar parse");
        let bar_sum = resolve_object(&bar_obj);
        let foo_obj = parse_object(&obj_with_def_and_xref("foo", "bar")).expect("foo parse");
        let foo_sum = resolve_object(&foo_obj);

        let members = vec![
            ("bar.o".to_string(), bar_obj, bar_sum),
            ("foo.o".to_string(), foo_obj, foo_sum),
        ];
        let picked = select_archive_members(&[main_sum], &members);
        assert_eq!(picked, vec![1, 0]);
    }

    #[test]
    fn prefers_first_member_for_duplicate_definition() {
        let main_bytes = obj_with_xref_and_request("foo", "libx.a");
        let main = parse_object(&main_bytes).expect("main parse");
        let main_sum = resolve_object(&main);

        let foo1_obj = parse_object(&obj_with_def("foo")).expect("foo1 parse");
        let foo1_sum = resolve_object(&foo1_obj);
        let foo2_obj = parse_object(&obj_with_def("foo")).expect("foo2 parse");
        let foo2_sum = resolve_object(&foo2_obj);

        let members = vec![
            ("foo1.o".to_string(), foo1_obj, foo1_sum),
            ("foo2.o".to_string(), foo2_obj, foo2_sum),
        ];
        let picked = select_archive_members(&[main_sum], &members);
        assert_eq!(picked, vec![0]);
    }

    #[test]
    fn reports_unresolved_symbols_after_expansion() {
        let main = parse_object(&obj_with_xref_and_request("foo", "libx.a")).expect("main parse");
        let main_sum = resolve_object(&main);
        let inputs = vec!["main.o".to_string()];
        let err = validate_unresolved_symbols(&[main_sum], &inputs).expect_err("must fail");
        assert!(err.to_string().contains("未定義シンボル: foo in main.o"));
    }

    #[test]
    fn allows_archive_revisit_when_listed_multiple_times() {
        let uniq = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time")
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("rhlk-linker-test-{uniq}"));
        fs::create_dir_all(&dir).expect("mkdir");

        let lib = dir.join("libx.a");
        let main = dir.join("main.o");
        let ar = make_simple_ar(&[("foo.o", &obj_with_def("foo"))]);
        fs::write(&lib, ar).expect("write lib");
        fs::write(&main, obj_with_xref("foo")).expect("write main");

        let inputs = vec![
            lib.to_string_lossy().to_string(),
            main.to_string_lossy().to_string(),
            lib.to_string_lossy().to_string(),
        ];
        let (_, sums, names) = load_objects_with_requests(&inputs, false).expect("load");
        validate_unresolved_symbols(&sums, &names).expect("resolved");
        assert!(names.iter().any(|v| v.ends_with("libx.a(foo.o)")));

        let _ = fs::remove_file(main);
        let _ = fs::remove_file(lib);
        let _ = fs::remove_dir(dir);
    }

    #[test]
    fn allows_archive_revisit_via_multiple_requests() {
        let uniq = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time")
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("rhlk-linker-test-{uniq}"));
        fs::create_dir_all(&dir).expect("mkdir");

        let lib = dir.join("libx.a");
        let main1 = dir.join("main1.o");
        let main2 = dir.join("main2.o");
        let ar = make_simple_ar(&[("foo.o", &obj_with_def("foo"))]);
        fs::write(&lib, ar).expect("write lib");
        fs::write(&main1, obj_with_xref_and_request("dummy", "libx.a")).expect("write main1");
        fs::write(&main2, obj_with_xref_and_request("foo", "libx.a")).expect("write main2");

        let inputs = vec![
            main1.to_string_lossy().to_string(),
            main2.to_string_lossy().to_string(),
        ];
        let (_, sums, names) = load_objects_with_requests(&inputs, false).expect("load");
        // dummy stays unresolved (expected), but foo should be resolved by second archive visit.
        let err = validate_unresolved_symbols(&sums, &names).expect_err("must have unresolved");
        assert!(!err.to_string().contains("未定義シンボル: foo"));

        let _ = fs::remove_file(main1);
        let _ = fs::remove_file(main2);
        let _ = fs::remove_file(lib);
        let _ = fs::remove_dir(dir);
    }

    #[test]
    fn resolves_map_output_name() {
        let o = resolve_map_output(&Some(String::new()), &Some("out.x".to_string()), &["in.o".to_string()]);
        assert_eq!(o.as_deref(), Some("out.map"));
        let i = resolve_map_output(&Some(String::new()), &None, &["src/main.o".to_string()]);
        assert_eq!(i.as_deref(), Some("src/main.map"));
        let n = resolve_map_output(&Some("foo".to_string()), &None, &[]);
        assert_eq!(n.as_deref(), Some("foo.map"));
        let e = resolve_map_output(&Some("foo.txt".to_string()), &None, &[]);
        assert_eq!(e.as_deref(), Some("foo.txt"));
    }

    #[test]
    fn resolves_output_path_name() {
        let mut args = Args {
            output: None,
            r_format: false,
            r_no_check: false,
            no_x_ext: false,
            opt_an: false,
            align: None,
            base_address: None,
            load_mode: None,
            defines: Vec::new(),
            make_mcs: false,
            omit_bss: false,
            cut_symbols: false,
            map: None,
            verbose: false,
            quiet: false,
            warn_off: false,
            title: false,
            section_info: false,
            indirect_files: Vec::new(),
            lib_paths: Vec::new(),
            libs: Vec::new(),
            use_env_lib: false,
            g2lk_off: false,
            g2lk_on: false,
            inputs: vec!["foo.o".to_string()],
        };
        assert_eq!(resolve_output_path(&args, &args.inputs), "foo.x");

        args.no_x_ext = true;
        assert_eq!(resolve_output_path(&args, &args.inputs), "foo");

        args.opt_an = true;
        assert_eq!(resolve_output_path(&args, &args.inputs), "foo.x");

        args.output = Some("bar".to_string());
        assert_eq!(resolve_output_path(&args, &args.inputs), "bar");

        args.r_format = true;
        assert_eq!(resolve_output_path(&args, &args.inputs), "bar.r");

        args.make_mcs = true;
        assert_eq!(resolve_output_path(&args, &args.inputs), "bar.mcs");
    }

    #[test]
    fn run_writes_map_with_default_name() {
        let uniq = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time")
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("rhlk-linker-test-{uniq}"));
        fs::create_dir_all(&dir).expect("mkdir");
        let input = dir.join("foo.o");
        fs::write(&input, [0x00, 0x00]).expect("write input");

        let args = Args {
            output: None,
            r_format: false,
            r_no_check: false,
            no_x_ext: false,
            opt_an: false,
            align: None,
            base_address: None,
            load_mode: None,
            defines: Vec::new(),
            make_mcs: false,
            omit_bss: false,
            cut_symbols: false,
            map: Some(String::new()),
            verbose: false,
            quiet: false,
            warn_off: false,
            title: false,
            section_info: false,
            indirect_files: Vec::new(),
            lib_paths: Vec::new(),
            libs: Vec::new(),
            use_env_lib: false,
            g2lk_off: false,
            g2lk_on: false,
            inputs: vec![input.to_string_lossy().to_string()],
        };
        run(args).expect("run");
        assert!(dir.join("foo.map").exists());

        let _ = fs::remove_file(dir.join("foo.map"));
        let _ = fs::remove_file(input);
        let _ = fs::remove_dir(dir);
    }

    #[test]
    fn run_writes_map_with_explicit_name_extension_completion() {
        let uniq = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time")
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("rhlk-linker-test-{uniq}"));
        fs::create_dir_all(&dir).expect("mkdir");
        let input = dir.join("foo.o");
        fs::write(&input, [0x00, 0x00]).expect("write input");

        let map_path = dir.join("bar");
        let args = Args {
            output: None,
            r_format: false,
            r_no_check: false,
            no_x_ext: false,
            opt_an: false,
            align: None,
            base_address: None,
            load_mode: None,
            defines: Vec::new(),
            make_mcs: false,
            omit_bss: false,
            cut_symbols: false,
            map: Some(map_path.to_string_lossy().to_string()),
            verbose: false,
            quiet: false,
            warn_off: false,
            title: false,
            section_info: false,
            indirect_files: Vec::new(),
            lib_paths: Vec::new(),
            libs: Vec::new(),
            use_env_lib: false,
            g2lk_off: false,
            g2lk_on: false,
            inputs: vec![input.to_string_lossy().to_string()],
        };
        run(args).expect("run");
        assert!(dir.join("bar.map").exists());

        let _ = fs::remove_file(dir.join("bar.map"));
        let _ = fs::remove_file(input);
        let _ = fs::remove_dir(dir);
    }

    #[test]
    fn rejects_invalid_align_option_value() {
        let args = Args {
            output: None,
            r_format: false,
            r_no_check: false,
            no_x_ext: false,
            opt_an: false,
            align: Some(3),
            base_address: None,
            load_mode: None,
            defines: Vec::new(),
            make_mcs: false,
            omit_bss: false,
            cut_symbols: false,
            map: None,
            verbose: false,
            quiet: false,
            warn_off: false,
            title: false,
            section_info: false,
            indirect_files: Vec::new(),
            lib_paths: Vec::new(),
            libs: Vec::new(),
            use_env_lib: false,
            g2lk_off: false,
            g2lk_on: false,
            inputs: vec!["foo.o".to_string()],
        };
        let err = run(args).expect_err("must reject invalid align");
        assert!(err.to_string().contains("align size must be power of two"));
    }

    #[test]
    fn injects_define_symbols_as_absolute_xdef() {
        let args = Args {
            output: None,
            r_format: false,
            r_no_check: false,
            no_x_ext: false,
            opt_an: false,
            align: None,
            base_address: None,
            load_mode: None,
            defines: vec![DefineArg {
                name: "_FOO".to_string(),
                value: 0x1234,
            }],
            make_mcs: false,
            omit_bss: false,
            cut_symbols: false,
            map: None,
            verbose: false,
            quiet: false,
            warn_off: false,
            title: false,
            section_info: false,
            indirect_files: Vec::new(),
            lib_paths: Vec::new(),
            libs: Vec::new(),
            use_env_lib: false,
            g2lk_off: false,
            g2lk_on: false,
            inputs: vec!["in.o".to_string()],
        };
        let mut objects = Vec::new();
        let mut summaries = Vec::new();
        let mut names = Vec::new();
        inject_define_symbols(&args, &mut objects, &mut summaries, &mut names);
        assert_eq!(objects.len(), 1);
        assert_eq!(summaries.len(), 1);
        assert_eq!(names, vec!["*DEFINE*"]);
        assert_eq!(summaries[0].symbols.len(), 1);
        assert_eq!(summaries[0].symbols[0].name, b"_FOO".to_vec());
        assert_eq!(summaries[0].symbols[0].value, 0x1234);
    }

    #[test]
    fn injects_section_info_system_object() {
        let mut objects = Vec::new();
        let mut summaries = Vec::new();
        let mut names = Vec::new();
        inject_section_info_object(&mut objects, &mut summaries, &mut names);
        assert_eq!(objects.len(), 1);
        assert_eq!(summaries.len(), 1);
        assert_eq!(names, vec!["*SYSTEM*"]);
        assert!(summaries[0]
            .symbols
            .iter()
            .any(|s| s.name == b"___size_info" && s.section == SectionKind::Data));
        assert!(summaries[0]
            .symbols
            .iter()
            .any(|s| s.name == b"___rsize" && s.section == SectionKind::Abs));
    }

    #[test]
    fn updates_section_info_rsize_symbol_from_layout_totals() {
        let mut objects = Vec::new();
        let mut summaries = Vec::new();
        let mut names = Vec::new();
        inject_section_info_object(&mut objects, &mut summaries, &mut names);
        let obj = ObjectFile {
            commands: vec![
                Command::Header {
                    section: 0x05,
                    size: 3,
                    name: b"rdata".to_vec(),
                },
                Command::ChangeSection { section: 0x05 },
                Command::RawData(vec![0xaa, 0xbb]),
                Command::DefineSpace { size: 1 },
                Command::End,
            ],
            scd_tail: Vec::new(),
        };
        let sum = resolve_object(&obj);
        objects.push(obj);
        summaries.push(sum);
        let layout = plan_layout(&summaries);
        update_section_info_rsize(&mut summaries, &layout);
        let rsize = summaries[0]
            .symbols
            .iter()
            .find(|s| s.name == b"___rsize")
            .map(|s| s.value)
            .expect("___rsize");
        assert_eq!(rsize, 4);
    }
}
