const ELF_HEADER_SIZE: usize = 64;
const PROGRAM_HEADER_SIZE: usize = 56;
const PT_LOAD: u32 = 1;
const ET_EXEC: u16 = 2;
const EM_X86_64: u16 = 62;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    Truncated,
    InvalidMagic,
    UnsupportedFormat,
    InvalidProgramHeaders,
    InvalidSegment,
}

#[derive(Clone, Copy)]
pub struct Segment {
    pub virtual_address: u64,
    pub memory_size: u64,
    pub file_offset: usize,
    pub file_size: usize,
    pub flags: u32,
}

pub struct Elf64<'a> {
    bytes: &'a [u8],
    pub entry: u64,
    program_headers: usize,
    program_header_count: usize,
    program_header_size: usize,
}

impl<'a> Elf64<'a> {
    pub fn parse(bytes: &'a [u8]) -> Result<Self, Error> {
        if bytes.len() < ELF_HEADER_SIZE {
            return Err(Error::Truncated);
        }
        if &bytes[..4] != b"\x7fELF" {
            return Err(Error::InvalidMagic);
        }
        if bytes[4] != 2
            || bytes[5] != 1
            || read_u16(bytes, 16)? != ET_EXEC
            || read_u16(bytes, 18)? != EM_X86_64
            || read_u32(bytes, 20)? != 1
            || read_u16(bytes, 52)? as usize != ELF_HEADER_SIZE
        {
            return Err(Error::UnsupportedFormat);
        }
        let entry = read_u64(bytes, 24)?;
        let program_headers =
            usize::try_from(read_u64(bytes, 32)?).map_err(|_| Error::InvalidProgramHeaders)?;
        let program_header_size = read_u16(bytes, 54)? as usize;
        let program_header_count = read_u16(bytes, 56)? as usize;
        if program_header_size < PROGRAM_HEADER_SIZE
            || program_header_count == 0
            || program_header_count > 128
        {
            return Err(Error::InvalidProgramHeaders);
        }
        let table_size = program_header_size
            .checked_mul(program_header_count)
            .ok_or(Error::InvalidProgramHeaders)?;
        let table_end = program_headers
            .checked_add(table_size)
            .ok_or(Error::InvalidProgramHeaders)?;
        if table_end > bytes.len() {
            return Err(Error::InvalidProgramHeaders);
        }
        let elf = Self {
            bytes,
            entry,
            program_headers,
            program_header_count,
            program_header_size,
        };
        let mut loadable = false;
        let mut executable_entry = false;
        for index in 0..program_header_count {
            if let Some(segment) = elf.segment(index)? {
                loadable = true;
                let end = segment
                    .virtual_address
                    .checked_add(segment.memory_size)
                    .ok_or(Error::InvalidSegment)?;
                if segment.flags & 1 != 0 && entry >= segment.virtual_address && entry < end {
                    executable_entry = true;
                }
            }
        }
        if !loadable || !executable_entry {
            return Err(Error::InvalidSegment);
        }
        Ok(elf)
    }

    pub fn segment(&self, index: usize) -> Result<Option<Segment>, Error> {
        if index >= self.program_header_count {
            return Ok(None);
        }
        // Checked arithmetic: in release (overflow checks off) the naive
        // `+`/`*` could wrap and make `read_u32` inspect the wrong header.
        let stride = index
            .checked_mul(self.program_header_size)
            .ok_or(Error::InvalidProgramHeaders)?;
        let header = self
            .program_headers
            .checked_add(stride)
            .ok_or(Error::InvalidProgramHeaders)?;
        if read_u32(self.bytes, header)? != PT_LOAD {
            return Ok(None);
        }
        let file_offset = usize::try_from(read_u64(self.bytes, header + 8)?)
            .map_err(|_| Error::InvalidSegment)?;
        let virtual_address = read_u64(self.bytes, header + 16)?;
        let file_size = usize::try_from(read_u64(self.bytes, header + 32)?)
            .map_err(|_| Error::InvalidSegment)?;
        let memory_size = read_u64(self.bytes, header + 40)?;
        let file_end = file_offset
            .checked_add(file_size)
            .ok_or(Error::InvalidSegment)?;
        let memory_end = virtual_address
            .checked_add(memory_size)
            .ok_or(Error::InvalidSegment)?;
        if file_end > self.bytes.len()
            || file_size as u64 > memory_size
            || memory_size == 0
            || virtual_address < 0x400000
            || memory_end > 0x0000_8000_0000_0000
        {
            return Err(Error::InvalidSegment);
        }
        Ok(Some(Segment {
            virtual_address,
            memory_size,
            file_offset,
            file_size,
            flags: read_u32(self.bytes, header + 4)?,
        }))
    }

