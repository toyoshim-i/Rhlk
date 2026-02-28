use crate::format::FormatError;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObjectFile {
    pub commands: Vec<Command>,
    pub scd_tail: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    End,
    RawData(Vec<u8>),
    ChangeSection { section: u8 },
    DefineSpace { size: u32 },
    Header {
        section: u8,
        size: u32,
        name: Vec<u8>,
    },
    SourceFile {
        size: u32,
        name: Vec<u8>,
    },
    StartAddress {
        section: u16,
        address: u32,
    },
    Request {
        file_name: Vec<u8>,
    },
    DefineSymbol {
        section: u8,
        value: u32,
        name: Vec<u8>,
    },
    Opaque {
        code: u16,
        payload: Vec<u8>,
    },
}

/// Parses one HAS/HLK object stream into structured commands.
///
/// # Errors
/// Returns `FormatError` when the stream is malformed or contains unsupported commands.
pub fn parse_object(input: &[u8]) -> Result<ObjectFile, FormatError> {
    let mut reader = Reader::new(input);
    let mut commands = Vec::new();

    while !reader.is_eof() {
        let code = reader.read_u16_be()?;
        match code {
            0x0000 => {
                commands.push(Command::End);
                break;
            }
            0x3000 => {
                let size = reader.read_u32_be()?;
                commands.push(Command::DefineSpace { size });
            }
            0xd000 => {
                let size = reader.read_u32_be()?;
                let name = reader.read_cstring_even()?;
                commands.push(Command::SourceFile { size, name });
            }
            0xe000 => {
                let section = reader.read_u16_be()?;
                let address = reader.read_u32_be()?;
                commands.push(Command::StartAddress { section, address });
            }
            0xe001 => {
                let file_name = reader.read_cstring_even()?;
                commands.push(Command::Request { file_name });
            }
            0xe00c | 0xe00d => {
                commands.push(Command::Opaque {
                    code,
                    payload: Vec::new(),
                });
            }
            _ if (code & 0xff00) == 0x1000 => {
                let size = usize::from(code.to_be_bytes()[1]) + 1;
                let data = reader.read_bytes(size)?.to_vec();
                reader.align_even();
                commands.push(Command::RawData(data));
            }
            _ if (code & 0xff00) == 0x2000 => {
                let section = code.to_be_bytes()[1];
                let _reserved = reader.read_u32_be()?;
                commands.push(Command::ChangeSection { section });
            }
            _ if (code & 0xff00) == 0xc000 => {
                let section = code.to_be_bytes()[1];
                let size = reader.read_u32_be()?;
                let name = reader.read_cstring_even()?;
                commands.push(Command::Header {
                    section,
                    size,
                    name,
                });
            }
            _ if (code & 0xff00) == 0xb200 => {
                let section = code.to_be_bytes()[1];
                let value = reader.read_u32_be()?;
                let name = reader.read_cstring_even()?;
                commands.push(Command::DefineSymbol {
                    section,
                    value,
                    name,
                });
            }
            0xb0ff => {
                let value = reader.read_u32_be()?;
                let name = reader.read_cstring_even()?;
                commands.push(Command::DefineSymbol {
                    section: 0xff,
                    value,
                    name,
                });
            }
            _ if is_supported_opaque(code) => {
                let payload = read_opaque_payload(&mut reader, code)?;
                commands.push(Command::Opaque { code, payload });
            }
            _ => return Err(FormatError::UnsupportedCommand(code)),
        }
    }

    let scd_tail = reader.remaining().to_vec();
    Ok(ObjectFile { commands, scd_tail })
}

fn is_supported_opaque(code: u16) -> bool {
    let [hi, lo] = code.to_be_bytes();
    match hi {
        0x4c | 0x4d => lo == 0x01,
        0x90 | 0x91 | 0x92 | 0x93 | 0x96 | 0x99 | 0x9a => lo == 0x00,
        0x40 | 0x41 | 0x42 | 0x43 | 0x45 | 0x46 | 0x47 | 0x50 | 0x51 | 0x52 | 0x53 | 0x55
        | 0x56 | 0x57 | 0x65 | 0x69 | 0x6a | 0x6b | 0x80 | 0xa0 => true,
        _ => false,
    }
}

fn read_opaque_payload(reader: &mut Reader<'_>, code: u16) -> Result<Vec<u8>, FormatError> {
    let [hi, lo] = code.to_be_bytes();
    let payload_len = match hi {
        0x40 | 0x41 | 0x42 | 0x43 | 0x46 | 0x80 => if is_label_section(lo) { 2 } else { 4 },
        0x45 | 0x47 => 2,
        0x4c | 0x4d => 4,
        0x50 | 0x55 | 0x57 | 0x65 | 0x69 | 0x6a | 0x6b => 6,
        0x51 | 0x52 | 0x53 | 0x56 => if is_label_section(lo) { 6 } else { 8 },
        0x90 | 0x91 | 0x92 | 0x93 | 0x96 | 0x99 | 0x9a | 0xa0 => 0,
        _ => return Err(FormatError::UnsupportedCommand(code)),
    };
    Ok(reader.read_bytes(payload_len)?.to_vec())
}

