use std::collections::HashMap;

use crate::format::obj::Command;
use crate::resolver::{ObjectSummary, SectionKind, Symbol};

use super::{ExprEntry, is_abs_section, is_common_or_xref_section, is_xref_section, opcode, read_i32_be, read_u16_be, reloc_section_kind};

pub(super) fn classify_expression_errors(
    code: u16,
    cmd: &Command,
    summary: &ObjectSummary,
    global_symbols: &HashMap<Vec<u8>, Symbol>,
    current: SectionKind,
    calc_stack: &mut Vec<ExprEntry>,
) -> Vec<&'static str> {
    const CALC_STACK_SIZE_HLK: usize = 1024;
    let [hi, lo] = code.to_be_bytes();
    let payload = match cmd {
        Command::Opaque { payload, .. } => payload.as_slice(),
        _ => &[],
    };
    match hi {
        opcode::OPH_PUSH_VALUE_BASE => {
            if calc_stack.len() >= CALC_STACK_SIZE_HLK {
                return vec!["計算用スタックが溢れました"];
            }
            if let Some(entry) = evaluate_push_80(lo, payload, summary, global_symbols) {
                calc_stack.push(entry);
            }
            Vec::new()
        }
        opcode::OPH_EXPR_BASE => evaluate_a0(lo, calc_stack),
        opcode::OPH_WRT_STK_BYTE => evaluate_wrt_stk_9000(calc_stack),
        opcode::OPH_WRT_STK_WORD_TEXT => evaluate_wrt_stk_9100(calc_stack, current),
        opcode::OPH_WRT_STK_LONG | opcode::OPH_WRT_STK_LONG_ALT | opcode::OPH_WRT_STK_LONG_RELOC => {
            evaluate_wrt_stk_long(calc_stack)
        }
        opcode::OPH_WRT_STK_BYTE_RAW => evaluate_wrt_stk_9300(calc_stack),
        opcode::OPH_WRT_STK_WORD_RELOC => evaluate_wrt_stk_9900(calc_stack, current),
        opcode::OPH_ABS_WORD | opcode::OPH_ABS_BYTE => {
            evaluate_direct_byte(lo, payload, summary, global_symbols)
        }
        opcode::OPH_ADD_WORD | opcode::OPH_ADD_BYTE => {
            evaluate_direct_byte_with_offset(lo, payload, summary, global_symbols)
        }
        opcode::OPH_ABS_WORD_ALT => evaluate_direct_word(lo, payload, summary, global_symbols, current),
        opcode::OPH_ADD_WORD_ALT => {
            evaluate_direct_word_with_offset(lo, payload, summary, global_symbols, current)
        }
        opcode::OPH_DISP_WORD => evaluate_rel_word(payload, summary, global_symbols),
        opcode::OPH_DISP_LONG => evaluate_d32_adrs(payload, summary, global_symbols),
        opcode::OPH_DISP_BYTE => evaluate_rel_byte(payload, summary, global_symbols),
        _ => Vec::new(),
    }
}

fn evaluate_push_80(
    lo: u8,
    payload: &[u8],
    summary: &ObjectSummary,
    global_symbols: &HashMap<Vec<u8>, Symbol>,
) -> Option<ExprEntry> {
    if is_common_or_xref_section(lo) {
        let label_no = read_u16_be(payload)?;
        let (section, value) = resolve_xref(label_no, summary, global_symbols)?;
        let stat = match section_stat(section) {
            0 => 0,
            1 => 1,
            _ => 2,
        };
        return Some(ExprEntry { stat, value });
    }
    if reloc_section_kind(lo).is_some() || is_abs_section(lo) {
        let value = read_i32_be(payload)?;
        let stat = match lo {
            s if is_abs_section(s) => 0,
            0x01..=0x04 => 1,
            _ => 2,
        };
        return Some(ExprEntry { stat, value });
    }
    None
}

