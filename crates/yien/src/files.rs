//! The file tree for the window (RF-CTX-01, RF-CTX-03): listings and
//! searches on the platform layer's threads, files opened with the same
//! opener as the settings file.

use std::path::PathBuf;

use yien_platform::config_file;
use yien_platform::file_tree::FileTreeService;
use yien_proto::files::{SearchEnd, SearchResult};
use yien_ui::{Files, FilesHost, ListDone, OpenDone, SearchDone};

/// Test-only: folders to expand in each tab's tree at start, comma
/// separated, so a window check can capture an expanded tree without
/// sending keys.
const SEED_VAR: &str = "YIEN_TEST_FILE_TREE";

struct Host {
    service: Option<FileTreeService>,
}

impl FilesHost for Host {
    fn list(&self, root: PathBuf, dir: String, done: ListDone) {
        match &self.service {
            Some(service) => service.list(
                root,
                dir,
                Box::new(move |listing| done(listing.map_err(|e| e.to_string()))),
            ),
            None => done(Err("the file tree is off".into())),
        }
    }

    fn search(&self, root: PathBuf, query: String, done: SearchDone) {
        match &self.service {
            Some(service) => service.search(root, query, done),
            None => done(SearchResult {
                hits: Vec::new(),
                end: SearchEnd::Unavailable,
            }),
        }
    }

    fn cancel_search(&self) {
        if let Some(service) = &self.service {
            service.cancel_search();
        }
    }

    fn open(&self, file: PathBuf, editor: Option<String>, done: OpenDone) {
        config_file::open(file, editor, move |result| {
            if let Err(error) = &result {
                tracing::warn!("a file was not opened: {error}");
            }
            done(result.err().map(|e| e.to_string()));
        });
    }
}

/// The host, with the test seed when its variable is set.
pub(crate) fn start() -> Files {
    let service = FileTreeService::start()
        .inspect_err(|e| tracing::warn!("the file tree is off: {e}"))
        .ok();
    let seed = std::env::var(SEED_VAR)
        .map(|v| {
            v.split(',')
                .map(|p| p.trim().trim_matches('/').to_owned())
                .filter(|p| !p.is_empty())
                .collect()
        })
        .unwrap_or_default();
    Files {
        host: Box::new(Host { service }),
        seed,
    }
}
