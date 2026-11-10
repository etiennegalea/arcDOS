use std::{
    collections::{BTreeSet, VecDeque},
    io,
    path::{Component, Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, SyncSender},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use arcdos_filesystem::{Kind, Root, omitted_entry};
use notify::{Config, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use tokio::sync::Mutex;

use super::{Result, index::Document, store::Store};

const RECONCILE_INTERVAL: Duration = Duration::from_secs(120);
const IDLE_TICK: Duration = Duration::from_millis(250);
const PAGE: usize = 128;
const INDEX_BATCH: usize = 24;
const EVENT_BUFFER: usize = 128;

pub(super) struct Observer {
    stop: Arc<AtomicBool>,
    wake: SyncSender<Vec<PathBuf>>,
    thread: Option<JoinHandle<()>>,
}

impl Observer {
    pub fn start(path: PathBuf, store: Arc<Mutex<Store>>) -> io::Result<Self> {
        let (wake, events) = mpsc::sync_channel(EVENT_BUFFER);
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = stop.clone();
        let worker_wake = wake.clone();
        let thread = thread::Builder::new().name("arcdos-index".into()).spawn(move || {
            let overflow = Arc::new(AtomicBool::new(false));
            let _watcher = match watcher(&path, worker_wake, overflow.clone()) {
                Ok(watcher) => Some(watcher),
                Err(error) => {
                    tracing::warn!(%error, "filesystem notifications unavailable; using reconciliation");
                    None
                }
            };
            let root = store.blocking_lock().files_root();
            Worker { path, store, root, events, stop: worker_stop, overflow }.run();
        })?;
        Ok(Self {
            stop,
            wake,
            thread: Some(thread),
        })
    }
}

impl Drop for Observer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        let _ = self.wake.try_send(Vec::new());
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn watcher(
    path: &Path,
    events: SyncSender<Vec<PathBuf>>,
    overflow: Arc<AtomicBool>,
) -> notify::Result<RecommendedWatcher> {
    let mut watcher = RecommendedWatcher::new(
        move |event: notify::Result<notify::Event>| match event {
            Ok(event) if matches!(event.kind, EventKind::Access(_)) => {}
            Ok(event) if !event.need_rescan() && !event.paths.is_empty() => {
                if events.try_send(event.paths).is_err() {
                    overflow.store(true, Ordering::Release);
                }
            }
            _ => {
                overflow.store(true, Ordering::Release);
            }
        },
        Config::default().with_follow_symlinks(false),
    )?;
    watcher.watch(path, RecursiveMode::Recursive)?;
    Ok(watcher)
}

struct Worker {
    path: PathBuf,
    store: Arc<Mutex<Store>>,
    root: Arc<Root>,
    events: Receiver<Vec<PathBuf>>,
    stop: Arc<AtomicBool>,
    overflow: Arc<AtomicBool>,
}

impl Worker {
    fn run(&self) {
        let mut next_scan = Instant::now();
        let mut pending = Vec::new();
        while !self.stopping() {
            if self.overflow.swap(false, Ordering::AcqRel) || Instant::now() >= next_scan {
                let interval = match self.reconcile() {
                    Ok(()) => RECONCILE_INTERVAL,
                    Err(error) => {
                        tracing::warn!(%error, "search reconciliation will retry");
                        Duration::from_secs(5)
                    }
                };
                next_scan = Instant::now() + interval;
            }
            pending.extend(self.events.try_iter().take(EVENT_BUFFER).flatten());
            self.apply_events(pending.drain(..));
            let work = match self.index_batch() {
                Ok(work) => work,
                Err(error) => {
                    tracing::warn!(%error, "search indexing will retry");
                    false
                }
            };
            if !work && let Ok(paths) = self.events.recv_timeout(IDLE_TICK) {
                pending = paths;
            }
        }
    }

    fn stopping(&self) -> bool {
        self.stop.load(Ordering::Acquire)
    }

    fn with_store<T>(&self, operation: impl FnOnce(&Store) -> Result<T>) -> Result<T> {
        let store = self.store.blocking_lock();
        store.recover()?;
        operation(&store)
    }

    fn apply_events(&self, paths: impl Iterator<Item = PathBuf>) {
        let paths: BTreeSet<_> = paths
            .filter_map(|path| {
                let path = path.strip_prefix(&self.path).ok()?;
                if !path
                    .components()
                    .all(|part| matches!(part, Component::Normal(_)))
                {
                    return None;
                }
                path.to_str().map(str::to_owned)
            })
            .collect();
        for path in paths {
            if self.stopping() {
                break;
            }
            let observed = self.with_store(|store| store.observe_path(&path));
            let result = match observed {
                Ok(Some(Kind::Directory)) => self.scan_tree(path),
                Ok(_) => Ok(()),
                Err(error) => Err(error),
            };
            if let Err(error) = result {
                tracing::debug!(%error, "filesystem notification could not be reconciled");
            }
        }
    }

    fn reconcile(&self) -> Result<()> {
        self.scan_tree(String::new())?;
        let mut after = String::new();
        while !self.stopping() {
            let Some(next) = self.with_store(|store| store.prune_index(&after, PAGE))? else {
                break;
            };
            after = next;
        }
        Ok(())
    }

    fn scan_tree(&self, first: String) -> Result<()> {
        let mut directories = VecDeque::from([first]);
        let mut paths = Vec::with_capacity(PAGE);
        while let Some(directory) = directories.pop_front() {
            if self.stopping() {
                break;
            }
            let location = super::catalog::Location::new(super::catalog::Area::Files, &directory);
            let scan = self.root.scan(&directory, |child| {
                if self.stopping() {
                    return Ok(false);
                }
                paths.push(location.child(&child.name).path);
                if paths.len() == PAGE {
                    directories.extend(
                        self.with_store(|store| store.observe_batch(&paths))
                            .map_err(io::Error::other)?,
                    );
                    paths.clear();
                }
                Ok(true)
            });
            if let Err(error) = scan
                && !omitted_entry(&error)
            {
                return Err(error.into());
            }
            if !paths.is_empty() && !self.stopping() {
                directories.extend(self.with_store(|store| store.observe_batch(&paths))?);
                paths.clear();
            }
        }
        Ok(())
    }

    fn index_batch(&self) -> Result<bool> {
        let jobs = self.with_store(|store| store.index_jobs(INDEX_BATCH))?;
        let work = jobs.len() == INDEX_BATCH;
        for job in jobs {
            if self.stopping() {
                break;
            }
            match Document::read(&self.root, &job) {
                Ok(Some(document)) => self.with_store(|store| store.write_index(&job, document))?,
                Ok(None) | Err(_) => self.with_store(|store| store.retry_index(&job))?,
            }
        }
        Ok(work)
    }
}
