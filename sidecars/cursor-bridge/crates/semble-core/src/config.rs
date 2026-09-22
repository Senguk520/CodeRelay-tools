//! User-independent configuration and fixed index format parameters.

use std::path::PathBuf;

/// Current persisted snapshot format. Incompatible changes must bump this value.
pub const INDEX_FORMAT_VERSION: u32 = 7;

/// Search and persistence settings shared by every indexed repository.
#[derive(Clone, Debug)]
pub struct SembleConfig {
    pub cache_dir: PathBuf,
    pub desired_chunk_bytes: usize,
    pub max_file_bytes: u64,
}

impl SembleConfig {
    pub fn new(cache_dir: impl Into<PathBuf>) -> Self {
        Self {
            cache_dir: cache_dir.into(),
            desired_chunk_bytes: 750,
            max_file_bytes: 1_000_000,
        }
    }
}

// There is deliberately no `Default` impl.
//
// There used to be one that fell back to `~/.coderelay-cursor-bridge/cache/semble`
// (overridable via `SEMBLE_CACHE_LOCATION`). That fallback contradicted the
// contract the host application relies on — "the bridge keeps its state under
// `CODERELAY_CURSOR_DATA_DIR`" — and it did so on a path that is reachable
// without any user action: a single tool call from a model would create a cache
// in the user's home directory, download tens of megabytes of model assets from
// huggingface.co, and `git clone` whatever URL the model supplied. The cache
// root is now a required argument so the caller has to say where it goes, and
// the host passes its managed data directory.
