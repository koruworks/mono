// SPDX-License-Identifier: MPL-2.0

//! Where the derived read model lives.
//!
//! The projection (`projection.db`) and the search index (`search.db`) are
//! caches rebuilt from the Store. A device keeps them under its Store root; an
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
    /// `projection.db` and `search.db` under this Store root.
    Root(PathBuf),
    /// Both in this process's memory.
    Memory(Arc<MemoryDerived>),
}

pub struct MemoryDerived {
    projection: MemoryName,
    search: MemoryName,
    _keepers: Mutex<Vec<rusqlite::Connection>>,
}

impl MemoryDerived {
    pub fn open() -> rusqlite::Result<Self> {
        let projection = MemoryName::fresh("projection.db");
        let search = MemoryName::fresh("search.db");
        let keepers = vec![
            koru_sqlite::open_memory(&projection)?,
            koru_sqlite::open_memory(&search)?,
        ];
        Ok(Self {
            projection,
            search,
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

    /// The search index's writer and a reader.
    pub fn search_pair(&self) -> koru_search::SearchResult<(SearchIndex, SearchIndex)> {
        match self {
            Self::Root(root) => koru_search::open(root),
            Self::Memory(memory) => SearchIndex::pair_in_memory(&memory.search),
        }
    }
}
