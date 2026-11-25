//! Background worker pool, priority decode queue, and byte-budgeted LRU cache.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender, unbounded};
use kalux_core::{ByteLru, DecodedImage};

use crate::DecodeError;
use crate::scale::fit_dimensions;

type DecodeFn =
    Arc<dyn Fn(&Path, Option<(u32, u32)>) -> Result<DecodedImage, DecodeError> + Send + Sync>;

#[derive(Clone, Debug)]
pub struct StoreConfig {
    /// Worker threads. Default: min(available_parallelism - 1, 4), at least 1.
    pub threads: usize,
    /// Cache budget in bytes. Default 128 MiB.
    pub cache_budget: usize,
    /// How many neighbors on each side to preload. Default 2.
    pub preload_radius: usize,
}

impl Default for StoreConfig {
    fn default() -> Self {
        let cpus = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(2);
        Self {
            threads: cpus.saturating_sub(1).clamp(1, 4),
            cache_budget: 128 * 1024 * 1024,
            preload_radius: 2,
        }
    }
}

#[derive(Debug)]
pub enum StoreEvent {
    /// A decode finished for `path`. `full_resolution` is true when it was decoded without `max_size`.
    Ready {
        path: PathBuf,
        image: Arc<DecodedImage>,
        full_resolution: bool,
        elapsed: Duration,
    },
    Failed {
        path: PathBuf,
        error: DecodeError,
    },
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum JobKind {
    Fit(u32, u32),
    Full,
}

impl JobKind {
    fn max_size(self) -> Option<(u32, u32)> {
        match self {
            Self::Fit(w, h) => Some((w, h)),
            Self::Full => None,
        }
    }

    fn is_full(self) -> bool {
        matches!(self, Self::Full)
    }

    /// True if `self` provides at least as much detail as `wanted`.
    fn satisfies(self, wanted: Self) -> bool {
        match (self, wanted) {
            (Self::Full, _) => true,
            (Self::Fit(_, _), Self::Full) => false,
            (Self::Fit(aw, ah), Self::Fit(bw, bh)) => aw >= bw && ah >= bh,
        }
    }
}

#[derive(Clone, Debug)]
struct Job {
    path: PathBuf,
    kind: JobKind,
    /// Lower number = higher priority (0 = current, 1 = full-res request, 2+ = neighbors).
    priority: usize,
    /// Tie-breaker so equal priorities preserve submission order.
    seq: u64,
    /// Per-path invalidation generation when queued.
    generation: u64,
}

#[derive(Clone, Debug)]
struct InFlight {
    path: PathBuf,
    kind: JobKind,
    generation: u64,
}

struct QueueState {
    pending: Vec<Job>,
    in_flight: Vec<InFlight>,
    shutdown: bool,
}

struct JobResult {
    path: PathBuf,
    full_resolution: bool,
    generation: u64,
    elapsed: Duration,
    outcome: Result<Arc<DecodedImage>, DecodeError>,
}

/// True if `img` already has enough resolution for a `target` client size.
fn is_adequate_for(img: &DecodedImage, target: Option<(u32, u32)>) -> bool {
    if !img.is_downscaled() {
        return true;
    }
    let Some((tw, th)) = target else {
        return false;
    };
    if tw == 0 || th == 0 {
        return true;
    }
    let (needed_w, needed_h) = fit_dimensions(img.source_width, img.source_height, tw, th);
    // Allow 1 px tolerance for integer rounding differences between scalers.
    img.width.saturating_add(1) >= needed_w && img.height.saturating_add(1) >= needed_h
}

/// True if `incoming` has strictly lower resolution than `existing` (prevents a slow fit decode
/// from clobbering a full-resolution or larger-target cached entry).
fn is_downgrade(existing: &DecodedImage, incoming: &DecodedImage) -> bool {
    let existing_pixels = u64::from(existing.width) * u64::from(existing.height);
    let incoming_pixels = u64::from(incoming.width) * u64::from(incoming.height);
    incoming_pixels < existing_pixels
}

/// Background decoder + cache. Owned by the UI thread; not `Sync`.
pub struct ImageStore {
    shared: Arc<(Mutex<QueueState>, Condvar)>,
    result_rx: Receiver<JobResult>,
    cache: ByteLru<PathBuf, Arc<DecodedImage>>,
    path_gen: HashMap<PathBuf, u64>,
    target: Option<(u32, u32)>,
    next_seq: u64,
}

impl ImageStore {
    /// Start workers. `notify` is called from a worker thread whenever a result is queued;
    /// the app uses it to `PostMessageW` its window so the UI thread calls [`poll`](Self::poll).
    pub fn new(config: StoreConfig, notify: Arc<dyn Fn() + Send + Sync>) -> Self {
        Self::with_decoder(config, notify, Arc::new(crate::decode_file))
    }

