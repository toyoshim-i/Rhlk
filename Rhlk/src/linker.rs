use crate::cli::Args;
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
        let object = parse_object(&bytes)?;
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
        for req in &summary.requests {
            let req_name = String::from_utf8_lossy(req).to_string();
            let req_path = resolve_requested_path(base_dir, &req_name).ok_or_else(|| {
                anyhow::anyhow!("ファイルがありません: {}", req_name)
            })?;
            pending.push_back(req_path);
        }

        objects.push(object);
        summaries.push(summary);
        input_names.push(path.to_string_lossy().to_string());
    }

    Ok((objects, summaries, input_names))
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

#[cfg(test)]
mod tests {
    use super::load_objects_with_requests;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

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
}