pub(super) fn evaluate_a0(lo: u8, calc_stack: &mut Vec<ExprEntry>) -> Vec<&'static str> {
    const STACK_UNDERFLOW: &str = "計算用スタックに値がありません";
    match lo {
        0x02 => {
            let Some(entry) = calc_stack.pop() else {
                return vec![STACK_UNDERFLOW];
            };
            calc_stack.push(entry);
            Vec::new()
        }
        0x01 | 0x03 | 0x04 | 0x05 | 0x06 | 0x07 => {
            let Ok(entry) = pop_unary(calc_stack, STACK_UNDERFLOW) else {
                return vec![STACK_UNDERFLOW];
            };
            let (result, errors) = evaluate_a0_unary(lo, entry);
            calc_stack.push(result);
            errors
        }
        0x0a | 0x0b => {
            let Ok((top, next)) = pop_binary(calc_stack, STACK_UNDERFLOW) else {
                return vec![STACK_UNDERFLOW];
            };
            let (result, errors) = evaluate_a0_div_mod(lo, top, next);
            if let Some(result) = result {
                calc_stack.push(result);
            }
            errors
        }
        0x0f => {
            let Ok((top, next)) = pop_binary(calc_stack, STACK_UNDERFLOW) else {
                return vec![STACK_UNDERFLOW];
            };
            let (result, errors) = evaluate_a0_sub(top, next);
            calc_stack.push(result);
            errors
        }
        0x10 => {
            let Ok((top, next)) = pop_binary(calc_stack, STACK_UNDERFLOW) else {
                return vec![STACK_UNDERFLOW];
            };
            let (result, errors) = evaluate_a0_add(top, next);
            calc_stack.push(result);
            errors
        }
        0x09 | 0x0c | 0x0d | 0x0e | 0x11..=0x1d => {
            let Ok((top, next)) = pop_binary(calc_stack, STACK_UNDERFLOW) else {
                return vec![STACK_UNDERFLOW];
            };
            let (result, errors) = evaluate_a0_binary(lo, top, next);
            if let Some(result) = result {
                calc_stack.push(result);
            }
            errors
        }
        _ => Vec::new(),
    }
}

fn pop_unary(calc_stack: &mut Vec<ExprEntry>, underflow_message: &'static str) -> Result<ExprEntry, &'static str> {
    calc_stack.pop().ok_or(underflow_message)
}

fn pop_binary(
    calc_stack: &mut Vec<ExprEntry>,
    underflow_message: &'static str,
) -> Result<(ExprEntry, ExprEntry), &'static str> {
    // Arithmetic opcodes consume stack as: next (left operand), then top (right operand).
    // Return order is (top, next) to preserve this relationship explicitly at call sites.
    let Some(top) = calc_stack.pop() else {
        return Err(underflow_message);
    };
    let Some(next) = calc_stack.pop() else {
        calc_stack.push(top);
        return Err(underflow_message);
    };
    Ok((top, next))
}

fn evaluate_a0_unary(lo: u8, mut entry: ExprEntry) -> (ExprEntry, Vec<&'static str>) {
    if entry.stat > 0 {
        entry.stat = -1;
        return (entry, vec!["不正な式"]);
    }
    if entry.stat == 0 {
        entry.value = match lo {
            0x01 => entry.value.wrapping_neg(),
            0x03 => {
                if entry.value == 0 {
                    -1
                } else {
                    0
                }
            }
            0x04 => i32::from((((entry.value.cast_unsigned() & 0xffff) >> 8) as u16).cast_signed()),
            0x05 => (entry.value.cast_unsigned() & 0xff).cast_signed(),
            0x06 => (entry.value.cast_unsigned() >> 16).cast_signed(),
            0x07 => (entry.value.cast_unsigned() & 0xffff).cast_signed(),
            _ => entry.value,
        };
    }
    (entry, Vec::new())
}

fn evaluate_a0_div_mod(
    lo: u8,
    top: ExprEntry,
    next: ExprEntry,
) -> (Option<ExprEntry>, Vec<&'static str>) {
    let (res, errors) = eval_chk_calcexp2(top, next);
    let Some(mut result) = res else {
        return (None, errors);
    };
    if result.stat >= 0 && top.value == 0 {
        result.stat = -1;
        let mut out = errors;
        out.push("ゼロ除算");
        return (Some(result), out);
    }
    if result.stat == 0 {
        if lo == 0x0a {
            result.value = next.value.wrapping_div(top.value);
        } else {
            // HLK's divs_d0d1 leaves abs(remainder).
            result.value = next.value.wrapping_rem(top.value).abs();
        }
    }
    (Some(result), errors)
}