    /// Returns `None` instead of panicking when the segment's file range
    /// is out of bounds or wraps around `usize::MAX`.
    pub fn file_bytes(&self, segment: Segment) -> Option<&'a [u8]> {
        let end = segment.file_offset.checked_add(segment.file_size)?;
        self.bytes.get(segment.file_offset..end)
    }

    pub fn program_header_count(&self) -> usize {
        self.program_header_count
    }
}

fn read_u16(bytes: &[u8], offset: usize) -> Result<u16, Error> {
    Ok(u16::from_le_bytes(
        bytes
            .get(offset..offset + 2)
            .ok_or(Error::Truncated)?
            .try_into()
            .map_err(|_| Error::Truncated)?,
    ))
}

fn read_u32(bytes: &[u8], offset: usize) -> Result<u32, Error> {
    Ok(u32::from_le_bytes(
        bytes
            .get(offset..offset + 4)
            .ok_or(Error::Truncated)?
            .try_into()
            .map_err(|_| Error::Truncated)?,
    ))
}

fn read_u64(bytes: &[u8], offset: usize) -> Result<u64, Error> {
    Ok(u64::from_le_bytes(
        bytes
            .get(offset..offset + 8)
            .ok_or(Error::Truncated)?
            .try_into()
            .map_err(|_| Error::Truncated)?,
    ))
}

#[cfg(test)]
mod tests {
    use super::{Elf64, Error};

    fn valid_elf() -> [u8; 128] {
        let mut bytes = [0u8; 128];
        bytes[..4].copy_from_slice(b"\x7fELF");
        bytes[4] = 2;
        bytes[5] = 1;
        bytes[6] = 1;
        bytes[16..18].copy_from_slice(&2u16.to_le_bytes());
        bytes[18..20].copy_from_slice(&62u16.to_le_bytes());
        bytes[20..24].copy_from_slice(&1u32.to_le_bytes());
        bytes[24..32].copy_from_slice(&0x400000u64.to_le_bytes());
        bytes[32..40].copy_from_slice(&64u64.to_le_bytes());
        bytes[52..54].copy_from_slice(&64u16.to_le_bytes());
        bytes[54..56].copy_from_slice(&56u16.to_le_bytes());
        bytes[56..58].copy_from_slice(&1u16.to_le_bytes());
        bytes[64..68].copy_from_slice(&1u32.to_le_bytes());
        bytes[68..72].copy_from_slice(&1u32.to_le_bytes());
        bytes[72..80].copy_from_slice(&120u64.to_le_bytes());
        bytes[80..88].copy_from_slice(&0x400000u64.to_le_bytes());
        bytes[96..104].copy_from_slice(&8u64.to_le_bytes());
        bytes[104..112].copy_from_slice(&8u64.to_le_bytes());
        bytes[112..120].copy_from_slice(&4096u64.to_le_bytes());
        bytes[120..128].copy_from_slice(b"testcode");
        bytes
    }

    #[test]
    fn parses_executable_load_segment() {
        let bytes = valid_elf();
        let elf = Elf64::parse(&bytes).unwrap();
        assert_eq!(elf.entry, 0x400000);
        let segment = elf.segment(0).unwrap().unwrap();
        assert_eq!(segment.virtual_address, 0x400000);
        assert_eq!(elf.file_bytes(segment).unwrap(), b"testcode");
    }

    #[test]
    fn rejects_truncated_or_out_of_range_images() {
        assert!(matches!(Elf64::parse(&[0x7f, b'E']), Err(Error::Truncated)));
        let mut bytes = valid_elf();
        bytes[80..88].copy_from_slice(&0xffff_ffff_ffff_f000u64.to_le_bytes());
        assert!(matches!(Elf64::parse(&bytes), Err(Error::InvalidSegment)));
    }
}
