use crate::storage::Storage;
use std::io;

pub(crate) const MAX_TABLES: usize = 4096;
const MAGIC: &[u8; 8] = b"STOWMAN\0";
const HEADER: usize = 24;
const MAX_SIZE: usize = HEADER + MAX_TABLES * 8 + 4;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Manifest {
    pub(crate) wal: u64,
    pub(crate) tables: Vec<u64>,
}
impl Manifest {
    fn validate(&self) -> io::Result<()> {
        if self.wal == 0 || self.tables.len() > MAX_TABLES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid manifest generation/count",
            ));
        }
        let mut previous = 0;
        for &generation in &self.tables {
            if generation <= previous || generation > self.wal {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "manifest generations must increase and not exceed WAL generation",
                ));
            }
            previous = generation;
        }
        Ok(())
    }
    pub(crate) fn encode(&self) -> io::Result<Vec<u8>> {
        self.validate()?;
        let mut bytes = Vec::with_capacity(HEADER + self.tables.len() * 8 + 4);
        bytes.extend_from_slice(MAGIC);
        bytes.extend_from_slice(&1u32.to_le_bytes());
        bytes.extend_from_slice(&self.wal.to_le_bytes());
        bytes.extend_from_slice(&(self.tables.len() as u32).to_le_bytes());
        for id in &self.tables {
            bytes.extend_from_slice(&id.to_le_bytes());
        }
        bytes.extend_from_slice(&crc32fast::hash(&bytes).to_le_bytes());
        Ok(bytes)
    }
    pub(crate) fn decode(bytes: &[u8]) -> io::Result<Self> {
        let invalid = || {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "corrupt or unsupported manifest",
            )
        };
        if bytes.len() < HEADER + 4 || bytes.len() > MAX_SIZE || &bytes[..8] != MAGIC {
            return Err(invalid());
        }
        let checksum =
            u32::from_le_bytes(bytes[bytes.len() - 4..].try_into().map_err(|_| invalid())?);
        if crc32fast::hash(&bytes[..bytes.len() - 4]) != checksum {
            return Err(invalid());
        }
        let version = u32::from_le_bytes(bytes[8..12].try_into().map_err(|_| invalid())?);
        if version != 1 {
            return Err(invalid());
        }
        let wal = u64::from_le_bytes(bytes[12..20].try_into().map_err(|_| invalid())?);
        let count = u32::from_le_bytes(bytes[20..24].try_into().map_err(|_| invalid())?) as usize;
        if count > MAX_TABLES || bytes.len() != HEADER + count * 8 + 4 {
            return Err(invalid());
        }
        let mut tables = Vec::with_capacity(count);
        for chunk in bytes[HEADER..bytes.len() - 4].chunks_exact(8) {
            tables.push(u64::from_le_bytes(chunk.try_into().map_err(|_| invalid())?));
        }
        let manifest = Self { wal, tables };
        manifest.validate()?;
        Ok(manifest)
    }
    pub(crate) fn read<S: Storage>(storage: &mut S) -> io::Result<Self> {
        let len = storage.len()?;
        if len < (HEADER + 4) as u64 || len > MAX_SIZE as u64 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "manifest length outside limit",
            ));
        }
        let mut bytes = vec![0; len as usize];
        let mut offset = 0;
        while offset < bytes.len() {
            let n = storage.read_at(offset as u64, &mut bytes[offset..])?;
            if n == 0 || n > bytes.len() - offset {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "short manifest read",
                ));
            }
            offset += n;
        }
        Self::decode(&bytes)
    }
}
pub(crate) fn wal_name(g: u64) -> String {
    format!("g{g:020}.wal")
}
pub(crate) fn table_name(g: u64) -> String {
    format!("g{g:020}.sst")
}
pub(crate) fn temp_name(g: u64) -> String {
    format!("g{g:020}.manifest.tmp")
}
pub(crate) fn generation(name: &str) -> Option<u64> {
    let rest = name.strip_prefix('g')?;
    let (id, suffix) = rest.split_at_checked(20)?;
    if !id.bytes().all(|b| b.is_ascii_digit())
        || !matches!(suffix, ".wal" | ".sst" | ".manifest.tmp")
    {
        return None;
    }
    id.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn roundtrip_bounds_and_all_corrupted_bytes() {
        let m = Manifest {
            wal: 5,
            tables: vec![2, 4, 5],
        };
        let bytes = m.encode().unwrap();
        assert_eq!(Manifest::decode(&bytes).unwrap(), m);
        for i in 0..bytes.len() {
            let mut bad = bytes.clone();
            bad[i] ^= 1;
            assert!(Manifest::decode(&bad).is_err());
        }
        for len in 0..bytes.len() {
            assert!(Manifest::decode(&bytes[..len]).is_err());
        }
        for tables in [vec![0], vec![3, 2], vec![2, 2], vec![6]] {
            assert!(Manifest { wal: 5, tables }.encode().is_err());
        }
        assert!(
            Manifest {
                wal: 0,
                tables: vec![]
            }
            .encode()
            .is_err()
        );
        assert!(
            Manifest {
                wal: 1,
                tables: vec![1; MAX_TABLES + 1]
            }
            .encode()
            .is_err()
        );
    }
    #[test]
    fn future_version_and_bad_counts_with_repaired_crc_fail() {
        let valid = Manifest {
            wal: 1,
            tables: vec![],
        }
        .encode()
        .unwrap();
        for (offset, value) in [(8, 2u32), (20, u32::MAX)] {
            let mut bad = valid.clone();
            bad[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
            let n = bad.len();
            let crc = crc32fast::hash(&bad[..n - 4]);
            bad[n - 4..].copy_from_slice(&crc.to_le_bytes());
            assert!(Manifest::decode(&bad).is_err());
        }
    }
    #[test]
    fn generation_names_are_strict() {
        assert_eq!(generation(&wal_name(1)), Some(1));
        assert_eq!(generation(&table_name(u64::MAX)), Some(u64::MAX));
        assert_eq!(generation(&temp_name(2)), Some(2));
        for name in [
            "g1.wal",
            "g00000000000000000001.foo",
            "MANIFEST",
            "../g00000000000000000001.wal",
        ] {
            assert_eq!(generation(name), None);
        }
    }
}
