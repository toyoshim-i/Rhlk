use crate::cli::Args;
use crate::format::obj::parse_object;
use crate::layout::plan_layout;
use crate::resolver::resolve_object;
use crate::writer::write_output;

pub fn run(args: Args) -> anyhow::Result<()> {
    if args.inputs.is_empty() {
        anyhow::bail!("no input files")
    }

    let mut objects = Vec::new();
    let mut summaries = Vec::new();
    for input in &args.inputs {
        let bytes = std::fs::read(input)?;
        let object = parse_object(&bytes)?;
        let command_count = object.commands.len();
        let summary = resolve_object(&object);
        objects.push(object);
        summaries.push(summary.clone());
        if args.verbose {
            println!("parsed {input}: {command_count} commands");
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
            &summaries,
            &layout,
        )?;
        if args.verbose {
            println!("wrote output: {output}");
        }
    }

    println!("rhlk: parsed {} input file(s)", args.inputs.len());
    Ok(())
}