fn is_label_section(section: u8) -> bool {
    matches!(section, 0xfc..=0xff)
}

struct Reader<'a> {
    input: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn new(input: &'a [u8]) -> Self {
        Self { input, pos: 0 }
    }

    fn is_eof(&self) -> bool {
        self.pos >= self.input.len()
    }

    fn read_u16_be(&mut self) -> Result<u16, FormatError> {
        let bytes = self.read_bytes(2)?;
        Ok(u16::from_be_bytes([bytes[0], bytes[1]]))
    }

    fn read_u32_be(&mut self) -> Result<u32, FormatError> {
        let bytes = self.read_bytes(4)?;
        Ok(u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    fn read_bytes(&mut self, size: usize) -> Result<&'a [u8], FormatError> {
        if self.pos + size > self.input.len() {
            return Err(FormatError::UnexpectedEof);
        }
        let begin = self.pos;
        self.pos += size;
        Ok(&self.input[begin..self.pos])
    }

    fn read_cstring_even(&mut self) -> Result<Vec<u8>, FormatError> {
        let begin = self.pos;
        while self.pos < self.input.len() {
            if self.input[self.pos] == 0 {
                let value = self.input[begin..self.pos].to_vec();
                self.pos += 1;
                self.align_even();
                return Ok(value);
            }
            self.pos += 1;
        }
        Err(FormatError::UnterminatedString)
    }

    fn align_even(&mut self) {
        if !self.pos.is_multiple_of(2) && self.pos < self.input.len() {
            self.pos += 1;
        }
    }

    fn remaining(&self) -> &'a [u8] {
        &self.input[self.pos..]
    }
}

#[cfg(test)]
mod tests {
    use super::{parse_object, Command};

    #[test]
    fn parses_minimal_supported_stream() {
        let data: &[u8] = &[
            // d0 00 size=4 name=\"A\" even
            0xd0, 0x00, 0x00, 0x00, 0x00, 0x04, b'A', 0x00,
            // c0 01 size=2 name=\"text\" even
            0xc0, 0x01, 0x00, 0x00, 0x00, 0x02, b't', b'e', b'x', b't', 0x00, 0x00,
            // 10 01 data(2)
            0x10, 0x01, 0xaa, 0xbb,
            // 20 01 + reserved
            0x20, 0x01, 0x00, 0x00, 0x00, 0x00,
            // 30 00 size
            0x30, 0x00, 0x00, 0x00, 0x00, 0x10,
            // b2 ff label_no=1 name=\"_sym\" even
            0xb2, 0xff, 0x00, 0x00, 0x00, 0x01, b'_', b's', b'y', b'm', 0x00, 0x00,
            // e0 00 sect=1 address=0
            0xe0, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00,
            // e0 01 request \"libfoo\"
            0xe0, 0x01, b'l', b'i', b'b', b'f', b'o', b'o', 0x00, 0x00,
            // end
            0x00, 0x00,
        ];

        let object = parse_object(data).expect("parse should succeed");
        assert!(matches!(object.commands.last(), Some(Command::End)));
        assert_eq!(object.commands.len(), 9);
        assert!(object.scd_tail.is_empty());
    }

    #[test]
    fn parses_opaque_relocation_commands() {
        let data: &[u8] = &[
            // 42 ff label_no.w
            0x42, 0xff, 0x00, 0x10,
            // 53 01 adr.l + num.l
            0x53, 0x01, 0x00, 0x00, 0x00, 0x20, 0xff, 0xff, 0xff, 0xf0,
            // a0 10
            0xa0, 0x10,
            // b0 ff label
            0xb0, 0xff, 0x00, 0x00, 0x00, 0x01, b'l', b'b', b'l', 0x00,
            // end
            0x00, 0x00,
        ];

        let object = parse_object(data).expect("parse should succeed");
        assert!(matches!(object.commands[0], Command::Opaque { code: 0x42ff, .. }));
        assert!(matches!(object.commands[1], Command::Opaque { code: 0x5301, .. }));
        assert!(matches!(object.commands[2], Command::Opaque { code: 0xa010, .. }));
        assert!(matches!(object.commands[3], Command::DefineSymbol { section: 0xff, .. }));
    }

    #[test]
    fn parses_ctor_dtor_opaque_commands() {
        let data: &[u8] = &[
            // 4c01 adr.l
            0x4c, 0x01, 0x12, 0x34, 0x56, 0x78,
            // 4d01 adr.l
            0x4d, 0x01, 0x87, 0x65, 0x43, 0x21,
            // end
            0x00, 0x00,
        ];

        let object = parse_object(data).expect("parse should succeed");
        assert_eq!(object.commands.len(), 3);
        assert_eq!(
            object.commands[0],
            Command::Opaque {
                code: 0x4c01,
                payload: vec![0x12, 0x34, 0x56, 0x78]
            }
        );
        assert_eq!(
            object.commands[1],
            Command::Opaque {
                code: 0x4d01,
                payload: vec![0x87, 0x65, 0x43, 0x21]
            }
        );
        assert!(matches!(object.commands[2], Command::End));
    }
}
