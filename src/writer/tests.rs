    use std::collections::{BTreeMap, HashMap};

    use super::expr::{classify_expression_errors, evaluate_a0};
    use super::ExprEntry;

    use crate::format::obj::{Command, ObjectFile};
    use crate::layout::plan_layout;
    use crate::resolver::{ObjectSummary, SectionKind, Symbol, resolve_object};
    use crate::writer::{
        MapSizes, apply_x_header_options, build_map_text, build_r_payload, build_x_image,
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

        let payload = build_r_payload(&[obj0, obj1], &[sum0, sum1], &layout, true, false).expect("payload");
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
        let layout = plan_layout(std::slice::from_ref(&sum));
        let with_bss = build_r_payload(
            std::slice::from_ref(&obj),
            std::slice::from_ref(&sum),
            &layout,
            false,
            false,
        )
        .expect("r with bss");
        let without_bss = build_r_payload(&[obj], &[sum], &layout, true, false).expect("r without bss");
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

        let layout = plan_layout(std::slice::from_ref(&sum));
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
        let with_symbols = build_x_image_with_options(
            std::slice::from_ref(&obj),
            std::slice::from_ref(&sum),
            &layout,
            true,
            false,
        )
        .expect("x image");
        let without_symbols = build_x_image_with_options(&[obj], &[sum], &layout, false, false).expect("x image");

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
        let text = build_map_text("a.x", &[s0, s1], &layout, MapSizes::new(4, 2, 0, 0), &[]);
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
        let layout = plan_layout(std::slice::from_ref(&sum));
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
        let layout = plan_layout(std::slice::from_ref(&sum));
        let err = validate_r_convertibility(&[obj], &[sum], &layout, "out.r", false)
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
        let layout = plan_layout(std::slice::from_ref(&sum));
        let err = validate_r_convertibility(&[obj], &[sum], &layout, "out.r", false)
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
        let layout = plan_layout(std::slice::from_ref(&sum));
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
            validate_link_inputs(&[obj], &[], &[mk_summary(2, 0, 0)], true).expect_err("must reject expression command");
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
        let image = build_x_image_with_options(&[sys.clone(), app], &[sum0, sum1], &layout, false, false)
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
        let err = build_x_image_with_options(&[obj], &[sum], &layout, false, false).expect_err("must fail");
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
        let err = validate_link_inputs(&[obj], &[], &[mk_summary(2, 4, 0)], true).expect_err("must reject");
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
        let err = validate_link_inputs(&[obj], &[], &[mk_summary(2, 4, 0)], true).expect_err("must reject");
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
        let err = validate_link_inputs(&[obj], &[], &[mk_summary(2, 4, 0)], true).expect_err("must reject");
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
        let err = validate_link_inputs(&[obj], &[], &[mk_summary(2, 4, 0)], true).expect_err("must reject");
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
            build_x_image_with_options(&[obj0, obj1], &[sum0, sum1], &layout, false, false).expect_err("must fail");
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
        let err = build_x_image_with_options(&[obj], &[sum], &layout, false, false).expect_err("must fail");
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
            validate_link_inputs(&[obj], &[], &[mk_summary(2, 0, 0)], true)
                .expect("doctor/dodtor should be accepted as no-op");
        }
    }

    #[test]
    fn rejects_ctor_dtor_when_g2lk_is_off() {
        let obj = ObjectFile {
            commands: vec![
                Command::Header {
                    section: 0x01,
                    size: 4,
                    name: b"text".to_vec(),
                },
                Command::Opaque {
                    code: 0x4c01,
                    payload: vec![0, 0, 0, 0],
                },
                Command::Opaque {
                    code: 0xe00c,
                    payload: Vec::new(),
                },
                Command::End,
            ],
            scd_tail: Vec::new(),
        };
        let err = validate_link_inputs(&[obj], &[], &[mk_summary(2, 4, 0)], false).expect_err("must reject");
        assert!(err.to_string().contains("-1 オプション"));
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
            validate_link_inputs(&[obj], &[], &[mk_summary(2, 0, 0)], true)
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
    fn rebases_scd_info_bss_value_with_obj_size_rule() {
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
            // linfo=0, sinfo+einfo=18, ninfo=0, sinfo_count=1
            // entry: val=10, sect=3(bss)
            scd_tail: vec![
                0, 0, 0, 0, 0, 0, 0, 18, 0, 0, 0, 0, //
                b'_', b'b', 0, 0, 0, 0, 0, 0, //
                0, 0, 0, 10, //
                0, 3, //
                0, 0, //
                0, 0,
            ],
        };
        let mut sum = mk_summary(2, 2, 2);
        sum.declared_section_sizes.insert(SectionKind::Bss, 4);
        let layout = plan_layout(std::slice::from_ref(&sum));
        let image = build_x_image(std::slice::from_ref(&obj), std::slice::from_ref(&sum), &layout)
            .expect("x image");

        let text_size = u32::from_be_bytes([image[12], image[13], image[14], image[15]]) as usize;
        let data_size = u32::from_be_bytes([image[16], image[17], image[18], image[19]]) as usize;
        let reloc_size = u32::from_be_bytes([image[24], image[25], image[26], image[27]]) as usize;
        let sym_size = u32::from_be_bytes([image[28], image[29], image[30], image[31]]) as usize;
        let line_size = u32::from_be_bytes([image[32], image[33], image[34], image[35]]) as usize;
        let scd_info_pos = 64 + text_size + data_size + reloc_size + sym_size + line_size;
        // bss delta: bss_pos - obj_size = (text+data+0) - (text+data+bss) = -4
        // 10 + (-4) = 6
        assert_eq!(&image[scd_info_pos + 8..scd_info_pos + 12], &[0, 0, 0, 6]);
    }

    #[test]
    fn rejects_scd_sinfo_stack_section() {
        let obj = ObjectFile {
            commands: vec![Command::End],
            // sinfo_count=1, sect=4(stack)
            scd_tail: vec![
                0, 0, 0, 0, 0, 0, 0, 18, 0, 0, 0, 0, //
                b'_', b's', 0, 0, 0, 0, 0, 0, //
                0, 0, 0, 1, //
                0, 4, //
                0, 0, //
                0, 0,
            ],
        };
        let sum = mk_summary(2, 0, 0);
        let layout = plan_layout(std::slice::from_ref(&sum));
        let err = build_x_image(std::slice::from_ref(&obj), std::slice::from_ref(&sum), &layout)
            .expect_err("must reject");
        assert!(err.to_string().contains("unsupported SCD sinfo section"));
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
        let layout = plan_layout(std::slice::from_ref(&sum));
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
    #[allow(clippy::too_many_lines)]
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
        let layout = plan_layout(std::slice::from_ref(&sum));
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
        let layout = plan_layout(std::slice::from_ref(&sum));
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
        let layout = plan_layout(std::slice::from_ref(&sum));
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
                    code: 0x6b00 | u16::from(lo),
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
                    size: super::usize_to_u32_saturating(expected.len()),
                    name: b"text".to_vec(),
                },
                Command::ChangeSection { section: 0x01 },
                Command::Opaque {
                    code: (u16::from(code_hi) << 8) | u16::from(lo),
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
        let mut s0 = mk_summary(2, super::usize_to_u32_saturating(expected.len()), 0);
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
