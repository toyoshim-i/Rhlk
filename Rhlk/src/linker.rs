use crate::cli::Args;
use crate::format::FormatError;
use crate::format::obj::parse_object;
use crate::layout::plan_layout;
use crate::resolver::resolve_object;
use crate::resolver::ObjectSummary;
use crate::writer::write_output;
use std::collections::{HashSet, VecDeque};
use std::path::{Path, PathBuf};

pub fn run(args: Args) -> anyhow::Result<()> {
    if args.inputs.is_empty() {
        anyhow::bail!("no input files")
    }

    let (objects, summaries, input_names) = load_objects_with_requests(&args.inputs, args.verbose)?;
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
    if let Some(output) = &args.output {
        write_output(
            output,
            effective_r,
            args.r_no_check,
            args.omit_bss,
            args.make_mcs,
            &objects,
            &input_names,
            &summaries,
            &layout,
        )?;
        if args.verbose {
            println!("wrote output: {output}");
        }
    }

    if args.verbose {
        println!("rhlk: parsed {} input file(s)", input_names.len());
    }
    Ok(())
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
    let mut objects = Vec::new();
    let mut summaries = Vec::new();
    let mut input_names = Vec::new();
    let mut pending = VecDeque::<PathBuf>::new();
    let mut loaded = HashSet::<PathBuf>::new();

    for input in initial_inputs {
        pending.push_back(PathBuf::from(input));
    }

    while let Some(path) = pending.pop_front() {
        let abs = absolutize_path(&path)?;
        if !loaded.insert(abs.clone()) {
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
                enqueue_requests(&mut pending, base_dir, &summary.requests)?;
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
                    enqueue_requests(&mut pending, base_dir, &summary.requests)?;
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
    if req.is_absolute() {
        if req.exists() {
            return Some(req.to_path_buf());
        }
        for ext in ["o", "obj"] {
            let c = req.with_extension(ext);
            if c.exists() {
                return Some(c);
            }
        }
        return None;
    }
    let c1 = base_dir.join(req);
    if c1.exists() {
        return Some(c1);
    }
    for ext in ["o", "obj"] {
        let c = base_dir.join(req).with_extension(ext);
        if c.exists() {
            return Some(c);
        }
    }
    let c2 = PathBuf::from(req);
    if c2.exists() {
        return Some(c2);
    }
    for ext in ["o", "obj"] {
        let c = PathBuf::from(req).with_extension(ext);
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
            }
            continue;
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
        is_ar_archive, load_objects_with_requests, parse_ar_members, select_archive_members,
        validate_unresolved_symbols,
    };
    use crate::format::obj::parse_object;
    use crate::resolver::resolve_object;
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
}
