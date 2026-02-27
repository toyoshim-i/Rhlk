use std::collections::HashMap;

use crate::format::obj::Command;
use crate::resolver::{ObjectSummary, SectionKind, Symbol};

use super::{ExprEntry, opcode, read_i32_be, read_u16_be};

pub(super) fn classify_expression_errors(
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
        opcode::OPH_WRT_STK_BYTE => evaluate_wrt_stk_9000(calc_stack),
        opcode::OPH_WRT_STK_WORD_TEXT => evaluate_wrt_stk_9100(calc_stack, current),
        opcode::OPH_WRT_STK_LONG => evaluate_wrt_stk_long(calc_stack),
        opcode::OPH_WRT_STK_BYTE_RAW => evaluate_wrt_stk_9300(calc_stack),
        opcode::OPH_WRT_STK_LONG_ALT => evaluate_wrt_stk_long(calc_stack),
        opcode::OPH_WRT_STK_WORD_RELOC => evaluate_wrt_stk_9900(calc_stack, current),
        opcode::OPH_WRT_STK_LONG_RELOC => evaluate_wrt_stk_long(calc_stack),
        0x40 | 0x43 => evaluate_direct_byte(lo, payload, summary, global_symbols),
        0x50 | 0x53 => evaluate_direct_byte_with_offset(lo, payload, summary, global_symbols),
        0x41 => evaluate_direct_word(lo, payload, summary, global_symbols, current),
        0x51 => evaluate_direct_word_with_offset(lo, payload, summary, global_symbols, current),
        0x65 => evaluate_rel_word(payload, summary, global_symbols),
        0x6a => evaluate_d32_adrs(payload, summary, global_symbols),
        0x6b => evaluate_rel_byte(payload, summary, global_symbols),
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

pub(super) fn evaluate_a0(lo: u8, calc_stack: &mut Vec<ExprEntry>) -> Vec<&'static str> {
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

pub(super) fn section_stat(section: SectionKind) -> i16 {
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

fn fits_byte(v: i32) -> bool {
    (-0x80..=0xff).contains(&v)
}

fn fits_word(v: i32) -> bool {
    (-0x8000..=0xffff).contains(&v)
}

fn fits_word2(v: i32) -> bool {
    (-0x8000..=0x7fff).contains(&v)
}