    pub(crate) fn with_decoder(
        config: StoreConfig,
        notify: Arc<dyn Fn() + Send + Sync>,
        decode_fn: DecodeFn,
    ) -> Self {
        let threads = config.threads.max(1);
        let shared = Arc::new((
            Mutex::new(QueueState {
                pending: Vec::new(),
                in_flight: Vec::new(),
                shutdown: false,
            }),
            Condvar::new(),
        ));
        let (result_tx, result_rx) = unbounded::<JobResult>();

        for idx in 0..threads {
            let shared_w = Arc::clone(&shared);
            let tx_w = result_tx.clone();
            let notify_w = Arc::clone(&notify);
            let decode_w = Arc::clone(&decode_fn);

            let _ = std::thread::Builder::new()
                .name(format!("kalux-decode-{idx}"))
                .spawn(move || worker_loop(shared_w, tx_w, notify_w, decode_w));
        }

        Self {
            shared,
            result_rx,
            cache: ByteLru::new(config.cache_budget),
            path_gen: HashMap::new(),
            target: None,
            next_seq: 0,
        }
    }

    fn current_gen(&self, path: &Path) -> u64 {
        self.path_gen.get(path).copied().unwrap_or(0)
    }

    fn effective_target(&mut self, target: (u32, u32)) -> Option<(u32, u32)> {
        if target.0 > 0 && target.1 > 0 {
            self.target = Some(target);
            Some(target)
        } else {
            self.target
        }
    }

    fn enqueue_locked(
        state: &mut QueueState,
        next_seq: &mut u64,
        path: PathBuf,
        kind: JobKind,
        priority: usize,
        generation: u64,
    ) -> bool {
        // Deduplicate against in-flight jobs of the same generation that already satisfy `kind`.
        if state
            .in_flight
            .iter()
            .any(|f| f.path == path && f.generation == generation && f.kind.satisfies(kind))
        {
            return false;
        }

        // Deduplicate / reprioritize against queued jobs of the same generation.
        if let Some(existing) = state
            .pending
            .iter_mut()
            .find(|j| j.path == path && j.generation == generation)
        {
            if existing.kind.satisfies(kind) {
                existing.priority = existing.priority.min(priority);
                return false;
            }
            // Existing queued job has smaller target than what we want now: upgrade it in place.
            existing.kind = kind;
            existing.priority = existing.priority.min(priority);
            return false;
        }

        let seq = *next_seq;
        *next_seq = next_seq.wrapping_add(1);
        state.pending.push(Job {
            path,
            kind,
            priority,
            seq,
            generation,
        });
        true
    }