fn evaluate_a0_sub(top: ExprEntry, next: ExprEntry) -> (ExprEntry, Vec<&'static str>) {
    let mut errors = Vec::new();
    let mut out = ExprEntry {
        stat: -1,
        value: next.value.wrapping_sub(top.value),
    };
    if top.stat == 0 {
        out.stat = next.stat ^ top.stat;
    } else if top.stat < 0 || next.stat < 0 {
        out.stat = -1;
    } else if top.stat != next.stat {
        errors.push("不正な式");
    } else {
        out.stat = next.stat ^ top.stat;
    }
    (out, errors)
}

fn evaluate_a0_add(top: ExprEntry, next: ExprEntry) -> (ExprEntry, Vec<&'static str>) {
    let mut errors = Vec::new();
    let mut out = ExprEntry {
        stat: -1,
        value: next.value.wrapping_add(top.value),
    };
    if top.stat == 0 {
        out.stat = next.stat ^ top.stat;
    } else if top.stat < 0 {
        out.stat = -1;
    } else if next.stat == 0 {
        out.stat = next.stat ^ top.stat;
    } else {
        if next.stat >= 0 {
            errors.push("不正な式");
        }
        out.stat = -1;
    }
    (out, errors)
}

fn evaluate_a0_binary(
    lo: u8,
    top: ExprEntry,
    next: ExprEntry,
) -> (Option<ExprEntry>, Vec<&'static str>) {
    let (res, errors) = eval_chk_calcexp2(top, next);
    let Some(mut result) = res else {
        return (None, errors);
    };
    if result.stat == 0 {
        result.value = eval_a0_const_binop(lo, next.value, top.value);
    }
    (Some(result), errors)
}

fn eval_a0_const_binop(lo: u8, b: i32, a: i32) -> i32 {
    match lo {
        0x09 => b.wrapping_mul(a),
        0x0c => ((b.cast_unsigned()) >> (a.cast_unsigned() & 63)).cast_signed(),
        0x0d => ((b.cast_unsigned()) << (a.cast_unsigned() & 63)).cast_signed(),
        0x0e => b.wrapping_shr(a.cast_unsigned() & 63),
        0x11 => if b == a { -1 } else { 0 },
        0x12 => if b == a { 0 } else { -1 },
        0x13 => if b.cast_unsigned() < a.cast_unsigned() { -1 } else { 0 },
        0x14 => if b.cast_unsigned() <= a.cast_unsigned() { -1 } else { 0 },
        0x15 => if b.cast_unsigned() > a.cast_unsigned() { -1 } else { 0 },
        0x16 => if b.cast_unsigned() >= a.cast_unsigned() { -1 } else { 0 },
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
    if is_base_section(current) {
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
    if v.stat == 1 || !is_base_section(current) {
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
    match direct_xref_label_no(lo, payload) {
        Err(()) => return vec!["アドレス属性シンボルの値をバイトサイズで出力"],
        Ok(Some(label_no)) => {
            if let Some((section, value)) = resolve_xref(label_no, summary, global_symbols) {
                return validate_direct_byte_xref(section, value);
            }
            return Vec::new();
        }
        Ok(None) => {}
    }
    let Some(value) = read_i32_be(payload) else {
        return Vec::new();
    };
    validate_direct_byte_const(value)
}

fn evaluate_direct_byte_with_offset(
    lo: u8,
    payload: &[u8],
    summary: &ObjectSummary,
    global_symbols: &HashMap<Vec<u8>, Symbol>,
) -> Vec<&'static str> {
    match direct_xref_label_no(lo, payload) {
        Err(()) => return vec!["アドレス属性シンボルの値をバイトサイズで出力"],
        Ok(Some(label_no)) => {
            let offset = read_i32_be(&payload[2..]).unwrap_or(0);
            if let Some((section, value)) = resolve_xref(label_no, summary, global_symbols) {
                return validate_direct_byte_xref(section, value.wrapping_add(offset));
            }
            return Vec::new();
        }
        Ok(None) => {}
    }
    let Some(value) = read_i32_be(payload) else {
        return Vec::new();
    };
    let offset = read_i32_be(&payload[4..]).unwrap_or(0);
    validate_direct_byte_const(value.wrapping_add(offset))
}

fn evaluate_direct_word(
    lo: u8,
    payload: &[u8],
    summary: &ObjectSummary,
    global_symbols: &HashMap<Vec<u8>, Symbol>,
    current: SectionKind,
) -> Vec<&'static str> {
    match direct_xref_label_no(lo, payload) {
        Err(()) => return vec!["アドレス属性シンボルの値をワードサイズで出力"],
        Ok(Some(label_no)) => {
            if let Some((section, value)) = resolve_xref(label_no, summary, global_symbols) {
                return validate_direct_word_xref(section, value, current);
            }
            return Vec::new();
        }
        Ok(None) => {}
    }
    let Some(value) = read_i32_be(payload) else {
        return Vec::new();
    };
    validate_direct_word_const(value)
}

fn evaluate_direct_word_with_offset(
    lo: u8,
    payload: &[u8],
    summary: &ObjectSummary,
    global_symbols: &HashMap<Vec<u8>, Symbol>,
    current: SectionKind,
) -> Vec<&'static str> {
    match direct_xref_label_no(lo, payload) {
        Err(()) => return vec!["アドレス属性シンボルの値をワードサイズで出力"],
        Ok(Some(label_no)) => {
            let offset = read_i32_be(&payload[2..]).unwrap_or(0);
            if let Some((section, value)) = resolve_xref(label_no, summary, global_symbols) {
                return validate_direct_word_xref(section, value.wrapping_add(offset), current);
            }
            return Vec::new();
        }
        Ok(None) => {}
    }
    let Some(value) = read_i32_be(payload) else {
        return Vec::new();
    };
    let offset = read_i32_be(&payload[4..]).unwrap_or(0);
    validate_direct_word_const(value.wrapping_add(offset))
}

