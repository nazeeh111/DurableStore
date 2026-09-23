use crate::{Error, Result};
use std::io::{Read, Seek, SeekFrom};

pub const FILE_HEADER: usize = 16;
pub const RECORD_HEADER: usize = 32;
pub const MAX_KEY: usize = 1024 * 1024;
pub const MAX_VALUE: usize = 16 * 1024 * 1024;

// IEEE CRC-32, reflected polynomial; bitwise implementation keeps dependencies at zero.
pub fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &byte in bytes {
        crc ^= byte as u32;
        for _ in 0..8 {
            crc = (crc >> 1) ^ (0xedb88320 & (0u32.wrapping_sub(crc & 1)));
        }
    }
    !crc
}

pub fn file_header() -> [u8; FILE_HEADER] {
    let mut h = [0; FILE_HEADER];
    h[..8].copy_from_slice(b"DSTOR001");
    h[8..12].copy_from_slice(&1u32.to_le_bytes());
    let crc = crc32(&h[..12]);
    h[12..].copy_from_slice(&crc.to_le_bytes());
    h
}

pub fn validate(key: &[u8], value: &[u8]) -> Result<()> {
    if key.len() > MAX_KEY || value.len() > MAX_VALUE {
        return Err(Error::InvalidInput(
            "key exceeds 1 MiB or value exceeds 16 MiB",
        ));
    }
    Ok(())
}

pub fn encode(seq: u64, key: &[u8], value: Option<&[u8]>) -> Result<Vec<u8>> {
    let payload = value.unwrap_or_default();
    validate(key, payload)?;
    let mut record = vec![0; RECORD_HEADER + key.len() + payload.len()];
    record[..4].copy_from_slice(b"DSR1");
    record[4] = u8::from(value.is_none());
    record[8..16].copy_from_slice(&seq.to_le_bytes());
    record[16..20].copy_from_slice(&(key.len() as u32).to_le_bytes());
    record[20..24].copy_from_slice(&(payload.len() as u32).to_le_bytes());
    record[32..32 + key.len()].copy_from_slice(key);
    record[32 + key.len()..].copy_from_slice(payload);
    let crc = crc32(&record[32..]);
    record[24..28].copy_from_slice(&crc.to_le_bytes());
    let crc = crc32(&record[..28]);
    record[28..32].copy_from_slice(&crc.to_le_bytes());
    Ok(record)
}

pub struct Scan {
    pub entries: std::collections::BTreeMap<Vec<u8>, Vec<u8>>,
    pub records: u64,
    pub valid_bytes: u64,
    pub tail_bytes: u64,
}

pub fn scan<R: Read + Seek>(reader: &mut R) -> Result<Scan> {
    let len = reader.seek(SeekFrom::End(0))?;
    reader.rewind()?;
    if len < FILE_HEADER as u64 {
        return Err(Error::Corruption {
            offset: 0,
            reason: "incomplete file header",
        });
    }
    let mut header = [0; FILE_HEADER];
    reader.read_exact(&mut header)?;
    if header != file_header() {
        return Err(Error::Corruption {
            offset: 0,
            reason: "bad file header or unsupported version",
        });
    }
    let mut result = Scan {
        entries: Default::default(),
        records: 0,
        valid_bytes: FILE_HEADER as u64,
        tail_bytes: 0,
    };
    while result.valid_bytes < len {
        let at = result.valid_bytes;
        if len - at < RECORD_HEADER as u64 {
            break;
        }
        let mut h = [0; RECORD_HEADER];
        reader.read_exact(&mut h)?;
        let corrupt = |reason| Error::Corruption { offset: at, reason };
        if &h[..4] != b"DSR1" || h[5..8] != [0, 0, 0] || h[4] > 1 {
            return Err(corrupt("invalid record header"));
        }
        let u32_at = |i| u32::from_le_bytes(h[i..i + 4].try_into().unwrap());
        if crc32(&h[..28]) != u32_at(28) {
            return Err(corrupt("record header checksum mismatch"));
        }
        let seq = u64::from_le_bytes(h[8..16].try_into().unwrap());
        if result.records.checked_add(1) != Some(seq) {
            return Err(corrupt("record sequence mismatch"));
        }
        let key_len = u32_at(16) as usize;
        let val_len = u32_at(20) as usize;
        if key_len > MAX_KEY || val_len > MAX_VALUE || (h[4] == 1 && val_len != 0) {
            return Err(corrupt("invalid record lengths"));
        }
        let payload_len = key_len + val_len;
        if len - at - (RECORD_HEADER as u64) < payload_len as u64 {
            break;
        }
        let mut payload = vec![0; payload_len];
        reader.read_exact(&mut payload)?;
        if crc32(&payload) != u32_at(24) {
            return Err(corrupt("payload checksum mismatch"));
        }
        if h[4] == 1 {
            result.entries.remove(&payload[..key_len]);
        } else {
            let value = payload.split_off(key_len);
            result.entries.insert(payload, value);
        }
        result.records = seq;
        result.valid_bytes += RECORD_HEADER as u64 + payload_len as u64;
    }
    result.tail_bytes = len - result.valid_bytes;
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn crc_known_vectors() {
        assert_eq!(crc32(b"123456789"), 0xcbf43926);
        assert_eq!(crc32(b""), 0);
    }
    #[test]
    fn rejects_checked_oversized_lengths() {
        let mut bytes = file_header().to_vec();
        let mut rec = encode(1, b"a", Some(b"b")).unwrap();
        rec[16..20].copy_from_slice(&u32::MAX.to_le_bytes());
        let crc = crc32(&rec[..28]);
        rec[28..32].copy_from_slice(&crc.to_le_bytes());
        bytes.extend(rec);
        assert!(matches!(
            scan(&mut std::io::Cursor::new(bytes)),
            Err(Error::Corruption {
                reason: "invalid record lengths",
                ..
            })
        ));
    }
}
