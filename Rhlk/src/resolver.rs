use std::collections::BTreeMap;

use crate::format::obj::{Command, ObjectFile};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum SectionKind {
    Abs,
    Text,
    Data,
    Bss,
    Stack,
    RData,
    RBss,
    RStack,
    RLData,
    RLBss,
    RLStack,
    Common,
    RCommon,
    RLCommon,
    Xref,
    Unknown(u8),
}

impl SectionKind {
    #[must_use]
    pub fn from_u8(section: u8) -> Self {
        match section {
            0x00 => Self::Abs,
            0x01 => Self::Text,
            0x02 => Self::Data,
            0x03 => Self::Bss,
            0x04 => Self::Stack,
            0x05 => Self::RData,
            0x06 => Self::RBss,
            0x07 => Self::RStack,
            0x08 => Self::RLData,
            0x09 => Self::RLBss,
            0x0a => Self::RLStack,
            0xfc => Self::RLCommon,
            0xfd => Self::RCommon,
            0xfe => Self::Common,
            0xff => Self::Xref,
            _ => Self::Unknown(section),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Symbol {
    pub name: Vec<u8>,
    pub section: SectionKind,
    pub value: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObjectSummary {
    pub object_align: u32,
    pub declared_section_sizes: BTreeMap<SectionKind, u32>,
    pub observed_section_usage: BTreeMap<SectionKind, u32>,
    pub symbols: Vec<Symbol>,
    pub xrefs: Vec<Symbol>,
    pub requests: Vec<Vec<u8>>,
    pub start_address: Option<(u16, u32)>,
}

#[must_use]
pub fn resolve_object(object: &ObjectFile) -> ObjectSummary {
    let mut object_align = 2;
    let mut declared_section_sizes = BTreeMap::new();
    let mut observed_section_usage = BTreeMap::new();
    let mut symbols = Vec::new();
    let mut xrefs = Vec::new();
    let mut requests = Vec::new();
    let mut start_address = None;

    let mut current_section = SectionKind::Text;

    for cmd in &object.commands {
        match cmd {
            Command::Header { section, size, .. } => {
                declared_section_sizes.insert(SectionKind::from_u8(*section), *size);
            }
            Command::ChangeSection { section } => {
                current_section = SectionKind::from_u8(*section);
            }
            Command::RawData(bytes) => {
                bump_usage(
                    &mut observed_section_usage,
                    current_section,
                    usize_to_u32_saturating(bytes.len()),
                );
            }
            Command::DefineSpace { size } => {
                bump_usage(&mut observed_section_usage, current_section, *size);
            }
            Command::DefineSymbol {
                section,
                value,
                name,
            } => {
                if *section != 0xff && name.first() == Some(&b'*') {
                    let align = 1u32.checked_shl(*value).unwrap_or(0);
                    if (2..=256).contains(&align) {
                        object_align = align;
                    }
                }
                let symbol = Symbol {
                    name: name.clone(),
                    section: SectionKind::from_u8(*section),
                    value: *value,
                };
                if *section == 0xff {
                    xrefs.push(symbol);
                } else {
                    symbols.push(symbol);
                }
            }
            Command::Request { file_name } => requests.push(file_name.clone()),
            Command::StartAddress { section, address } => {
                start_address = Some((*section, *address));
            }
            Command::SourceFile { .. } | Command::Opaque { .. } | Command::End => {}
        }
    }

    ObjectSummary {
        object_align,
        declared_section_sizes,
        observed_section_usage,
        symbols,
        xrefs,
        requests,
        start_address,
    }
}

fn usize_to_u32_saturating(value: usize) -> u32 {
    u32::try_from(value).unwrap_or(u32::MAX)
}

fn bump_usage(map: &mut BTreeMap<SectionKind, u32>, section: SectionKind, amount: u32) {
    let entry = map.entry(section).or_insert(0);
    *entry = entry.saturating_add(amount);
}

#[cfg(test)]
mod tests {
    use crate::format::obj::{Command, ObjectFile};
    use crate::resolver::{resolve_object, SectionKind};

    #[test]
    fn collects_sizes_symbols_and_requests() {
        let object = ObjectFile {
            commands: vec![
                Command::Header {
                    section: 0x01,
                    size: 0x30,
                    name: b"text".to_vec(),
                },
                Command::Header {
                    section: 0x02,
                    size: 0x10,
                    name: b"data".to_vec(),
                },
                Command::ChangeSection { section: 0x01 },
                Command::RawData(vec![1, 2, 3]),
                Command::DefineSpace { size: 5 },
                Command::ChangeSection { section: 0x02 },
                Command::RawData(vec![4, 5]),
                Command::DefineSymbol {
                    section: 0x01,
                    value: 0x20,
                    name: b"_entry".to_vec(),
                },
                Command::DefineSymbol {
                    section: 0xff,
                    value: 1,
                    name: b"_puts".to_vec(),
                },
                Command::Request {
                    file_name: b"libc.a".to_vec(),
                },
                Command::StartAddress {
                    section: 0x01,
                    address: 0x100,
                },
                Command::End,
            ],
            scd_tail: Vec::new(),
        };

        let summary = resolve_object(&object);
        assert_eq!(summary.object_align, 2);
        assert_eq!(
            summary.declared_section_sizes.get(&SectionKind::Text),
            Some(&0x30)
        );
        assert_eq!(
            summary.declared_section_sizes.get(&SectionKind::Data),
            Some(&0x10)
        );
        assert_eq!(
            summary.observed_section_usage.get(&SectionKind::Text),
            Some(&8)
        );
        assert_eq!(
            summary.observed_section_usage.get(&SectionKind::Data),
            Some(&2)
        );
        assert_eq!(summary.symbols.len(), 1);
        assert_eq!(summary.xrefs.len(), 1);
        assert_eq!(summary.requests, vec![b"libc.a".to_vec()]);
        assert_eq!(summary.start_address, Some((0x01, 0x100)));
    }

    #[test]
    fn extracts_object_align_from_special_symbol() {
        let object = ObjectFile {
            commands: vec![
                Command::DefineSymbol {
                    section: 0x01,
                    value: 4,
                    name: b"*align".to_vec(),
                },
                Command::End,
            ],
            scd_tail: Vec::new(),
        };
        let summary = resolve_object(&object);
        assert_eq!(summary.object_align, 16);
    }
}