fn direct_xref_label_no(lo: u8, payload: &[u8]) -> Result<Option<u16>, ()> {
    if !is_common_or_xref_section(lo) {
        return Ok(None);
    }
    if !is_xref_section(lo) {
        return Err(());
    }
    Ok(read_u16_be(payload))
}

fn validate_direct_byte_const(value: i32) -> Vec<&'static str> {
    if !fits_byte(value) {
        return vec!["バイトサイズ(-$80〜$ff)で表現できない値"];
    }
    Vec::new()
}

fn validate_direct_byte_xref(section: SectionKind, value: i32) -> Vec<&'static str> {
    if section_stat(section) != 0 {
        return vec!["アドレス属性シンボルの値をバイトサイズで出力"];
    }
    validate_direct_byte_const(value)
}

fn validate_direct_word_const(value: i32) -> Vec<&'static str> {
    if !fits_word(value) {
        return vec!["ワードサイズ(-$8000〜$ffff)で表現できない値"];
    }
    Vec::new()
}

fn validate_direct_word_xref(section: SectionKind, value: i32, current: SectionKind) -> Vec<&'static str> {
    let stat = section_stat(section);
    if stat == 0 {
        return validate_direct_word_const(value);
    }
    if stat == 1 {
        return vec!["アドレス属性シンボルの値をワードサイズで出力"];
    }
    if is_base_section(current) {
        if !fits_word2(value) {
            return vec!["ワードサイズ(-$8000〜$7fff)で表現できない値"];
        }
        return Vec::new();
    }
    vec!["アドレス属性シンボルの値をワードサイズで出力"]
}

fn resolve_xref(
    label_no: u16,
    summary: &ObjectSummary,
    global_symbols: &HashMap<Vec<u8>, Symbol>,
) -> Option<(SectionKind, i32)> {
    let xref = summary
        .xrefs
        .iter()
        .find(|x| x.value == u32::from(label_no))?;
    let target = global_symbols.get(&xref.name)?;
    Some((target.section, u32_bits_to_i32(target.value)))
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

fn is_base_section(section: SectionKind) -> bool {
    matches!(
        section,
        SectionKind::Text | SectionKind::Data | SectionKind::Bss | SectionKind::Stack
    )
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

fn u32_bits_to_i32(value: u32) -> i32 {
    i32::from_be_bytes(value.to_be_bytes())
}
