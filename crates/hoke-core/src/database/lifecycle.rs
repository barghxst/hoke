use crate::{
    Engine,
    directory::Directory,
    manifest::{self, Manifest},
    sstable::SSTable,
    storage::Storage,
};
use std::io;

use super::{Database, DbError, error::io_error, publication::publish};

impl<D: Directory> Database<D> {
    pub fn open(directory: D) -> Result<Self, DbError> {
        Self::open_with_options(directory, crate::DatabaseOptions::default())
    }

    pub fn open_with_options(
        directory: D,
        options: crate::DatabaseOptions,
    ) -> Result<Self, DbError> {
        options.validate()?;
        let manifest = match directory.open_existing("MANIFEST") {
            Ok(mut file) => Manifest::read(&mut file).map_err(io_error("read manifest"))?,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                if !directory
                    .list()
                    .map_err(io_error("list new database"))?
                    .is_empty()
                {
                    return Err(DbError::Corruption(
                        "manifest missing in nonempty directory".into(),
                    ));
                }
                let manifest = Manifest {
                    wal: 1,
                    tables: vec![],
                };
                let file = directory
                    .create_new(&manifest::wal_name(1))
                    .map_err(io_error("create initial WAL"))?;
                let wal = Engine::open(file).map_err(DbError::Wal)?;
                directory
                    .sync_directory()
                    .map_err(io_error("sync initial WAL name"))?;
                publish(&directory, &manifest)?;
                return Ok(Self {
                    wal,
                    tables: vec![],
                    manifest,
                    poisoned: false,
                    options,
                    directory,
                });
            }
            Err(error) => return Err(io_error("open manifest")(error)),
        };
        let mut tables = Vec::with_capacity(manifest.tables.len());
        for generation in &manifest.tables {
            let file = directory
                .open_existing(&manifest::table_name(*generation))
                .map_err(io_error("open referenced SST"))?;
            tables.push(SSTable::open(file).map_err(DbError::Sst)?);
        }
        let file = directory
            .open_existing(&manifest::wal_name(manifest.wal))
            .map_err(io_error("open referenced WAL"))?;
        if file.len().map_err(io_error("read referenced WAL length"))?
            < crate::wal::FILE_HEADER_LEN as u64
        {
            return Err(DbError::Corruption(
                "referenced WAL has no complete header".into(),
            ));
        }
        let wal = Engine::open(file).map_err(DbError::Wal)?;
        Ok(Self {
            wal,
            tables,
            manifest,
            poisoned: false,
            options,
            directory,
        })
    }

    pub fn close(self) -> Result<(), DbError> {
        self.writable()?;
        self.wal.close().map_err(DbError::Wal)
    }
}
