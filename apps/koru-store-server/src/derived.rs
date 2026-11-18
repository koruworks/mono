// SPDX-License-Identifier: MPL-2.0

//! Where the derived read model lives.
//!
//! The projection (`projection.db`), the search index (`search.db`) and the
//! readings mirror (`readings.db`) are caches rebuilt from the Store. A device keeps them under its Store root; an
//! in-memory composition ([`crate::memory`]) keeps the same two databases in
//! this process's memory, so every port that reads them — the outline, the
//! attribute and task queries, the indexer's tag names — opens them through
//! here and does not have to know which.
//!
//! **One name per database, held for the composition's lifetime.** Each port
//! opens its own connection, as it does on disk, and a database in memory
//! lives only while some connection holds it; the keepers here are that
//! connection, so a port dropping its reader can never take the read model
//! with it.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use koru_projection::Projections;
use koru_search::SearchIndex;
use koru_sqlite::MemoryName;

#[derive(Clone)]
pub enum Derived {
    /// `projection.db`, `search.db` and `readings.db` under this Store root.
    Root(PathBuf),
    /// All three in this process's memory.
    Memory(Arc<MemoryDerived>),
}

pub struct MemoryDerived {
    projection: MemoryName,
    search: MemoryName,
    readings: MemoryName,
    _keepers: Mutex<Vec<rusqlite::Connection>>,
}

impl MemoryDerived {
    pub fn open() -> rusqlite::Result<Self> {
        let projection = MemoryName::fresh("projection.db");
        let search = MemoryName::fresh("search.db");
        let readings = MemoryName::fresh("readings.db");
        let keepers = vec![
            koru_sqlite::open_memory(&projection)?,
            koru_sqlite::open_memory(&search)?,
            koru_sqlite::open_memory(&readings)?,
        ];
        Ok(Self {
            projection,
            search,
            readings,
            _keepers: Mutex::new(keepers),
        })
    }
}

impl Derived {
    pub fn root(root: &Path) -> Self {
        Self::Root(root.to_path_buf())
    }

    pub fn is_memory(&self) -> bool {
        matches!(self, Self::Memory(_))
    }

    /// One connection to the read model.
    pub fn projections(&self) -> Result<Projections, koru_projection::ProjectionError> {
        match self {
            Self::Root(root) => Projections::open(root),
            Self::Memory(memory) => Projections::in_named_memory(&memory.projection),
        }
    }

    /// One connection to the readings mirror (OCR TECHNICAL §7). Each of its
    /// users — the applier, the coordinator, the index's `Readings` source —
    /// opens its own, as each does of `projection.db`.
    pub fn mirror(&self) -> crate::readings::mirror::Result<crate::readings::mirror::Mirror> {
        match self {
            Self::Root(root) => crate::readings::mirror::Mirror::open(root),
            Self::Memory(memory) => {
                crate::readings::mirror::Mirror::in_named_memory(&memory.readings)
            }
        }
    }

    /// The search index's writer and a reader.
    pub fn search_pair(&self) -> koru_search::SearchResult<(SearchIndex, SearchIndex)> {
        match self {
            Self::Root(root) => koru_search::open(root),
            Self::Memory(memory) => SearchIndex::pair_in_memory(&memory.search),
        }
    }
}