    /// Make `current` the focused image and preload `neighbors` (nearest first).
    /// * Returns the cached image immediately if present (no event will be sent for it).
    /// * Otherwise queues `current` at top priority; neighbors after it.
    /// * Queued-but-not-started jobs for paths no longer in {current} ∪ neighbors are dropped.
    /// * Pins current + neighbors in the cache.
    ///
    /// `target` is the client size in physical px; images are decoded to fit it.
    pub fn focus(
        &mut self,
        current: &Path,
        neighbors: &[PathBuf],
        target: (u32, u32),
    ) -> Option<Arc<DecodedImage>> {
        let eff_target = self.effective_target(target);
        let job_kind = match eff_target {
            Some((w, h)) => JobKind::Fit(w, h),
            None => JobKind::Full,
        };

        let current_buf = current.to_path_buf();

        // Build deduplicated pinned & wanted set: current first, then neighbors in order.
        let mut pinned = Vec::with_capacity(1 + neighbors.len());
        let mut wanted = HashSet::with_capacity(1 + neighbors.len());
        pinned.push(current_buf.clone());
        wanted.insert(current_buf.clone());
        for n in neighbors {
            if wanted.insert(n.clone()) {
                pinned.push(n.clone());
            }
        }
        self.cache.set_pinned(&pinned);

        // Check current in cache (marks MRU on hit).
        let current_hit = self
            .cache
            .get(&current_buf)
            .filter(|img| is_adequate_for(img, eff_target))
            .map(Arc::clone);

        // Determine which paths need decoding.
        let mut to_queue: Vec<(PathBuf, usize, u64)> = Vec::new();
        if current_hit.is_none() {
            to_queue.push((current_buf.clone(), 0, self.current_gen(&current_buf)));
        }
        for (idx, n) in pinned.iter().skip(1).enumerate() {
            let cached_ok = self
                .cache
                .peek(n)
                .is_some_and(|img| is_adequate_for(img, eff_target));
            if !cached_ok {
                to_queue.push((n.clone(), 2 + idx, self.current_gen(n)));
            }
        }

        // Update shared priority queue: prune unwanted queued jobs, then enqueue needed ones.
        let (lock, cvar) = &*self.shared;
        let mut added = 0usize;
        {
            let mut state = lock.lock().unwrap_or_else(|e| e.into_inner());
            state.pending.retain(|j| wanted.contains(&j.path));

            // Update priorities of any remaining queued jobs (e.g. a neighbor promoted to current).
            for job in &mut state.pending {
                if job.path == current_buf && !job.kind.is_full() {
                    job.priority = 0;
                }
            }

            for (path, prio, gen_id) in to_queue {
                if Self::enqueue_locked(
                    &mut state,
                    &mut self.next_seq,
                    path,
                    job_kind,
                    prio,
                    gen_id,
                ) {
                    added += 1;
                }
            }
        }

        if added > 0 {
            if added == 1 {
                cvar.notify_one();
            } else {
                cvar.notify_all();
            }
        }

        current_hit
    }

    /// Queue a full-resolution decode of `path` (when the user zooms past the downscaled detail).
    /// Result arrives as `Ready { full_resolution: true }` and replaces the cached entry.
    pub fn request_full_resolution(&mut self, path: &Path) {
        let path_buf = path.to_path_buf();
        if self
            .cache
            .peek(&path_buf)
            .is_some_and(|img| !img.is_downscaled())
        {
            return;
        }
        let gen_id = self.current_gen(&path_buf);
        let (lock, cvar) = &*self.shared;
        let added = {
            let mut state = lock.lock().unwrap_or_else(|e| e.into_inner());
            Self::enqueue_locked(
                &mut state,
                &mut self.next_seq,
                path_buf,
                JobKind::Full,
                1,
                gen_id,
            )
        };
        if added {
            cvar.notify_one();
        }
    }

    /// Drain finished jobs (non-blocking). Results are inserted into the cache before being returned.
    pub fn poll(&mut self) -> Vec<StoreEvent> {
        let mut events = Vec::new();
        for res in self.result_rx.try_iter() {
            // Drop stale results from before `invalidate(path)` was called.
            if res.generation < self.current_gen(&res.path) {
                continue;
            }
            match res.outcome {
                Ok(image) => {
                    let should_insert = self
                        .cache
                        .peek(&res.path)
                        .is_none_or(|existing| !is_downgrade(existing, &image));
                    if should_insert {
                        let _ = self.cache.insert(res.path.clone(), Arc::clone(&image));
                    }
                    events.push(StoreEvent::Ready {
                        path: res.path,
                        image,
                        full_resolution: res.full_resolution,
                        elapsed: res.elapsed,
                    });
                }
                Err(DecodeError::Cancelled) => {}
                Err(error) => {
                    events.push(StoreEvent::Failed {
                        path: res.path,
                        error,
                    });
                }
            }
        }
        events
    }

    /// Forget a path (file rotated/deleted/changed on disk).
    pub fn invalidate(&mut self, path: &Path) {
        let path_buf = path.to_path_buf();
        let _ = self.cache.remove(&path_buf);
        let entry = self.path_gen.entry(path_buf.clone()).or_insert(0);
        *entry = entry.wrapping_add(1);

        let (lock, _) = &*self.shared;
        let mut state = lock.lock().unwrap_or_else(|e| e.into_inner());
        state.pending.retain(|j| j.path != path_buf);
    }

    /// Target size changed significantly (e.g. moved to a 4K monitor / fullscreen). Cached images
    /// smaller than needed are re-requested on next focus.
    pub fn set_target(&mut self, target: (u32, u32)) {
        if target.0 == 0 || target.1 == 0 {
            return;
        }
        self.target = Some(target);
        let (lock, _) = &*self.shared;
        let mut state = lock.lock().unwrap_or_else(|e| e.into_inner());
        let wanted = JobKind::Fit(target.0, target.1);
        // Drop queued fit jobs that would now decode too small.
        state.pending.retain(|j| j.kind.satisfies(wanted));
    }

