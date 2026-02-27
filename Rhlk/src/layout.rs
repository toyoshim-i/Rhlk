use std::collections::{BTreeMap, HashMap};

use crate::resolver::{ObjectSummary, SectionKind};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObjectPlacement {
    pub object_index: usize,
    pub by_section: BTreeMap<SectionKind, u32>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LayoutPlan {
    pub placements: Vec<ObjectPlacement>,
    pub total_size_by_section: BTreeMap<SectionKind, u32>,
    pub diagnostics: LayoutDiagnostics,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LayoutDiagnostics {
    pub common_conflicts: usize,
    pub common_warnings: usize,
}

#[must_use]
pub fn plan_layout(objects: &[ObjectSummary]) -> LayoutPlan {
    let mut placements = objects
        .iter()
        .enumerate()
        .map(|(idx, _)| ObjectPlacement {
            object_index: idx,
            by_section: BTreeMap::new(),
        })
        .collect::<Vec<_>>();

    let mut total_size_by_section = BTreeMap::new();

    let section_order = [
        SectionKind::Text,
        SectionKind::Data,
        SectionKind::RData,
        SectionKind::RLData,
        SectionKind::Bss,
        SectionKind::Stack,
        SectionKind::RBss,
        SectionKind::RStack,
        SectionKind::RLBss,
        SectionKind::RLStack,
    ];

    for section in section_order {
        let mut cursor = 0u32;
        for (idx, obj) in objects.iter().enumerate() {
            let size = section_size(obj, section);
            if size == 0 {
                continue;
            }

            cursor = align_up(cursor, obj.object_align.max(2));
            placements[idx].by_section.insert(section, cursor);
            cursor = cursor.saturating_add(size);
        }
        total_size_by_section.insert(section, cursor);
    }

    let (common_totals, diagnostics) = merge_common_symbols(objects);
    total_size_by_section.insert(SectionKind::Common, common_totals.common);
    total_size_by_section.insert(SectionKind::RCommon, common_totals.rcommon);
    total_size_by_section.insert(SectionKind::RLCommon, common_totals.rlcommon);

    LayoutPlan {
        placements,
        total_size_by_section,
        diagnostics,
    }
}

fn section_size(obj: &ObjectSummary, section: SectionKind) -> u32 {
    let declared = obj.declared_section_sizes.get(&section).copied().unwrap_or(0);
    let observed = obj.observed_section_usage.get(&section).copied().unwrap_or(0);
    align_even(declared.max(observed))
}

fn align_even(v: u32) -> u32 {
    (v + 1) & !1
}

fn align_up(value: u32, align: u32) -> u32 {
    let mask = align.saturating_sub(1);
    value.saturating_add(mask) & !mask
}

#[derive(Debug, Clone, Copy)]
struct CommonTotals {
    common: u32,
    rcommon: u32,
    rlcommon: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SymbolClass {
    Common,
    RCommon,
    RLCommon,
    Other,
}

#[derive(Debug, Clone)]
struct MergedSymbol {
    class: SymbolClass,
    size: u32,
}

fn merge_common_symbols(objects: &[ObjectSummary]) -> (CommonTotals, LayoutDiagnostics) {
    let mut totals = CommonTotals {
        common: 0,
        rcommon: 0,
        rlcommon: 0,
    };
    let mut diagnostics = LayoutDiagnostics::default();
    let mut merged = HashMap::<Vec<u8>, MergedSymbol>::new();

    for obj in objects {
        for sym in &obj.symbols {
            let class = classify(sym.section);
            let new_size = align_even(sym.value);
            let key = sym.name.clone();

            match merged.get_mut(&key) {
                None => {
                    if class != SymbolClass::Other {
                        add_total(&mut totals, class, new_size);
                    }
                    merged.insert(
                        key,
                        MergedSymbol {
                            class,
                            size: new_size,
                        },
                    );
                }
                Some(existing) => match (existing.class, class) {
                    (SymbolClass::Common, SymbolClass::Common)
                    | (SymbolClass::RCommon, SymbolClass::RCommon)
                    | (SymbolClass::RLCommon, SymbolClass::RLCommon) => {
                        if new_size > existing.size {
                            add_total(&mut totals, class, new_size - existing.size);
                            existing.size = new_size;
                        }
                    }
                    (
                        SymbolClass::Common | SymbolClass::RCommon | SymbolClass::RLCommon,
                        SymbolClass::Other,
                    ) => {
                        sub_total(&mut totals, existing.class, existing.size);
                        existing.class = SymbolClass::Other;
                        existing.size = new_size;
                    }
                    (
                        SymbolClass::Other,
                        SymbolClass::Common | SymbolClass::RCommon | SymbolClass::RLCommon,
                    ) => {
                        diagnostics.common_warnings += 1;
                    }
                    (SymbolClass::Common | SymbolClass::RLCommon, SymbolClass::RCommon)
                    | (SymbolClass::Common | SymbolClass::RCommon, SymbolClass::RLCommon)
                    | (SymbolClass::RCommon | SymbolClass::RLCommon, SymbolClass::Common) => {
                        diagnostics.common_conflicts += 1;
                    }
                    (SymbolClass::Other, SymbolClass::Other) => {}
                },
            }
        }
    }

    (totals, diagnostics)
}

fn classify(section: SectionKind) -> SymbolClass {
    match section {
        SectionKind::Common => SymbolClass::Common,
        SectionKind::RCommon => SymbolClass::RCommon,
        SectionKind::RLCommon => SymbolClass::RLCommon,
        _ => SymbolClass::Other,
    }
}

fn add_total(totals: &mut CommonTotals, class: SymbolClass, value: u32) {
    match class {
        SymbolClass::Common => totals.common = totals.common.saturating_add(value),
        SymbolClass::RCommon => totals.rcommon = totals.rcommon.saturating_add(value),
        SymbolClass::RLCommon => totals.rlcommon = totals.rlcommon.saturating_add(value),
        SymbolClass::Other => {}
    }
}

fn sub_total(totals: &mut CommonTotals, class: SymbolClass, value: u32) {
    match class {
        SymbolClass::Common => totals.common = totals.common.saturating_sub(value),
        SymbolClass::RCommon => totals.rcommon = totals.rcommon.saturating_sub(value),
        SymbolClass::RLCommon => totals.rlcommon = totals.rlcommon.saturating_sub(value),
        SymbolClass::Other => {}
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use crate::layout::plan_layout;
    use crate::resolver::{ObjectSummary, SectionKind};

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
            symbols: Vec::new(),
            xrefs: Vec::new(),
            requests: Vec::new(),
            start_address: None,
        }
    }

    #[test]
    fn places_objects_with_per_object_alignment() {
        let objects = vec![mk_summary(4, 3, 2), mk_summary(16, 5, 4), mk_summary(2, 0, 6)];
        let plan = plan_layout(&objects);

        assert_eq!(
            plan.placements[0].by_section.get(&SectionKind::Text),
            Some(&0)
        );
        assert_eq!(
            plan.placements[1].by_section.get(&SectionKind::Text),
            Some(&16)
        );
        assert_eq!(plan.total_size_by_section.get(&SectionKind::Text), Some(&22));

        assert_eq!(
            plan.placements[0].by_section.get(&SectionKind::Data),
            Some(&0)
        );
        assert_eq!(
            plan.placements[1].by_section.get(&SectionKind::Data),
            Some(&16)
        );
        assert_eq!(
            plan.placements[2].by_section.get(&SectionKind::Data),
            Some(&20)
        );
        assert_eq!(plan.total_size_by_section.get(&SectionKind::Data), Some(&26));
    }

    #[test]
    fn merges_common_labels_with_hlk_style_rules() {
        let mut a = mk_summary(2, 0, 0);
        a.symbols.push(crate::resolver::Symbol {
            name: b"_buf".to_vec(),
            section: SectionKind::Common,
            value: 3,
        });
        a.symbols.push(crate::resolver::Symbol {
            name: b"_rbuf".to_vec(),
            section: SectionKind::RCommon,
            value: 4,
        });

        let mut b = mk_summary(2, 0, 0);
        b.symbols.push(crate::resolver::Symbol {
            name: b"_buf".to_vec(),
            section: SectionKind::Common,
            value: 9,
        });
        b.symbols.push(crate::resolver::Symbol {
            name: b"_buf".to_vec(),
            section: SectionKind::Text,
            value: 0x100,
        });
        b.symbols.push(crate::resolver::Symbol {
            name: b"_rbuf".to_vec(),
            section: SectionKind::RLCommon,
            value: 8,
        });

        let plan = plan_layout(&[a, b]);
        assert_eq!(plan.total_size_by_section.get(&SectionKind::Common), Some(&0));
        assert_eq!(
            plan.total_size_by_section.get(&SectionKind::RCommon),
            Some(&4)
        );
        assert_eq!(
            plan.total_size_by_section.get(&SectionKind::RLCommon),
            Some(&0)
        );
        assert_eq!(plan.diagnostics.common_conflicts, 1);
        assert_eq!(plan.diagnostics.common_warnings, 0);
    }
}
