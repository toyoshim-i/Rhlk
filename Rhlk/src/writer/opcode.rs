pub const OP_CTOR_ENTRY: u16 = 0x4c01;
pub const OP_DTOR_ENTRY: u16 = 0x4d01;
pub const OP_DOCTOR: u16 = 0xe00c;
pub const OP_DODTOR: u16 = 0xe00d;

pub const OPH_PUSH_VALUE_BASE: u8 = 0x80;
pub const OPH_EXPR_BASE: u8 = 0xa0;

pub const OPH_ABS_WORD: u8 = 0x40;
pub const OPH_ABS_LONG: u8 = 0x42;
pub const OPH_ABS_BYTE: u8 = 0x43;
pub const OPH_XREF_WORD: u8 = 0x45;
pub const OPH_XREF_LONG: u8 = 0x46;
pub const OPH_XREF_BYTE: u8 = 0x47;
pub const OPH_ADD_WORD: u8 = 0x50;
pub const OPH_ADD_LONG: u8 = 0x52;
pub const OPH_ADD_BYTE: u8 = 0x53;
pub const OPH_ADD_XREF_WORD: u8 = 0x55;
pub const OPH_ADD_XREF_LONG: u8 = 0x56;
pub const OPH_ADD_XREF_BYTE: u8 = 0x57;
pub const OPH_DISP_WORD: u8 = 0x65;
pub const OPH_DISP_WORD_ALIAS: u8 = 0x69;
pub const OPH_DISP_LONG: u8 = 0x6a;
pub const OPH_DISP_BYTE: u8 = 0x6b;

pub const OPH_WRT_STK_BYTE: u8 = 0x90;
pub const OPH_WRT_STK_WORD_TEXT: u8 = 0x91;
pub const OPH_WRT_STK_LONG: u8 = 0x92;
pub const OPH_WRT_STK_BYTE_RAW: u8 = 0x93;
pub const OPH_WRT_STK_LONG_ALT: u8 = 0x96;
pub const OPH_WRT_STK_WORD_RELOC: u8 = 0x99;
pub const OPH_WRT_STK_LONG_RELOC: u8 = 0x9a;