    /// Pause neighbor preloading and evict every cached image except `current` so Edit Mode
    /// adds ~0 MB over Viewer Mode. Returns bytes freed from the LRU cache.
    pub fn enter_edit_mode(&mut self, current: &Path) -> usize {
        let current_buf = current.to_path_buf();
        let (lock, _) = &*self.shared;
        {
            let mut state = lock.lock().unwrap_or_else(|e| e.into_inner());
            state.pending.retain(|j| j.path == current_buf);
        }
        self.cache.retain_only(Some(&current_buf))
    }

    /// Drop all non-active cached entries before full-resolution export so peak RAM stays bounded.
    pub fn trim_for_export(&mut self) -> usize {
        let (lock, _) = &*self.shared;
        {
            let mut state = lock.lock().unwrap_or_else(|e| e.into_inner());
            state.pending.clear();
        }
        self.cache.retain_only(None)
    }

    /// Current cache weight in bytes (for `--bench` reporting).
    pub fn cache_bytes(&self) -> usize {
        self.cache.total_weight()
    }
}

impl Drop for ImageStore {
    fn drop(&mut self) {
        let (lock, cvar) = &*self.shared;
        {
            let mut state = lock.lock().unwrap_or_else(|e| e.into_inner());
            state.shutdown = true;
            state.pending.clear();
        }
        // Wake all idle workers so they exit immediately; active workers notice `shutdown`
        // as soon as their current decode completes and never block process exit.
        cvar.notify_all();
    }
}

fn worker_loop(
    shared: Arc<(Mutex<QueueState>, Condvar)>,
    result_tx: Sender<JobResult>,
    notify: Arc<dyn Fn() + Send + Sync>,
    decode_fn: DecodeFn,
) {
    let (lock, cvar) = &*shared;
    loop {
        let job = {
            let mut state = lock.lock().unwrap_or_else(|e| e.into_inner());
            loop {
                if state.shutdown {
                    return;
                }
                if !state.pending.is_empty() {
                    // Pop highest-priority job (smallest `priority`, then oldest `seq`).
                    let best_idx = state
                        .pending
                        .iter()
                        .enumerate()
                        .min_by_key(|(_, j)| (j.priority, j.seq))
                        .map(|(i, _)| i)
                        .unwrap_or(0);
                    let job = state.pending.swap_remove(best_idx);
                    state.in_flight.push(InFlight {
                        path: job.path.clone(),
                        kind: job.kind,
                        generation: job.generation,
                    });
                    break job;
                }
                state = cvar.wait(state).unwrap_or_else(|e| e.into_inner());
            }
        };

        let start = Instant::now();
        let outcome = decode_fn(&job.path, job.kind.max_size()).map(Arc::new);
        let elapsed = start.elapsed();

        let is_shutdown = {
            let mut state = lock.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(pos) = state.in_flight.iter().position(|f| {
                f.path == job.path && f.kind == job.kind && f.generation == job.generation
            }) {
                state.in_flight.swap_remove(pos);
            }
            state.shutdown
        };

        if is_shutdown {
            return;
        }

        let res = JobResult {
            path: job.path,
            full_resolution: job.kind.is_full(),
            generation: job.generation,
            elapsed,
            outcome,
        };
        if result_tx.send(res).is_err() {
            return;
        }
        // Call UI notification callback outside all locks.
        notify();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Barrier;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use kalux_core::{Frame, ImageFormat};

    fn fake_image(src_w: u32, src_h: u32, max_size: Option<(u32, u32)>) -> DecodedImage {
        let (w, h) = match max_size {
            Some((mw, mh)) => fit_dimensions(src_w, src_h, mw, mh),
            None => (src_w, src_h),
        };
        DecodedImage {
            width: w,
            height: h,
            source_width: src_w,
            source_height: src_h,
            format: ImageFormat::Jpeg,
            frames: vec![Frame {
                pixels: vec![0u8; (w * h * 4) as usize],
                delay_ms: 0,
            }],
            loop_count: 0,
        }
    }

    fn wait_for_events(store: &mut ImageStore, expected: usize) -> Vec<StoreEvent> {
        let mut all = Vec::new();
        let deadline = Instant::now() + Duration::from_secs(3);
        while all.len() < expected && Instant::now() < deadline {
            all.extend(store.poll());
            if all.len() < expected {
                std::thread::sleep(Duration::from_millis(5));
            }
        }
        all
    }

    #[test]
    fn store_config_default_matches_contract() {
        let cfg = StoreConfig::default();
        assert!((1..=4).contains(&cfg.threads));
        assert_eq!(cfg.cache_budget, 128 * 1024 * 1024);
        assert_eq!(cfg.preload_radius, 2);
    }

    #[test]
    fn focus_caches_notifies_and_returns_hits_immediately() {
        let notify_count = Arc::new(AtomicUsize::new(0));
        let notify_cb = {
            let nc = Arc::clone(&notify_count);
            Arc::new(move || {
                nc.fetch_add(1, Ordering::SeqCst);
            })
        };
        let decode_calls = Arc::new(AtomicUsize::new(0));
        let decode_fn: DecodeFn = {
            let dc = Arc::clone(&decode_calls);
            Arc::new(move |_path, max_size| {
                dc.fetch_add(1, Ordering::SeqCst);
                Ok(fake_image(400, 200, max_size))
            })
        };

        let mut store = ImageStore::with_decoder(
            StoreConfig {
                threads: 2,
                cache_budget: 10 * 1024 * 1024,
                preload_radius: 2,
            },
            notify_cb,
            decode_fn,
        );

        let a = PathBuf::from("a.jpg");
        let b = PathBuf::from("b.jpg");
        assert!(
            store
                .focus(&a, std::slice::from_ref(&b), (200, 200))
                .is_none()
        );

        let events = wait_for_events(&mut store, 2);
        assert_eq!(events.len(), 2);
        assert!(notify_count.load(Ordering::SeqCst) >= 2);
        assert!(store.cache_bytes() > 0);

        // Second focus on `a` is an immediate cache hit and queues no new decode.
        let before_calls = decode_calls.load(Ordering::SeqCst);
        let hit = store.focus(&a, std::slice::from_ref(&b), (200, 200));
        assert!(hit.is_some());
        let img = hit.unwrap();
        assert_eq!((img.width, img.height), (200, 100));
        assert_eq!((img.source_width, img.source_height), (400, 200));
        std::thread::sleep(Duration::from_millis(30));
        assert_eq!(decode_calls.load(Ordering::SeqCst), before_calls);
        assert!(store.poll().is_empty());
    }

    #[test]
    fn focus_drops_stale_queued_jobs_and_prioritizes_current() {
        // Use 1 worker thread and block the first job with a barrier so we can inspect queueing.
        let gate_enter = Arc::new(Barrier::new(2));
        let gate_release = Arc::new(Barrier::new(2));
        let decoded_order = Arc::new(Mutex::new(Vec::<PathBuf>::new()));

        let decode_fn: DecodeFn = {
            let enter = Arc::clone(&gate_enter);
            let release = Arc::clone(&gate_release);
            let order = Arc::clone(&decoded_order);
            Arc::new(move |path, max_size| {
                if path == Path::new("block.jpg") {
                    enter.wait();
                    release.wait();
                }
                order.lock().unwrap().push(path.to_path_buf());
                Ok(fake_image(100, 100, max_size))
            })
        };

        let mut store = ImageStore::with_decoder(
            StoreConfig {
                threads: 1,
                cache_budget: 10 * 1024 * 1024,
                preload_radius: 2,
            },
            Arc::new(|| {}),
            decode_fn,
        );

        // Focus `block.jpg` with neighbors `stale1.jpg`, `stale2.jpg`.
        store.focus(
            Path::new("block.jpg"),
            &[PathBuf::from("stale1.jpg"), PathBuf::from("stale2.jpg")],
            (100, 100),
        );
        // Wait until worker is inside `block.jpg` (so stale1/stale2 are still queued).
        gate_enter.wait();

        // Now user jumps to `new_cur.jpg` with neighbor `new_nbr.jpg`: stale1/stale2 must be dropped!
        store.focus(
            Path::new("new_cur.jpg"),
            &[PathBuf::from("new_nbr.jpg")],
            (100, 100),
        );

        // Release `block.jpg`.
        gate_release.wait();

        // Expect 3 total completions: block (was already in-flight), new_cur, new_nbr.
        let events = wait_for_events(&mut store, 3);
        assert_eq!(events.len(), 3);

        let order = decoded_order.lock().unwrap().clone();
        assert_eq!(
            order,
            vec![
                PathBuf::from("block.jpg"),
                PathBuf::from("new_cur.jpg"),
                PathBuf::from("new_nbr.jpg"),
            ]
        );
    }

    #[test]
    fn deduplicates_in_flight_jobs() {
        let gate_enter = Arc::new(Barrier::new(2));
        let gate_release = Arc::new(Barrier::new(2));
        let calls = Arc::new(AtomicUsize::new(0));

        let decode_fn: DecodeFn = {
            let enter = Arc::clone(&gate_enter);
            let release = Arc::clone(&gate_release);
            let calls = Arc::clone(&calls);
            Arc::new(move |_path, max_size| {
                let n = calls.fetch_add(1, Ordering::SeqCst);
                if n == 0 {
                    enter.wait();
                    release.wait();
                }
                Ok(fake_image(100, 100, max_size))
            })
        };

        let mut store = ImageStore::with_decoder(
            StoreConfig {
                threads: 2,
                cache_budget: 10 * 1024 * 1024,
                preload_radius: 1,
            },
            Arc::new(|| {}),
            decode_fn,
        );

        store.focus(Path::new("same.jpg"), &[], (100, 100));
        gate_enter.wait();
        // Repeated focus while in-flight must NOT schedule a duplicate decode.
        store.focus(Path::new("same.jpg"), &[], (100, 100));
        gate_release.wait();

        let events = wait_for_events(&mut store, 1);
        assert_eq!(events.len(), 1);
        std::thread::sleep(Duration::from_millis(30));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn full_resolution_set_target_and_invalidate() {
        let decode_fn: DecodeFn = Arc::new(|_path, max_size| Ok(fake_image(1000, 500, max_size)));

        let mut store = ImageStore::with_decoder(
            StoreConfig {
                threads: 2,
                cache_budget: 10 * 1024 * 1024,
                preload_radius: 1,
            },
            Arc::new(|| {}),
            decode_fn,
        );

        let p = PathBuf::from("photo.jpg");

        // 1. Initial fit decode at 200x200 -> 200x100
        assert!(store.focus(&p, &[], (200, 200)).is_none());
        let evs = wait_for_events(&mut store, 1);
        match &evs[0] {
            StoreEvent::Ready {
                image,
                full_resolution,
                ..
            } => {
                assert!(!full_resolution);
                assert_eq!((image.width, image.height), (200, 100));
            }
            other => panic!("unexpected event: {other:?}"),
        }

        // 2. Grow target to 600x600: next focus re-requests larger size
        store.set_target((600, 600));
        assert!(store.focus(&p, &[], (600, 600)).is_none());
        let evs = wait_for_events(&mut store, 1);
        match &evs[0] {
            StoreEvent::Ready {
                image,
                full_resolution,
                ..
            } => {
                assert!(!full_resolution);
                assert_eq!((image.width, image.height), (600, 300));
            }
            other => panic!("unexpected event: {other:?}"),
        }

        // 3. Request full resolution -> 1000x500 with full_resolution == true
        store.request_full_resolution(&p);
        let evs = wait_for_events(&mut store, 1);
        match &evs[0] {
            StoreEvent::Ready {
                image,
                full_resolution,
                ..
            } => {
                assert!(*full_resolution);
                assert_eq!((image.width, image.height), (1000, 500));
            }
            other => panic!("unexpected event: {other:?}"),
        }

        // 4. Invalidate clears cache
        store.invalidate(&p);
        assert_eq!(store.cache_bytes(), 0);
        assert!(store.focus(&p, &[], (600, 600)).is_none());
        let evs = wait_for_events(&mut store, 1);
        assert_eq!(evs.len(), 1);
    }

    #[test]
    fn workers_exit_cleanly_on_drop() {
        let notify = Arc::new(|| {});
        let store = ImageStore::with_decoder(
            StoreConfig {
                threads: 2,
                cache_budget: 1024 * 1024,
                preload_radius: 1,
            },
            Arc::clone(&notify) as Arc<dyn Fn() + Send + Sync>,
            Arc::new(|_, _| Err(DecodeError::NotFound)),
        );
        // 1 in test + 2 in workers = 3 strong refs
        assert_eq!(Arc::strong_count(&notify), 3);
        drop(store);

        let deadline = Instant::now() + Duration::from_secs(2);
        while Arc::strong_count(&notify) > 1 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(Arc::strong_count(&notify), 1);
    }
}
